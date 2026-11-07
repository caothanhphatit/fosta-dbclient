//! A small HTTP client for the Elasticsearch / OpenSearch REST API on top of `reqwest`: node failover, auth headers,
//! TLS options (CA, insecure, SHA-256 fingerprint pinning), per-request timeouts, a response size cap, and error
//! bodies turned into sentences.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use reqwest::{header, Method};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::{normalize_fingerprint, EsConnectOptions, DEFAULT_CONNECT_TIMEOUT_MS, DEFAULT_TIMEOUT_MS};
use crate::error::{AppError, AppResult};

/// Responses above this size are refused (a `_search` with a huge `size`, a `_cat` of a giant cluster).
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// A request body: one JSON value, newline-delimited JSON (`_bulk`, `_msearch`), or raw text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum EsBody {
    Json(Value),
    NdJson(Vec<Value>),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct EsRequest {
    pub method: Method,
    /// Path with an optional query string: `/products/_search?typed_keys=true`.
    pub path: String,
    pub body: Option<EsBody>,
    pub timeout: Option<Duration>,
}

impl EsRequest {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            body: None,
            timeout: None,
        }
    }

    pub fn json(mut self, body: Value) -> Self {
        self.body = Some(EsBody::Json(body));
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EsResponse {
    pub status: u16,
    /// Parsed JSON, or a JSON string holding the text (`_cat` without `format=json`, HEAD has none).
    pub body: Value,
    /// True when the body was JSON.
    pub is_json: bool,
    pub took_ms: u64,
    /// `Warning` headers (deprecations).
    pub warnings: Vec<String>,
}

#[derive(Clone)]
pub struct EsClient {
    http: reqwest::Client,
    urls: Arc<Vec<String>>,
    current: Arc<AtomicUsize>,
    authorization: Option<String>,
    timeout: Duration,
}

impl EsClient {
    pub fn new(options: &EsConnectOptions) -> AppResult<Self> {
        let urls = options.base_urls()?;
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(
                options.connect_timeout_ms.unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS),
            ))
            .user_agent(concat!("fosta-studio/", env!("CARGO_PKG_VERSION")));
        if let Some(fingerprint) = options.tls.ca_fingerprint.as_deref().filter(|text| !text.trim().is_empty()) {
            builder = builder.use_preconfigured_tls(pinned_tls_config(normalize_fingerprint(fingerprint)?)?);
        } else {
            if let Some(pem) = options.tls.ca_pem.as_deref().filter(|pem| !pem.trim().is_empty()) {
                for certificate in reqwest::Certificate::from_pem_bundle(pem.as_bytes())
                    .map_err(|error| AppError::new(format!("The CA certificate is not valid PEM: {error}")))?
                {
                    builder = builder.add_root_certificate(certificate);
                }
            }
            builder = builder
                .danger_accept_invalid_certs(options.tls.accept_invalid_certs)
                .danger_accept_invalid_hostnames(options.tls.accept_invalid_hostnames);
        }
        Ok(Self {
            http: builder.build()?,
            urls: Arc::new(urls),
            current: Arc::new(AtomicUsize::new(0)),
            authorization: options.authorization()?,
            timeout: Duration::from_millis(options.request_timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
        })
    }

    /// The node URL requests currently go to.
    pub fn base_url(&self) -> &str {
        &self.urls[self.current.load(Ordering::Relaxed) % self.urls.len()]
    }

    /// Sends a request and returns the response whatever its status (the Dev Tools console shows errors as bodies).
    /// Connection failures move on to the next node URL.
    pub async fn send(&self, request: &EsRequest) -> AppResult<EsResponse> {
        let started = Instant::now();
        let first = self.current.load(Ordering::Relaxed);
        let mut last_error = None;
        for attempt in 0..self.urls.len() {
            let index = (first + attempt) % self.urls.len();
            match self.send_to(&self.urls[index], request).await {
                Ok(response) => {
                    self.current.store(index, Ordering::Relaxed);
                    return read_response(response, started).await;
                }
                Err(error) if error.is_connect() && attempt + 1 < self.urls.len() => last_error = Some(error),
                Err(error) => return Err(transport_error(error, request.timeout.unwrap_or(self.timeout))),
            }
        }
        Err(last_error
            .map(|error| transport_error(error, self.timeout))
            .unwrap_or_else(|| AppError::new("No cluster URL is set.")))
    }

    async fn send_to(&self, base: &str, request: &EsRequest) -> reqwest::Result<reqwest::Response> {
        let url = format!("{base}/{}", request.path.trim_start_matches('/'));
        let mut builder = self
            .http
            .request(request.method.clone(), url)
            .timeout(request.timeout.unwrap_or(self.timeout));
        if let Some(authorization) = &self.authorization {
            builder = builder.header(header::AUTHORIZATION, authorization);
        }
        builder = match &request.body {
            None => builder,
            Some(EsBody::Json(value)) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(value).unwrap_or_default()),
            Some(EsBody::NdJson(lines)) => {
                let mut text = String::new();
                for line in lines {
                    text.push_str(&serde_json::to_string(line).unwrap_or_default());
                    text.push('\n');
                }
                builder.header(header::CONTENT_TYPE, "application/x-ndjson").body(text)
            }
            Some(EsBody::Text(text)) => builder.header(header::CONTENT_TYPE, "text/plain").body(text.clone()),
        };
        builder.send().await
    }

    /// Sends a request and fails on HTTP errors with the server's reason.
    pub async fn call(&self, request: &EsRequest) -> AppResult<Value> {
        let response = self.send(request).await?;
        if response.status >= 400 {
            return Err(AppError::new(error_message(response.status, &response.body)));
        }
        Ok(response.body)
    }

    pub async fn get(&self, path: &str) -> AppResult<Value> {
        self.call(&EsRequest::new(Method::GET, path)).await
    }

    pub async fn post(&self, path: &str, body: Value) -> AppResult<Value> {
        self.call(&EsRequest::new(Method::POST, path).json(body)).await
    }

    pub async fn put(&self, path: &str, body: Value) -> AppResult<Value> {
        self.call(&EsRequest::new(Method::PUT, path).json(body)).await
    }

    pub async fn delete(&self, path: &str) -> AppResult<Value> {
        self.call(&EsRequest::new(Method::DELETE, path)).await
    }
}

