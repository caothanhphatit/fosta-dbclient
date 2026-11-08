//! Connection options from a `ConnectRequest` (the endpoint the manager prepared, possibly an SSH tunnel's local port; the
//! password from the secrets; `spec.tls` as `tcps`) and Oracle Instant Client: it is loaded before the first connection,
//! from the profile's "Instant Client folder" when set, else from PATH / ORACLE_HOME or a copy Fosta Studio downloaded.
//!
//! Without a client, connecting fails with the code `ORACLE_CLIENT` and `kind` (`notInstalled`, `load`, …), the same shape
//! as the `oracle_client_*` commands use, plus `download` (what the pinned package for this platform would fetch, absent
//! when there is none), so the UI can offer the download or a folder and then retry.

use std::{path::Path, time::Duration};

use super::{
    descriptor,
    instant_client::{self, ClientError, ClientErrorKind},
    OraOptions,
};
use crate::{
    contract::{ConnectRequest, ServerInfo, TlsMode},
    engines::common::{EngineError, TlsMode as CoreTls},
    error::{AppError, AppResult},
    runtime::oracle_client::ORACLE_CLIENT,
};

/// The connection option holding an Instant Client folder (`options.clientDir` in the descriptor's form).
pub const CLIENT_DIR: &str = "clientDir";

pub fn build_options(request: &ConnectRequest) -> AppResult<OraOptions> {
    let spec = &request.spec;
    let defaults = OraOptions::default();
    let mut options = OraOptions {
        host: request.endpoint.host.clone(),
        port: request.endpoint.port,
        service: spec
            .database
            .clone()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or(defaults.service.clone()),
        user: spec.user.trim().to_owned(),
        password: Some(request.secrets.password_or_empty().to_owned()).filter(|password| !password.is_empty()),
        connect_timeout: request.connect_timeout,
        connect_string: spec.option("connectString").map(|text| text.trim().to_owned()),
        ..defaults
    };
    if options.user.is_empty() {
        options.user = "system".into();
    }
    if let Some(text) = spec.option("connectTimeout") {
        let seconds: u64 = text
            .trim()
            .parse()
            .map_err(|_| AppError::new(format!("The connect timeout '{text}' is not a number of seconds.")))?;
        options.connect_timeout = Duration::from_secs(seconds.max(1));
    }
    if let Some(tls) = &spec.tls {
        options.tls = match tls.mode {
            TlsMode::Disable => CoreTls::Disable,
            // Instant Client verifies tcps against its wallet; it cannot skip the check (the descriptor offers VerifyFull only).
            _ if tls.accept_invalid_certs => CoreTls::SkipVerify,
            _ => CoreTls::Verify { ca_file: None },
        };
    }
    Ok(options)
}

/// `Oracle 23.6.0.24.10` from the server version.
pub fn server_info(version: &str) -> ServerInfo {
    let major = version.split('.').next().unwrap_or(version);
    ServerInfo {
        product: descriptor::LABEL.to_owned(),
        version: version.to_owned(),
        label: format!("{} {version}", descriptor::LABEL),
        flavor: (major.parse::<u32>().is_ok_and(|n| n >= 23)).then(|| "23ai".to_owned()),
    }
}

/// The structured error for a missing or unusable client.
pub fn client_error(error: ClientError) -> AppError {
    let offer = instant_client::current_package().ok().map(|package| {
        serde_json::json!({
            "version": package.version,
            "size": package.size,
            "url": package.url,
            "licenseUrl": instant_client::LICENSE_URL,
        })
    });
    AppError::coded(ORACLE_CLIENT, error.message.clone())
        .with_engine(descriptor::ID)
        .with("kind", error.kind)
        .with("download", offer)
}

