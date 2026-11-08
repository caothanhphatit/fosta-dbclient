//! Client options from a `ConnectRequest`, and the checks after the first request: the server must be the flavor the
//! profile names (an Elasticsearch profile on an OpenSearch cluster is refused, and the reverse) and a supported
//! version.
//!
//! - **Where**: through an SSH tunnel, the tunnel's local end (`request.endpoint`); else the node list in `spec.url`,
//!   the Cloud ID (`options.cloudId`, Elasticsearch only), or `host:port`.
//! - **TLS**: `https` when a URL says so or `spec.tls` requires TLS; then `spec.tls` goes through
//!   `net::tls::rustls_client_config` (CA file, client certificate, pinned SHA-256 fingerprint, accept invalid
//!   certificates). An `https` URL without TLS settings is verified against the system roots.
//! - **Auth**: basic (user + password; the OpenSearch security plugin uses it too), API key (Elasticsearch), bearer
//!   token, or none.

use std::time::Duration;

use reqwest::Url;

use super::{
    config::{EsAuth, EsConnectOptions},
    connection::{Distribution, EsEngine},
    descriptor::Flavor,
};
use crate::{
    contract::{AuthMethodId, ConnectRequest, SecretSlot, ServerInfo, TlsConfig, TlsMode},
    error::{AppError, AppResult},
    net::{self, spec::TlsSpec},
};

/// The oldest versions the adapter is tested against (point in time needs ES 7.10 / OpenSearch 2.4; older versions
/// page with from / size only).
const MIN_ELASTICSEARCH: (u32, u32) = (7, 10);
const MIN_OPENSEARCH: (u32, u32) = (1, 0);

fn requires_tls(tls: Option<&TlsConfig>) -> bool {
    tls.is_some_and(|tls| matches!(tls.mode, TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull))
}

/// The base URLs to use, and whether they are https.
pub fn base_urls(request: &ConnectRequest, flavor: Flavor) -> AppResult<(Vec<String>, bool)> {
    let spec = &request.spec;
    let tunnelled = spec.ssh.as_ref().is_some_and(|ssh| ssh.enabled);
    let wants_tls = requires_tls(spec.tls.as_ref());
    let configured: Vec<String> = spec
        .url
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| {
            if url.contains("://") {
                url.to_owned()
            } else {
                format!("http://{url}")
            }
        })
        .collect();
    if let Some(cloud_id) = spec.option("cloudId").filter(|_| flavor == Flavor::Elasticsearch && !tunnelled) {
        return Ok((vec![super::config::parse_cloud_id(cloud_id)?], true));
    }
    let https = wants_tls || configured.first().is_some_and(|url| url.starts_with("https://"));
    let scheme = if https { "https" } else { "http" };
    let host_port = |host: &str, port: u16| {
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        format!("{scheme}://{host}:{port}")
    };
    // A tunnel forwards one node: connect to its local end, keeping a proxy path prefix of the first URL.
    if tunnelled || configured.is_empty() {
        let prefix = configured
            .first()
            .and_then(|url| Url::parse(url).ok())
            .map(|url| url.path().trim_end_matches('/').to_owned())
            .unwrap_or_default();
        return Ok((
            vec![format!("{}{prefix}", host_port(&request.endpoint.host, request.endpoint.port))],
            https,
        ));
    }
    // TLS required by the profile upgrades plain node URLs.
    let urls = configured
        .into_iter()
        .map(|url| match url.strip_prefix("http://") {
            Some(rest) if https => format!("https://{rest}"),
            _ => url,
        })
        .collect();
    Ok((urls, https))
}

/// The rustls configuration for `https`: `spec.tls`, or verify-full against the system roots when it is not set.
fn tls_config(request: &ConnectRequest) -> AppResult<rustls::ClientConfig> {
    let mut tls = request.spec.tls.as_ref().map(TlsSpec::from).unwrap_or_default();
    if !tls.is_enabled() || tls.mode == net::spec::TlsMode::Prefer {
        // The URL is https, so TLS is used; verify it like `require`.
        tls.mode = net::spec::TlsMode::VerifyFull;
    }
    net::tls::rustls_client_config(&tls)
}

