//! Connection options from a [`ConnectRequest`]: the endpoint (already rewritten by the SSH manager), the password from the
//! secret store, the form's `options.*` and the TLS files. Redis's client has its own rustls glue, so the profile's
//! certificate files are read into PEM here instead of going through `net::tls::rustls_client_config`.

use std::fs;

use super::connection::{RedisConnectOptions, RedisTls, RedisTopology, TlsVerify};
use crate::{
    contract::{ConnectRequest, SecretSlot, TlsConfig, TlsMode},
    error::{AppError, AppResult},
};

/// The default `CLIENT SETNAME`.
pub const CLIENT_NAME: &str = "Fosta Studio";

fn read_pem(path: &Option<String>, what: &str) -> AppResult<Option<String>> {
    let Some(path) = path.as_deref().map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    fs::read_to_string(path).map(Some).map_err(|error| {
        AppError::coded(
            crate::net::tls::TLS_CONFIG,
            format!("Could not read the {what} file {path}: {error}"),
        )
    })
}

/// TLS settings, or `None` for a plain connection. Redis cannot negotiate TLS, so `prefer` (the profile default, not
/// offered by this engine's form) means plain; `require` encrypts without checking the certificate, `verifyCa` checks the
/// chain but not the host name, `verifyFull` checks both (so a tunnel needs `verifyCa`, the endpoint is 127.0.0.1).
pub fn tls_options(tls: &TlsConfig) -> AppResult<Option<RedisTls>> {
    let verify = match tls.mode {
        TlsMode::Disable | TlsMode::Prefer => return Ok(None),
        _ if tls.accept_invalid_certs => TlsVerify::None,
        TlsMode::Require => TlsVerify::None,
        TlsMode::VerifyCa => TlsVerify::SkipHostname,
        TlsMode::VerifyFull => TlsVerify::Full,
    };
    let client_cert_pem = read_pem(&tls.client_cert_file, "client certificate")?;
    let client_key_pem = read_pem(&tls.client_key_file, "client key")?;
    if client_cert_pem.is_some() != client_key_pem.is_some() {
        return Err(AppError::coded(
            crate::net::tls::TLS_CONFIG,
            "Mutual TLS needs both the client certificate and its key file.",
        ));
    }
    Ok(Some(RedisTls {
        verify,
        ca_pem: read_pem(&tls.ca_file, "CA certificate")?,
        client_cert_pem,
        client_key_pem,
    }))
}

pub fn options(request: &ConnectRequest) -> AppResult<RedisConnectOptions> {
    let spec = &request.spec;
    let number = |name: &str| -> AppResult<Option<u64>> {
        spec.option(name)
            .map(|text| {
                text.trim()
                    .parse()
                    .map_err(|_| AppError::new(format!("`{text}` is not a number (option {name}).")))
            })
            .transpose()
    };
    let db = match spec.option("db").or(spec.database.as_deref().filter(|name| !name.is_empty())) {
        Some(text) => text
            .trim()
            .trim_start_matches("db")
            .parse::<i64>()
            .ok()
            .filter(|db| *db >= 0)
            .ok_or_else(|| AppError::new(format!("The Redis database is a number such as 0 or 3, not `{text}`.")))?,
        None => 0,
    };
    Ok(RedisConnectOptions {
        host: request.endpoint.host.clone(),
        port: Some(request.endpoint.port),
        username: Some(spec.user.trim().to_owned()).filter(|user| !user.is_empty()),
        password: request
            .secrets
            .get(SecretSlot::Password)
            .map(str::to_owned)
            .filter(|password| !password.is_empty()),
        db,
        tls: spec.tls.as_ref().map(tls_options).transpose()?.flatten(),
        resp3: matches!(spec.option("resp3"), Some("true" | "1")),
        connect_timeout_ms: Some(request.connect_timeout.as_millis().min(u128::from(u64::MAX)) as u64).filter(|ms| *ms > 0),
        command_timeout_ms: number("commandTimeoutMs")?,
        client_name: Some(spec.option("clientName").unwrap_or(CLIENT_NAME).to_owned()),
        topology: RedisTopology::Standalone,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use super::*;
    use crate::contract::{ConnectSecrets, ConnectionSpec, Endpoint};

    fn request(spec: ConnectionSpec, password: &str) -> ConnectRequest {
        ConnectRequest {
            spec,
            secrets: ConnectSecrets::password(password),
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: 16379,
            },
            connect_timeout: Duration::from_secs(3),
        }
    }

    #[test]
    fn the_endpoint_wins_over_the_spec_host_and_options_are_read() {
        let mut spec = ConnectionSpec {
            engine: "redis".into(),
            host: "cache.internal".into(),
            port: 6379,
            user: "app".into(),
            options: HashMap::from([("db".to_owned(), "3".to_owned()), ("resp3".to_owned(), "true".to_owned())]),
            ..Default::default()
        };
        spec.options.insert("commandTimeoutMs".into(), "5000".into());
        let options = options(&request(spec, "s3cret")).unwrap();
        assert_eq!((options.host.as_str(), options.port), ("127.0.0.1", Some(16379)));
        assert_eq!(
            (options.username.as_deref(), options.password.as_deref()),
            (Some("app"), Some("s3cret"))
        );
        assert_eq!((options.db, options.resp3, options.command_timeout_ms), (3, true, Some(5000)));
        assert_eq!(options.connect_timeout_ms, Some(3000));
        assert_eq!(options.client_name.as_deref(), Some(CLIENT_NAME));
        assert!(options.tls.is_none());
        let bad = ConnectionSpec {
            options: HashMap::from([("db".to_owned(), "x".to_owned())]),
            ..Default::default()
        };
        assert!(super::options(&request(bad, "")).is_err());
    }

    #[test]
    fn tls_modes_map_to_verification() {
        let mode = |mode| {
            tls_options(&TlsConfig {
                mode,
                ..Default::default()
            })
            .unwrap()
        };
        assert!(mode(TlsMode::Disable).is_none() && mode(TlsMode::Prefer).is_none());
        assert_eq!(mode(TlsMode::Require).unwrap().verify, TlsVerify::None);
        assert_eq!(mode(TlsMode::VerifyCa).unwrap().verify, TlsVerify::SkipHostname);
        assert_eq!(mode(TlsMode::VerifyFull).unwrap().verify, TlsVerify::Full);
        let lax = TlsConfig {
            mode: TlsMode::VerifyFull,
            accept_invalid_certs: true,
            ..Default::default()
        };
        assert_eq!(tls_options(&lax).unwrap().unwrap().verify, TlsVerify::None);
        let missing = TlsConfig {
            mode: TlsMode::VerifyCa,
            ca_file: Some("/definitely/not/here.pem".into()),
            ..Default::default()
        };
        assert_eq!(tls_options(&missing).unwrap_err().code(), Some(crate::net::tls::TLS_CONFIG));
        let half = TlsConfig {
            mode: TlsMode::VerifyCa,
            client_cert_file: Some("cert.pem".into()),
            ..Default::default()
        };
        assert!(tls_options(&half).is_err());
    }
}
