//! Connection options from a [`ConnectRequest`]: the endpoint (already rewritten by the SSH manager), the password from the
//! secret store, the form's `options.*` and the TLS settings.
//!
//! TLS means HTTPS (8443 by default). The rustls configuration comes from `net::tls::rustls_client_config`, so the profile's
//! CA file, client certificate, pinned fingerprint and `accept_invalid_certs` work as in every other engine. Through an
//! SSH tunnel the URL keeps the database host (for certificate names) and the client connects to the tunnel's end.

use std::{net::SocketAddr, time::Duration};

use super::ChOptions;
use crate::{
    contract::{ConnectRequest, TlsMode as ContractTlsMode},
    engines::common::TlsMode,
    error::{AppError, AppResult},
    net,
};

pub struct Prepared {
    pub options: ChOptions,
    pub tls: Option<rustls::ClientConfig>,
}

pub fn prepare(request: &ConnectRequest) -> AppResult<Prepared> {
    let spec = &request.spec;
    let secure = spec
        .tls
        .as_ref()
        // Prefer would need a plain fallback the HTTP client does not have: it connects without TLS.
        .filter(|tls| !matches!(tls.mode, ContractTlsMode::Disable | ContractTlsMode::Prefer));
    let tls = match secure {
        Some(tls) => Some(net::tls::rustls_client_config(&net::TlsSpec::from(tls))?),
        None => None,
    };
    let connect_timeout = match spec.option("connectTimeout") {
        Some(text) => Duration::from_secs(
            text.trim()
                .parse()
                .map_err(|_| AppError::new(format!("`{text}` is not a number of seconds (option connectTimeout).")))?,
        ),
        None => request.connect_timeout,
    };
    let host = if spec.host.trim().is_empty() {
        "localhost"
    } else {
        spec.host.trim()
    };
    let tunnelled = request.endpoint.host != host || request.endpoint.port != spec.port;
    let connect_to = tunnelled
        .then(|| request.endpoint.host.parse::<std::net::IpAddr>().ok())
        .flatten()
        .map(|ip| SocketAddr::new(ip, request.endpoint.port));
    let (host, port) = if connect_to.is_some() {
        (host.to_owned(), request.endpoint.port)
    } else {
        (request.endpoint.host.clone(), request.endpoint.port)
    };
    let database = spec
        .database
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "default".to_owned());
    Ok(Prepared {
        options: ChOptions {
            host,
            port,
            user: if spec.user.trim().is_empty() {
                "default".to_owned()
            } else {
                spec.user.trim().to_owned()
            },
            password: Some(request.secrets.password_or_empty().to_owned()).filter(|password| !password.is_empty()),
            database,
            // The core only needs to know whether to speak https; the profile's TLS settings are in `tls`.
            tls: if tls.is_some() {
                TlsMode::Verify { ca_file: None }
            } else {
                TlsMode::Disable
            },
            connect_timeout,
            connect_to,
        },
        tls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ConnectSecrets, ConnectionSpec, Endpoint, TlsConfig};

    fn request(spec: ConnectionSpec, endpoint: (&str, u16)) -> ConnectRequest {
        ConnectRequest {
            spec,
            secrets: ConnectSecrets::password("pw"),
            endpoint: Endpoint {
                host: endpoint.0.into(),
                port: endpoint.1,
            },
            connect_timeout: Duration::from_secs(7),
        }
    }

    fn spec(tls: Option<TlsConfig>) -> ConnectionSpec {
        ConnectionSpec {
            engine: "clickhouse".into(),
            host: "ch.example.com".into(),
            port: 8123,
            user: "analyst".into(),
            tls,
            ..Default::default()
        }
    }

    #[test]
    fn a_plain_connection_uses_the_endpoint_and_the_profile_values() {
        let prepared = prepare(&request(spec(None), ("ch.example.com", 8123))).unwrap();
        assert_eq!(prepared.options.host, "ch.example.com");
        assert_eq!(prepared.options.user, "analyst");
        assert_eq!(prepared.options.password.as_deref(), Some("pw"));
        assert_eq!(prepared.options.database, "default");
        assert_eq!(prepared.options.connect_timeout, Duration::from_secs(7));
        assert!(prepared.tls.is_none() && prepared.options.connect_to.is_none());
        assert_eq!(prepared.options.tls, TlsMode::Disable);
    }

    #[test]
    fn prefer_is_plain_and_the_other_modes_build_a_rustls_config() {
        let tls = |mode| {
            Some(TlsConfig {
                mode,
                ..Default::default()
            })
        };
        assert!(prepare(&request(spec(tls(ContractTlsMode::Prefer)), ("ch.example.com", 8123)))
            .unwrap()
            .tls
            .is_none());
        let secure = prepare(&request(spec(tls(ContractTlsMode::VerifyFull)), ("ch.example.com", 8443))).unwrap();
        assert!(secure.tls.is_some());
        assert_ne!(secure.options.tls, TlsMode::Disable);
        let insecure = prepare(&request(
            spec(Some(TlsConfig {
                mode: ContractTlsMode::Require,
                accept_invalid_certs: true,
                ..Default::default()
            })),
            ("ch.example.com", 8443),
        ))
        .unwrap();
        assert!(insecure.tls.is_some());
    }

    #[test]
    fn a_tunnel_keeps_the_host_name_and_connects_to_its_end() {
        let prepared = prepare(&request(spec(None), ("127.0.0.1", 50123))).unwrap();
        assert_eq!(prepared.options.host, "ch.example.com");
        assert_eq!(prepared.options.port, 50123);
        assert_eq!(prepared.options.connect_to, Some("127.0.0.1:50123".parse().unwrap()));
    }
}