fn auth(request: &ConnectRequest, flavor: Flavor) -> AppResult<EsAuth> {
    let spec = &request.spec;
    let secret = |slot: SecretSlot| request.secrets.get(slot).map(str::to_owned).filter(|value| !value.is_empty());
    let method = spec.auth.unwrap_or(if spec.user.is_empty() {
        AuthMethodId::None
    } else {
        AuthMethodId::Password
    });
    Ok(match method {
        AuthMethodId::None => EsAuth::None,
        AuthMethodId::ApiKey if flavor == Flavor::Elasticsearch => EsAuth::ApiKey {
            id: None,
            key: None,
            encoded: Some(
                secret(SecretSlot::ApiKey)
                    .or_else(|| secret(SecretSlot::Password))
                    .ok_or_else(|| AppError::coded("PASSWORD_REQUIRED", "Enter the API key (the encoded value Kibana shows)."))?,
            ),
        },
        AuthMethodId::Bearer => EsAuth::Bearer {
            token: Some(
                secret(SecretSlot::Token)
                    .or_else(|| secret(SecretSlot::Password))
                    .ok_or_else(|| AppError::coded("PASSWORD_REQUIRED", "Enter the bearer token."))?,
            ),
        },
        AuthMethodId::Password => EsAuth::Basic {
            username: spec.user.clone(),
            password: secret(SecretSlot::Password),
        },
        other => {
            return Err(AppError::new(format!(
                "{} connections do not support the {other:?} sign-in method.",
                flavor.label()
            )))
        }
    })
}

/// Client options and the TLS configuration for a connect request.
pub fn options(request: &ConnectRequest, flavor: Flavor) -> AppResult<(EsConnectOptions, Option<rustls::ClientConfig>)> {
    let (urls, https) = base_urls(request, flavor)?;
    let tls = if https { Some(tls_config(request)?) } else { None };
    let options = EsConnectOptions {
        urls,
        cloud_id: None,
        auth: auth(request, flavor)?,
        request_timeout_ms: request.spec.option("requestTimeoutMs").and_then(|ms| ms.trim().parse().ok()),
        connect_timeout_ms: Some(request.connect_timeout.max(Duration::from_secs(1)).as_millis() as u64),
        ..EsConnectOptions::default()
    };
    Ok((options, tls))
}

/// Refuses a server of the other flavor or too old a version.
pub fn check_server(engine: &EsEngine, flavor: Flavor) -> AppResult<()> {
    let info = engine.info();
    match (flavor, info.distribution) {
        (Flavor::Elasticsearch, Distribution::OpenSearch) => {
            return Err(AppError::coded(
                "CONNECTION_ERROR",
                format!(
                    "This server is OpenSearch {}. Connect to it with the OpenSearch engine instead of Elasticsearch.",
                    info.version
                ),
            ))
        }
        (Flavor::OpenSearch, distribution) if distribution != Distribution::OpenSearch => {
            return Err(AppError::coded(
                "CONNECTION_ERROR",
                format!(
                    "This server is Elasticsearch {}. Connect to it with the Elasticsearch engine instead of OpenSearch.",
                    info.version
                ),
            ))
        }
        _ => {}
    }
    let minimum = match flavor {
        Flavor::Elasticsearch => MIN_ELASTICSEARCH,
        Flavor::OpenSearch => MIN_OPENSEARCH,
    };
    // Serverless reports its own API version; it is always current.
    if info.distribution != Distribution::ElasticsearchServerless && (info.major, info.minor) < minimum {
        return Err(AppError::coded(
            "CONNECTION_ERROR",
            format!(
                "{} {} is not supported; Fosta Studio needs {} {}.{} or newer.",
                flavor.label(),
                info.version,
                flavor.label(),
                minimum.0,
                minimum.1
            ),
        ));
    }
    Ok(())
}

/// Connects, then checks the flavor and version. Credential failures get a hint for the OpenSearch security plugin.
pub async fn connect(request: &ConnectRequest, flavor: Flavor) -> AppResult<EsEngine> {
    let (options, tls) = options(request, flavor)?;
    let engine = EsEngine::connect_with(&options, tls).await.map_err(|error| {
        let status = error.field("status").and_then(serde_json::Value::as_u64);
        match (flavor, status) {
            (Flavor::OpenSearch, Some(401)) => error.with(
                "hint",
                "With the security plugin enabled, OpenSearch needs a user and password (the demo configuration uses admin) and https.",
            ),
            _ => error,
        }
    })?;
    check_server(&engine, flavor)?;
    Ok(engine)
}