async fn read_response(mut response: reqwest::Response, started: Instant) -> AppResult<EsResponse> {
    let status = response.status().as_u16();
    let warnings = response
        .headers()
        .get_all(header::WARNING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_string)
        .collect();
    if response.content_length().is_some_and(|length| length as usize > MAX_RESPONSE_BYTES) {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    let took_ms = started.elapsed().as_millis() as u64;
    let (body, is_json) = match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => (value, true),
        Err(_) => (Value::String(String::from_utf8_lossy(&bytes).into_owned()), false),
    };
    Ok(EsResponse {
        status,
        body,
        is_json,
        took_ms,
        warnings,
    })
}

fn too_large() -> AppError {
    AppError::new(format!(
        "The response is larger than {} MB. Ask for fewer results (size, filter_path).",
        MAX_RESPONSE_BYTES / 1024 / 1024
    ))
}

fn transport_error(error: reqwest::Error, timeout: Duration) -> AppError {
    if error.is_timeout() {
        AppError::new(format!("The cluster did not answer within {} s.", timeout.as_secs_f32()))
    } else if error.is_connect() {
        // reqwest says only "client error (Connect)"; the cause (rustls "invalid peer certificate", "connection
        // refused") sits at the bottom of the source chain.
        let mut chain = Vec::new();
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            chain.push(cause.to_string());
            source = cause.source();
        }
        let detail = chain.last().cloned().unwrap_or_else(|| error.to_string());
        let lower = format!("{error} {}", chain.join(" ")).to_ascii_lowercase();
        if lower.contains("certificate") || lower.contains("tls") || lower.contains("ssl") {
            AppError::new(format!(
                "TLS handshake failed: {detail}. Check the CA certificate or fingerprint, or allow self-signed certificates."
            ))
        } else {
            AppError::new(format!(
                "Could not connect to the cluster: {detail}. Check the URL and that the node is running."
            ))
        }
    } else {
        AppError::new(error.to_string())
    }
}

