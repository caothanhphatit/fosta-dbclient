//! Connection options: node URLs or an Elastic Cloud ID, authentication, TLS.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

pub const DEFAULT_URL: &str = "http://127.0.0.1:9200";
pub(crate) const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub(crate) const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;

/// How to reach the cluster. Secrets (password, API key, token) come from the secret store and are never serialised.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EsConnectOptions {
    /// One or more node URLs (`https://es1:9200`). Requests go to the first that answers; later ones are failover.
    pub urls: Vec<String>,
    /// Elastic Cloud ID (`name:base64(host$es-uuid$kibana-uuid)`); replaces `urls` when set.
    pub cloud_id: Option<String>,
    pub auth: EsAuth,
    pub tls: EsTls,
    /// Default request timeout; the console and long admin calls pass their own.
    pub request_timeout_ms: Option<u64>,
    pub connect_timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum EsAuth {
    #[default]
    None,
    #[serde(rename_all = "camelCase")]
    Basic {
        username: String,
        #[serde(skip_serializing, default)]
        password: Option<String>,
    },
    /// An API key: either the `encoded` value Kibana shows, or `id` + `key`.
    #[serde(rename_all = "camelCase")]
    ApiKey {
        id: Option<String>,
        #[serde(skip_serializing, default)]
        key: Option<String>,
        #[serde(skip_serializing, default)]
        encoded: Option<String>,
    },
    /// `Authorization: Bearer <token>` (service account tokens, OAuth / OIDC access tokens, AWS-signed proxies).
    #[serde(rename_all = "camelCase")]
    Bearer {
        #[serde(skip_serializing, default)]
        token: Option<String>,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EsTls {
    /// Accept any certificate (self-signed test clusters). Ignored when a fingerprint is pinned.
    pub accept_invalid_certs: bool,
    /// Accept a valid certificate issued for another host name.
    pub accept_invalid_hostnames: bool,
    /// PEM CA certificate to trust in addition to the system store (`config/certs/http_ca.crt` of a new cluster).
    pub ca_pem: Option<String>,
    /// SHA-256 fingerprint (hex, `:` separators allowed) of a certificate in the server's chain: the CA fingerprint
    /// Elasticsearch prints on first start, or the server certificate itself. The chain is then trusted only if it
    /// contains that certificate.
    pub ca_fingerprint: Option<String>,
}

impl EsConnectOptions {
    pub fn single(url: &str) -> Self {
        Self {
            urls: vec![url.to_string()],
            ..Default::default()
        }
    }

    /// Base URLs without a trailing slash, from the Cloud ID or the URL list.
    pub fn base_urls(&self) -> AppResult<Vec<String>> {
        if let Some(cloud_id) = self.cloud_id.as_deref().filter(|id| !id.trim().is_empty()) {
            return Ok(vec![parse_cloud_id(cloud_id)?]);
        }
        let urls: Vec<String> = self
            .urls
            .iter()
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .filter(|url| !url.is_empty())
            .map(|url| if url.contains("://") { url } else { format!("http://{url}") })
            .collect();
        if urls.is_empty() {
            return Err(AppError::new(
                "Enter the cluster URL (for example http://localhost:9200) or an Elastic Cloud ID.",
            ));
        }
        for url in &urls {
            let parsed = reqwest::Url::parse(url).map_err(|error| AppError::new(format!("`{url}` is not a valid URL: {error}")))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(AppError::new(format!(
                    "Cluster URLs start with http:// or https://, not {}://.",
                    parsed.scheme()
                )));
            }
        }
        Ok(urls)
    }

    pub(crate) fn authorization(&self) -> AppResult<Option<String>> {
        Ok(match &self.auth {
            EsAuth::None => None,
            EsAuth::Basic { username, password } => Some(format!(
                "Basic {}",
                STANDARD.encode(format!("{username}:{}", password.as_deref().unwrap_or("")))
            )),
            EsAuth::ApiKey { id, key, encoded } => match (encoded.as_deref().filter(|e| !e.is_empty()), id, key) {
                (Some(encoded), _, _) => Some(format!("ApiKey {encoded}")),
                (None, Some(id), Some(key)) => Some(format!("ApiKey {}", STANDARD.encode(format!("{id}:{key}")))),
                _ => return Err(AppError::new("Enter the API key (the encoded value, or its id and key).")),
            },
            EsAuth::Bearer { token } => match token.as_deref().filter(|token| !token.is_empty()) {
                Some(token) => Some(format!("Bearer {token}")),
                None => return Err(AppError::new("Enter the bearer token.")),
            },
        })
    }
}

/// Decodes an Elastic Cloud ID into the Elasticsearch endpoint: `name:base64("host[:port]$es_uuid$kibana_uuid")` becomes
/// `https://es_uuid.host[:port]` (port 443 by default).
pub fn parse_cloud_id(cloud_id: &str) -> AppResult<String> {
    let invalid = || AppError::new("This Elastic Cloud ID is not valid. Copy it again from the deployment page.");
    let encoded = cloud_id
        .trim()
        .rsplit_once(':')
        .map(|(_, encoded)| encoded)
        .unwrap_or(cloud_id.trim());
    let decoded = STANDARD.decode(encoded).map_err(|_| invalid())?;
    let decoded = String::from_utf8(decoded).map_err(|_| invalid())?;
    let mut parts = decoded.split('$');
    let host = parts.next().filter(|host| !host.is_empty()).ok_or_else(invalid)?;
    let es_uuid = parts.next().filter(|uuid| !uuid.is_empty()).ok_or_else(invalid)?;
    let (host, port) = match host.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => (host, port),
        _ => (host, "443"),
    };
    Ok(if port == "443" {
        format!("https://{es_uuid}.{host}")
    } else {
        format!("https://{es_uuid}.{host}:{port}")
    })
}

