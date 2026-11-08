//! Connection options from a `ConnectRequest`: the endpoint the manager prepared (possibly an SSH tunnel's local port),
//! the password from the secrets and `spec.tls` through the shared rustls builder.

use std::time::Duration;

use super::{descriptor, PgOptions};
use crate::{
    contract::{ConnectRequest, ServerInfo, TlsMode},
    engines::common::TlsMode as CoreTls,
    error::{AppError, AppResult},
    net,
};

pub fn build_options(request: &ConnectRequest) -> AppResult<PgOptions> {
    let spec = &request.spec;
    let defaults = PgOptions::default();
    let mut options = PgOptions {
        host: request.endpoint.host.clone(),
        port: request.endpoint.port,
        user: if spec.user.trim().is_empty() {
            defaults.user.clone()
        } else {
            spec.user.clone()
        },
        password: Some(request.secrets.password_or_empty().to_owned()).filter(|password| !password.is_empty()),
        database: spec
            .database
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(defaults.database.clone()),
        connect_timeout: request.connect_timeout,
        application_name: spec.option("applicationName").unwrap_or(&defaults.application_name).to_owned(),
        ..defaults
    };
    if let Some(text) = spec.option("connectTimeout") {
        let seconds: u64 = text
            .trim()
            .parse()
            .map_err(|_| AppError::new(format!("The connect timeout '{text}' is not a number of seconds.")))?;
        options.connect_timeout = Duration::from_secs(seconds.max(1));
    }
    if let Some(tls) = &spec.tls {
        if tls.mode != TlsMode::Disable {
            let net_tls = net::TlsSpec::from(tls);
            // Fail with the structured TLS error (unreadable CA file, bad fingerprint) before connecting.
            net::tls::rustls_client_config(&net_tls)?;
            options.tls = CoreTls::Verify { ca_file: None };
            options.tls_spec = Some(net_tls);
        }
    }
    Ok(options)
}

/// `PostgreSQL 17.2` from `server_version` (`17.2 (Debian 17.2-1.pgdg120+1)`).
pub fn server_info(version: &str) -> ServerInfo {
    let short = version.split_whitespace().next().unwrap_or(version);
    ServerInfo {
        product: descriptor::LABEL.to_owned(),
        version: version.to_owned(),
        label: format!("{} {short}", descriptor::LABEL),
        flavor: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ConnectSecrets, ConnectionSpec, Endpoint, TlsConfig};

    fn request(spec: ConnectionSpec, password: &str) -> ConnectRequest {
        ConnectRequest {
            spec,
            secrets: ConnectSecrets::password(password),
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: 15432,
            },
            connect_timeout: Duration::from_secs(7),
        }
    }

    #[test]
    fn options_come_from_the_prepared_endpoint_and_the_secrets() {
        let mut spec = ConnectionSpec {
            engine: "postgres".into(),
            host: "db.internal".into(),
            port: 5432,
            user: "app".into(),
            database: Some("shop".into()),
            ..Default::default()
        };
        spec.options.insert("applicationName".into(), "etl".into());
        let options = build_options(&request(spec, "pw")).unwrap();
        assert_eq!((options.host.as_str(), options.port), ("127.0.0.1", 15432));
        assert_eq!((options.user.as_str(), options.database.as_str()), ("app", "shop"));
        assert_eq!(options.password.as_deref(), Some("pw"));
        assert_eq!(options.application_name, "etl");
        assert_eq!(options.connect_timeout, Duration::from_secs(7));
        assert!(options.tls_spec.is_none());
        let empty = build_options(&request(ConnectionSpec::default(), "")).unwrap();
        assert_eq!((empty.user.as_str(), empty.database.as_str()), ("postgres", "postgres"));
        assert!(empty.password.is_none());
    }

    #[test]
    fn tls_goes_through_the_shared_builder() {
        let mut spec = ConnectionSpec::default();
        spec.tls = Some(TlsConfig {
            mode: TlsMode::Require,
            accept_invalid_certs: true,
            ..Default::default()
        });
        let options = build_options(&request(spec.clone(), "")).unwrap();
        assert!(options.tls_spec.is_some());
        assert_ne!(options.tls, CoreTls::Disable);
        spec.tls = Some(TlsConfig {
            mode: TlsMode::VerifyFull,
            ca_file: Some("Z:/missing.pem".into()),
            ..Default::default()
        });
        assert!(build_options(&request(spec.clone(), "")).is_err());
        spec.tls = Some(TlsConfig {
            mode: TlsMode::Disable,
            ..Default::default()
        });
        assert!(build_options(&request(spec, "")).unwrap().tls_spec.is_none());
        let mut bad = ConnectionSpec::default();
        bad.options.insert("connectTimeout".into(), "soon".into());
        assert!(build_options(&request(bad, "")).is_err());
    }

    #[test]
    fn server_info_names_the_release() {
        let info = server_info("17.2 (Debian 17.2-1.pgdg120+1)");
        assert_eq!(info.label, "PostgreSQL 17.2");
        assert_eq!(info.product, "PostgreSQL");
    }
}