/// `type: reason (caused by: ...)` from an Elasticsearch / OpenSearch error body, with hints for auth failures.
pub fn error_message(status: u16, body: &Value) -> String {
    let error = &body["error"];
    let mut message = match error {
        Value::String(text) => text.clone(),
        Value::Object(_) => {
            let kind = error["type"].as_str().unwrap_or("error");
            let reason = error["reason"].as_str().unwrap_or("");
            let mut text = if reason.is_empty() {
                kind.to_string()
            } else {
                format!("{kind}: {reason}")
            };
            let mut cause = &error["caused_by"];
            while let Some(reason) = cause["reason"].as_str() {
                text.push_str(&format!(" (caused by {}: {reason})", cause["type"].as_str().unwrap_or("error")));
                cause = &cause["caused_by"];
            }
            if let Some(failure) = error["failed_shards"][0]["reason"]["reason"].as_str() {
                if !text.contains(failure) {
                    text.push_str(&format!(" (shard failure: {failure})"));
                }
            }
            text
        }
        _ => match body {
            Value::String(text) if !text.is_empty() => text.chars().take(500).collect(),
            Value::Object(map) if map.contains_key("message") => map["message"].as_str().unwrap_or_default().to_string(),
            _ => String::new(),
        },
    };
    match status {
        401 => message = format!("The cluster rejected the credentials (401). {message}"),
        403 => message = format!("The user has no permission for this request (403). {message}"),
        404 if message.is_empty() => message = "Not found (404).".into(),
        _ if message.is_empty() => message = format!("HTTP {status}"),
        _ => {}
    }
    message.trim().to_string()
}

// ---- Fingerprint pinning (rustls) ----

/// Trusts a server whose chain contains a certificate with the given SHA-256 fingerprint, the way the official
/// Elasticsearch clients' `ca_fingerprint` works. Handshake signatures are still verified.
#[derive(Debug)]
struct FingerprintVerifier {
    fingerprint: Vec<u8>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let matches = std::iter::once(end_entity)
            .chain(intermediates)
            .any(|certificate| ring::digest::digest(&ring::digest::SHA256, certificate.as_ref()).as_ref() == self.fingerprint.as_slice());
        if matches {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "no certificate in the server's chain matches the pinned SHA-256 fingerprint".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn pinned_tls_config(fingerprint: Vec<u8>) -> AppResult<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(FingerprintVerifier {
        fingerprint,
        provider: provider.clone(),
    });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| AppError::new(format!("TLS setup failed: {error}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_error_bodies() {
        let body = json!({
            "error": {
                "type": "search_phase_execution_exception",
                "reason": "all shards failed",
                "caused_by": { "type": "illegal_argument_exception", "reason": "Text fields are not optimised" },
                "failed_shards": [{ "reason": { "type": "x", "reason": "Text fields are not optimised" } }]
            },
            "status": 400
        });
        assert_eq!(
            error_message(400, &body),
            "search_phase_execution_exception: all shards failed (caused by illegal_argument_exception: Text fields are not optimised)"
        );
        assert!(error_message(
            401,
            &json!({ "error": { "type": "security_exception", "reason": "unable to authenticate" } })
        )
        .starts_with("The cluster rejected"));
        assert_eq!(error_message(404, &Value::String(String::new())), "Not found (404).");
        assert_eq!(error_message(500, &Value::Null), "HTTP 500");
    }

    #[test]
    fn builds_clients_for_each_tls_mode() {
        let mut options = EsConnectOptions::single("https://localhost:9200");
        assert!(EsClient::new(&options).is_ok());
        options.tls.ca_fingerprint = Some("00".repeat(32));
        assert!(EsClient::new(&options).is_ok());
        options.tls.ca_fingerprint = Some("zz".into());
        assert!(EsClient::new(&options).is_err());
        options.tls.ca_fingerprint = None;
        options.tls.ca_pem = Some("not a pem".into());
        // reqwest accepts an empty bundle; garbage without PEM markers yields no certificates.
        let _ = EsClient::new(&options);
    }
}