/// The status bar's server line.
pub fn server_info(engine: &EsEngine, flavor: Flavor) -> ServerInfo {
    let info = engine.info();
    let product = flavor.label().to_owned();
    ServerInfo {
        label: format!("{product} {}", info.version),
        product,
        version: info.version.clone(),
        flavor: match info.distribution {
            Distribution::ElasticsearchServerless => Some("serverless".into()),
            Distribution::OpenSearch => Some("opensearch".into()),
            _ => info.build_flavor.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::contract::{spec::SshAuth, ConnectSecrets, ConnectionSpec, Endpoint, SshConfig};

    fn request(spec: ConnectionSpec, secrets: ConnectSecrets) -> ConnectRequest {
        ConnectRequest {
            endpoint: Endpoint {
                host: spec.host.clone(),
                port: spec.port,
            },
            spec,
            secrets,
            connect_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn urls_come_from_the_node_list_host_or_tunnel() {
        let spec = ConnectionSpec {
            engine: "elasticsearch".into(),
            host: "localhost".into(),
            port: 9200,
            ..Default::default()
        };
        assert_eq!(
            base_urls(&request(spec.clone(), ConnectSecrets::default()), Flavor::Elasticsearch).unwrap(),
            (vec!["http://localhost:9200".to_owned()], false)
        );
        let tls = ConnectionSpec {
            tls: Some(TlsConfig {
                mode: TlsMode::VerifyFull,
                ..Default::default()
            }),
            url: Some("http://es1:9200, es2:9200".into()),
            ..spec.clone()
        };
        assert_eq!(
            base_urls(&request(tls.clone(), ConnectSecrets::default()), Flavor::Elasticsearch)
                .unwrap()
                .0,
            vec!["https://es1:9200".to_owned(), "https://es2:9200".to_owned()]
        );
        let mut tunnelled = request(
            ConnectionSpec {
                ssh: Some(SshConfig {
                    enabled: true,
                    host: "bastion".into(),
                    port: 22,
                    user: "u".into(),
                    auth: SshAuth::Agent,
                    jump_hosts: Vec::new(),
                    host_key: Default::default(),
                    keepalive_secs: None,
                }),
                url: Some("https://es1:9200/proxy/".into()),
                ..spec.clone()
            },
            ConnectSecrets::default(),
        );
        tunnelled.endpoint = Endpoint {
            host: "127.0.0.1".into(),
            port: 50123,
        };
        assert_eq!(
            base_urls(&tunnelled, Flavor::Elasticsearch).unwrap(),
            (vec!["https://127.0.0.1:50123/proxy".to_owned()], true)
        );
        let mut options = HashMap::new();
        options.insert("cloudId".to_owned(), "x:ZXUtd2VzdC0xLmF3cy5mb3VuZC5pbyRhYmMkZGVm".to_owned());
        let cloud = ConnectionSpec { options, ..spec };
        let (urls, https) = base_urls(&request(cloud.clone(), ConnectSecrets::default()), Flavor::Elasticsearch).unwrap();
        assert!(https && urls[0].starts_with("https://abc."), "{urls:?}");
        // OpenSearch has no Cloud ID: host and port win.
        assert_eq!(
            base_urls(&request(cloud, ConnectSecrets::default()), Flavor::OpenSearch).unwrap().0,
            vec!["http://localhost:9200".to_owned()]
        );
    }

    #[test]
    fn auth_methods_map_to_headers() {
        let spec = ConnectionSpec {
            engine: "elasticsearch".into(),
            host: "h".into(),
            port: 9200,
            user: "elastic".into(),
            ..Default::default()
        };
        let basic = auth(&request(spec.clone(), ConnectSecrets::password("pw")), Flavor::Elasticsearch).unwrap();
        assert!(
            matches!(basic, EsAuth::Basic { ref username, ref password } if username == "elastic" && password.as_deref() == Some("pw"))
        );
        let anonymous = ConnectionSpec {
            user: String::new(),
            ..spec.clone()
        };
        assert!(matches!(
            auth(&request(anonymous, ConnectSecrets::default()), Flavor::OpenSearch).unwrap(),
            EsAuth::None
        ));
        let api_key = ConnectionSpec {
            auth: Some(AuthMethodId::ApiKey),
            ..spec.clone()
        };
        let mut secrets = ConnectSecrets::default();
        secrets.values.insert(SecretSlot::ApiKey, "ZW5jb2RlZA==".into());
        assert!(matches!(
            auth(&request(api_key.clone(), secrets), Flavor::Elasticsearch).unwrap(),
            EsAuth::ApiKey { .. }
        ));
        assert_eq!(
            auth(&request(api_key.clone(), ConnectSecrets::default()), Flavor::Elasticsearch)
                .unwrap_err()
                .code(),
            Some("PASSWORD_REQUIRED")
        );
        assert!(auth(&request(api_key, ConnectSecrets::default()), Flavor::OpenSearch).is_err());
    }

    #[test]
    fn https_builds_a_rustls_config_and_bad_tls_settings_fail() {
        let spec = ConnectionSpec {
            engine: "elasticsearch".into(),
            host: "localhost".into(),
            port: 9200,
            tls: Some(TlsConfig {
                mode: TlsMode::Require,
                accept_invalid_certs: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let (options, tls) = options(&request(spec.clone(), ConnectSecrets::default()), Flavor::Elasticsearch).unwrap();
        assert_eq!(options.urls, vec!["https://localhost:9200".to_owned()]);
        assert!(tls.is_some());
        let pinned = ConnectionSpec {
            tls: Some(TlsConfig {
                mode: TlsMode::VerifyFull,
                pinned_sha256: Some("not hex".into()),
                ..Default::default()
            }),
            ..spec
        };
        assert!(options_err(&request(pinned, ConnectSecrets::default())));
    }

    fn options_err(request: &ConnectRequest) -> bool {
        options(request, Flavor::Elasticsearch).is_err()
    }
}