/// Loads Instant Client into ODPI-C (once per process; cheap afterwards). A configured folder without the client library is
/// an error of its own rather than silently skipped.
pub async fn ensure_client(configured: Option<&str>) -> AppResult<()> {
    let configured = configured.map(str::trim).filter(|dir| !dir.is_empty()).map(str::to_owned);
    if let Some(dir) = &configured {
        let lib = if cfg!(windows) {
            "oci.dll"
        } else if cfg!(target_os = "macos") {
            "libclntsh.dylib"
        } else {
            "libclntsh.so"
        };
        if !oracle::InitParams::is_initialized() && !Path::new(dir).join(lib).is_file() {
            return Err(client_error(ClientError::new(
                ClientErrorKind::NotInstalled,
                format!("The Instant Client folder {dir} has no {lib}. Choose the folder that contains it."),
            )));
        }
    }
    tokio::task::spawn_blocking(move || instant_client::initialize(configured.as_deref().map(Path::new)))
        .await
        .map_err(|error| AppError::new(format!("The worker thread failed: {error}")))?
        .map(|_| ())
        .map_err(client_error)
}

/// A connect failure as the contract's error: a client that could not be loaded keeps the `ORACLE_CLIENT` shape.
pub fn connect_error(error: EngineError) -> AppError {
    if error.message.starts_with("Oracle Instant Client was not found") {
        return client_error(ClientError::new(ClientErrorKind::NotInstalled, error.message));
    }
    AppError::from(error).with_engine(descriptor::ID)
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
                port: 15210,
            },
            connect_timeout: Duration::from_secs(7),
        }
    }

    #[test]
    fn options_come_from_the_prepared_endpoint_and_the_secrets() {
        let mut spec = ConnectionSpec {
            engine: "oracle".into(),
            host: "db.internal".into(),
            port: 1521,
            user: "shop".into(),
            database: Some(" ORCLPDB1 ".into()),
            ..Default::default()
        };
        spec.options.insert("connectTimeout".into(), "3".into());
        let options = build_options(&request(spec.clone(), "secret")).unwrap();
        assert_eq!((options.host.as_str(), options.port), ("127.0.0.1", 15210), "the tunnel's endpoint");
        assert_eq!(options.service, "ORCLPDB1");
        assert_eq!(options.password.as_deref(), Some("secret"));
        assert_eq!(options.connect_timeout, Duration::from_secs(3));
        assert_eq!(options.connect_string().unwrap(), "127.0.0.1:15210/ORCLPDB1?connect_timeout=3");
        spec.tls = Some(TlsConfig {
            mode: TlsMode::VerifyFull,
            ..Default::default()
        });
        spec.options.insert("connectString".into(), "PRODDB".into());
        let options = build_options(&request(spec, "")).unwrap();
        assert!(options.password.is_none());
        assert_eq!(options.tls, CoreTls::Verify { ca_file: None });
        assert_eq!(options.connect_string().unwrap(), "PRODDB");
        assert!(build_options(&request(
            ConnectionSpec {
                options: [("connectTimeout".to_owned(), "soon".to_owned())].into(),
                ..Default::default()
            },
            ""
        ))
        .is_err());
    }

    #[test]
    fn a_missing_client_is_a_structured_error_with_the_download_offer() {
        let error = connect_error(super::super::missing_client_error(
            "DPI-1047: Cannot locate a 64-bit Oracle Client library",
        ));
        assert_eq!(error.code(), Some("ORACLE_CLIENT"));
        assert_eq!(error.field("kind").and_then(|v| v.as_str()), Some("notInstalled"));
        assert_eq!(error.field("engine").and_then(|v| v.as_str()), Some("oracle"));
        if instant_client::current_package().is_ok() {
            assert!(error
                .field("download")
                .and_then(|d| d["size"].as_u64())
                .is_some_and(|size| size > 1_000_000));
        }
        let plain = connect_error(EngineError::connect("ORA-12541: TNS:no listener"));
        assert_eq!(plain.code(), Some("CONNECTION_ERROR"));
        assert_eq!(server_info("23.6.0.24.10").flavor.as_deref(), Some("23ai"));
        assert_eq!(server_info("19.3.0.0.0").label, "Oracle 19.3.0.0.0");
    }

    #[tokio::test]
    async fn a_configured_folder_without_the_client_library_is_refused() {
        if oracle::InitParams::is_initialized() {
            return;
        }
        let empty = std::env::temp_dir().join("fosta-oracle-no-client");
        std::fs::create_dir_all(&empty).unwrap();
        let error = ensure_client(Some(empty.to_str().unwrap())).await.unwrap_err();
        assert_eq!(error.code(), Some("ORACLE_CLIENT"));
        assert!(error.message().contains("has no"), "{error}");
    }
}