/// Normalises a fingerprint to lower-case hex without separators.
pub(crate) fn normalize_fingerprint(text: &str) -> AppResult<Vec<u8>> {
    let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 64 || text.chars().any(|c| !c.is_ascii_hexdigit() && c != ':' && !c.is_whitespace()) {
        return Err(AppError::new(
            "The certificate fingerprint must be a SHA-256 hash: 64 hex digits, optionally with `:` separators.",
        ));
    }
    Ok((0..32)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_cloud_ids() {
        // "us-east-1.aws.found.io$abc123$def456"
        let id = format!("my-deployment:{}", STANDARD.encode("us-east-1.aws.found.io$abc123$def456"));
        assert_eq!(parse_cloud_id(&id).unwrap(), "https://abc123.us-east-1.aws.found.io");
        let with_port = format!("x:{}", STANDARD.encode("eu.example.io:9243$es$kb"));
        assert_eq!(parse_cloud_id(&with_port).unwrap(), "https://es.eu.example.io:9243");
        assert!(parse_cloud_id("name:not-base64!").is_err());
    }

    #[test]
    fn builds_urls_and_auth_headers() {
        let options = EsConnectOptions {
            urls: vec!["localhost:9200/".into(), " https://b:9200 ".into()],
            ..Default::default()
        };
        assert_eq!(options.base_urls().unwrap(), vec!["http://localhost:9200", "https://b:9200"]);
        assert!(EsConnectOptions::default().base_urls().is_err());
        let basic = EsConnectOptions {
            auth: EsAuth::Basic {
                username: "elastic".into(),
                password: Some("pw".into()),
            },
            ..Default::default()
        };
        assert_eq!(basic.authorization().unwrap().unwrap(), "Basic ZWxhc3RpYzpwdw==");
        let key = EsConnectOptions {
            auth: EsAuth::ApiKey {
                id: Some("id".into()),
                key: Some("k".into()),
                encoded: None,
            },
            ..Default::default()
        };
        assert_eq!(key.authorization().unwrap().unwrap(), format!("ApiKey {}", STANDARD.encode("id:k")));
        // Secrets never serialise.
        let json = serde_json::to_string(&basic).unwrap();
        assert!(!json.contains("pw"), "{json}");
    }

    #[test]
    fn parses_fingerprints() {
        let hex = "AB:".repeat(31) + "AB";
        assert_eq!(normalize_fingerprint(&hex).unwrap(), vec![0xab; 32]);
        assert!(normalize_fingerprint("abcd").is_err());
    }
}
