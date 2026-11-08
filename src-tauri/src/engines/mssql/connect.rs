//! The tiberius configuration from a `ConnectRequest`, opening clients, and a small pool of idle clients for the tree,
//! browsing and edits (each call takes its own client, so a slow page does not hold up the tree).

use std::{future::Future, sync::Mutex, time::Duration};

use tiberius::{AuthMethod, Client, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::{
    contract::{ConnectRequest, ServerInfo, TlsMode},
    error::{AppError, AppResult},
    lock::lock,
};

pub type TdsClient = Client<Compat<TcpStream>>;

/// How long a pooled client or a query session may take to open after the first connect.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// Idle clients kept for catalog, browse and edit calls.
const POOL_SIZE: usize = 4;

/// Encryption and certificate trust from `spec.tls`; without one, from the version-1 options (`encrypt`,
/// `trustServerCertificate`), which default to an encrypted connection with a verified certificate.
fn tls_settings(request: &ConnectRequest) -> (EncryptionLevel, bool, Option<String>) {
    let spec = &request.spec;
    match &spec.tls {
        Some(tls) => {
            let level = match tls.mode {
                TlsMode::Disable => EncryptionLevel::Off,
                TlsMode::Prefer => EncryptionLevel::On,
                TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => EncryptionLevel::Required,
            };
            let ca = tls.ca_file.clone().filter(|path| !path.is_empty() && !tls.accept_invalid_certs);
            (level, tls.accept_invalid_certs, ca)
        }
        None => {
            let flag = |key: &str, default: bool| spec.options.get(key).map_or(default, |value| value == "true");
            let level = if flag("encrypt", true) {
                EncryptionLevel::Required
            } else {
                EncryptionLevel::Off
            };
            (level, flag("trustServerCertificate", false), None)
        }
    }
}

pub fn build_config(request: &ConnectRequest) -> Config {
    let spec = &request.spec;
    let mut config = Config::new();
    config.host(&request.endpoint.host);
    config.port(request.endpoint.port);
    config.authentication(AuthMethod::sql_server(&spec.user, request.secrets.password_or_empty()));
    config.application_name("Fosta Studio");
    if let Some(database) = spec.database.as_deref().filter(|name| !name.is_empty()) {
        config.database(database);
    }
    let (level, trust_all, ca) = tls_settings(request);
    config.encryption(level);
    if trust_all {
        config.trust_cert();
    } else if let Some(ca) = ca {
        config.trust_cert_ca(ca);
    }
    config
}

/// Opens a client within `limit`.
pub async fn connect(config: &Config, limit: Duration) -> AppResult<TdsClient> {
    let open = async {
        let tcp = TcpStream::connect(config.get_addr()).await?;
        tcp.set_nodelay(true)?;
        Ok::<_, AppError>(Client::connect(config.clone(), tcp.compat_write()).await?)
    };
    match tokio::time::timeout(limit, open).await {
        Ok(result) => result,
        Err(_) => Err(AppError::coded(
            "TIMEOUT",
            format!(
                "Connecting to {} timed out after {} s. Is SQL Server reachable on that host and port (and any port-forward running)?",
                config.get_addr(),
                limit.as_secs()
            ),
        )
        .retryable(true)),
    }
}

/// Waits at most `seconds` (0 = no limit) for an app-built statement.
pub async fn within<T>(seconds: u32, future: impl Future<Output = AppResult<T>>) -> AppResult<T> {
    if seconds == 0 {
        return future.await;
    }
    match tokio::time::timeout(Duration::from_secs(u64::from(seconds)), future).await {
        Ok(result) => result,
        Err(_) => Err(AppError::coded(
            "TIMEOUT",
            format!("The statement did not finish within {seconds} s (the workspace's statement timeout)."),
        )),
    }
}

/// Idle clients for one connection. A call takes a client (or opens one) and gives it back when it ends cleanly; a
/// client whose call failed is dropped, since its socket may be dead or mid-response.
pub struct ClientPool {
    config: Config,
    idle: Mutex<Vec<TdsClient>>,
}

impl ClientPool {
    pub fn new(config: Config, first: TdsClient) -> Self {
        Self {
            config,
            idle: Mutex::new(vec![first]),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub async fn take(&self) -> AppResult<TdsClient> {
        let idle = lock(&self.idle).pop();
        match idle {
            Some(client) => Ok(client),
            None => connect(&self.config, OPEN_TIMEOUT).await,
        }
    }

    /// Gives a client back after a call: kept when the call succeeded or failed with an ordinary server error (the
    /// connection is fine then), dropped otherwise.
    pub fn release<T>(&self, client: TdsClient, outcome: &AppResult<T>) {
        if outcome.as_ref().err().is_some_and(|error| !is_statement_error(error)) {
            return;
        }
        let mut idle = lock(&self.idle);
        if idle.len() < POOL_SIZE {
            idle.push(client);
        }
    }

    pub fn clear(&self) {
        lock(&self.idle).clear();
    }
}

/// A server error that leaves the connection usable (severity below 20; 20 and above close it).
pub fn is_statement_error(error: &AppError) -> bool {
    error.code() == Some("ENGINE_ERROR")
        && error
            .field("severity")
            .and_then(|s| s.as_u64())
            .is_some_and(|severity| severity < 20)
}

/// `16.0.4135.4` plus the edition, as `SERVERPROPERTY` reports them.
pub fn server_info(version: &str, edition: &str) -> ServerInfo {
    let major = version.split('.').next().and_then(|part| part.parse::<u32>().ok());
    let release = match major {
        Some(17) => "2025",
        Some(16) => "2022",
        Some(15) => "2019",
        Some(14) => "2017",
        Some(13) => "2016",
        Some(12) => "2014",
        Some(11) => "2012",
        _ => "",
    };
    let label = if release.is_empty() {
        format!("SQL Server {version}")
    } else {
        format!("SQL Server {release} ({version})")
    };
    ServerInfo {
        product: "SQL Server".into(),
        version: if edition.is_empty() {
            version.to_owned()
        } else {
            format!("{version} ({edition})")
        },
        label,
        flavor: (!edition.is_empty()).then(|| edition.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::contract::{ConnectSecrets, ConnectionSpec, Endpoint, TlsConfig};

    fn request(tls: Option<TlsConfig>, options: &[(&str, &str)]) -> ConnectRequest {
        ConnectRequest {
            spec: ConnectionSpec {
                engine: "mssql".into(),
                user: "sa".into(),
                database: Some("ERP".into()),
                options: options
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<HashMap<_, _>>(),
                tls,
                ..ConnectionSpec::default()
            },
            secrets: ConnectSecrets::password("pw"),
            endpoint: Endpoint {
                host: "sql.local".into(),
                port: 1444,
            },
            connect_timeout: Duration::from_secs(1),
        }
    }

    fn tls(mode: TlsMode, accept_invalid_certs: bool) -> Option<TlsConfig> {
        Some(TlsConfig {
            mode,
            accept_invalid_certs,
            ..TlsConfig::default()
        })
    }

    #[test]
    fn tls_modes_map_to_encryption_levels() {
        let level = |request: ConnectRequest| tls_settings(&request);
        assert!(matches!(
            level(request(tls(TlsMode::Disable, false), &[])),
            (EncryptionLevel::Off, false, None)
        ));
        assert!(matches!(
            level(request(tls(TlsMode::Prefer, false), &[])),
            (EncryptionLevel::On, false, None)
        ));
        assert!(matches!(
            level(request(tls(TlsMode::Require, true), &[])),
            (EncryptionLevel::Required, true, None)
        ));
        assert!(matches!(
            level(request(tls(TlsMode::VerifyFull, false), &[])),
            (EncryptionLevel::Required, false, None)
        ));
        // Version-1 options when the profile has no TLS settings: encrypted unless switched off.
        assert!(matches!(level(request(None, &[])), (EncryptionLevel::Required, false, None)));
        assert!(matches!(
            level(request(None, &[("encrypt", "false"), ("trustServerCertificate", "true")])),
            (EncryptionLevel::Off, true, None)
        ));
        let mut with_ca = tls(TlsMode::VerifyCa, false).unwrap();
        with_ca.ca_file = Some("C:/ca.pem".into());
        assert_eq!(level(request(Some(with_ca), &[])).2.as_deref(), Some("C:/ca.pem"));
    }

    #[test]
    fn configs_use_the_endpoint() {
        let config = build_config(&request(tls(TlsMode::Require, true), &[]));
        assert_eq!(config.get_addr(), "sql.local:1444");
    }

    #[test]
    fn server_labels_name_the_release() {
        let info = server_info("16.0.4135.4", "Developer Edition (64-bit)");
        assert_eq!(info.label, "SQL Server 2022 (16.0.4135.4)");
        assert_eq!(info.version, "16.0.4135.4 (Developer Edition (64-bit))");
        assert_eq!(server_info("9.0.1", "").label, "SQL Server 9.0.1");
    }
}
