//! Connection options from a `ConnectRequest`, and what the server is (MariaDB or MySQL, version, VECTOR support).

use std::{future::Future, time::Duration};

use mysql_async::{prelude::*, Conn, Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts};

use super::descriptor::Flavor;
use crate::{
    contract::{ConnectRequest, ServerInfo, TlsMode},
    error::{AppError, AppResult},
};

/// Browse, catalog and cancel requests share this many pooled connections.
const POOL_SIZE: usize = 4;
/// Grid saves run one at a time per connection anyway.
const EDIT_POOL_SIZE: usize = 2;
/// How long a pooled connection may take to open after the first connect.
pub const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// What the server is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub mariadb: bool,
    pub version: (u32, u32, u32),
    /// MariaDB 11.7+ has the VECTOR type and its functions.
    pub vector: bool,
}

impl Server {
    /// From `SELECT VERSION()`, e.g. `11.8.2-MariaDB-ubu2404` or `8.4.3`.
    pub fn from_version(text: &str) -> Self {
        let mariadb = text.to_ascii_lowercase().contains("mariadb");
        // MariaDB before 11 may prefix the version with `5.5.5-` for old clients.
        let text = text.strip_prefix("5.5.5-").unwrap_or(text);
        let mut parts = text
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<u32>().unwrap_or(0));
        let version = (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0));
        Self {
            mariadb,
            version,
            vector: mariadb && (version.0, version.1) >= (11, 7),
        }
    }

    pub fn info(&self, version: &str) -> ServerInfo {
        let product = if self.mariadb { "MariaDB" } else { "MySQL" };
        let (major, minor, patch) = self.version;
        ServerInfo {
            product: product.to_owned(),
            version: version.to_owned(),
            label: format!("{product} {major}.{minor}.{patch}"),
            flavor: None,
        }
    }
}

/// The options for query sessions and browsing; `edits` adds `CLIENT_FOUND_ROWS` so an UPDATE that writes the same
/// value still counts as matched (the concurrency check reads affected rows).
pub struct ConnectOptions {
    pub opts: Opts,
    pub edit_opts: Opts,
}

pub fn build_opts(flavor: Flavor, request: &ConnectRequest) -> AppResult<ConnectOptions> {
    if let Some(tls) = &request.spec.tls {
        if matches!(tls.mode, TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull) {
            // TODO(net::tls): map spec.tls onto mysql_async's rustls SslOpts once a TLS backend is compiled in.
            return Err(AppError::coded(
                "UNSUPPORTED_CAPABILITY",
                format!(
                    "TLS for {} connections arrives with the TLS work in a later version. Set TLS to Disable or Prefer for now.",
                    flavor.label()
                ),
            ));
        }
    }
    let spec = &request.spec;
    let builder = |pool_size: usize| {
        let constraints = PoolConstraints::new(0, pool_size).unwrap_or_default();
        OptsBuilder::default()
            .ip_or_hostname(request.endpoint.host.clone())
            .tcp_port(request.endpoint.port)
            .user(Some(spec.user.clone()))
            .pass(Some(request.secrets.password_or_empty().to_owned()))
            .db_name(spec.database.clone().filter(|name| !name.is_empty()))
            .pool_opts(PoolOpts::default().with_constraints(constraints))
    };
    Ok(ConnectOptions {
        opts: builder(POOL_SIZE).into(),
        edit_opts: builder(EDIT_POOL_SIZE).client_found_rows(true).into(),
    })
}

/// Waits at most `limit` for a connection step.
pub async fn within<T>(limit: Duration, what: &str, future: impl Future<Output = mysql_async::Result<T>>) -> AppResult<T> {
    match tokio::time::timeout(limit, future).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(AppError::coded(
            "TIMEOUT",
            format!(
                "{what} timed out after {} s. Is the server reachable (and any port-forward running)?",
                limit.as_secs()
            ),
        )
        .retryable(true)),
    }
}

/// Opens the pools and reads the server version.
pub async fn open(flavor: Flavor, request: &ConnectRequest) -> AppResult<(ConnectOptions, Pool, Pool, String)> {
    let options = build_opts(flavor, request)?;
    let pool = Pool::new(options.opts.clone());
    let edit_pool = Pool::new(options.edit_opts.clone());
    let what = format!("Connecting to {}:{}", request.endpoint.host, request.endpoint.port);
    let opened = async {
        let mut conn: Conn = within(request.connect_timeout, &what, pool.get_conn()).await?;
        let version: Option<String> = conn.query_first("SELECT VERSION()").await?;
        Ok::<_, AppError>(version.unwrap_or_default())
    }
    .await;
    match opened {
        Ok(version) => Ok((options, pool, edit_pool, version)),
        Err(error) => {
            let _ = pool.disconnect().await;
            let _ = edit_pool.disconnect().await;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::contract::{ConnectSecrets, ConnectionSpec, Endpoint, TlsConfig};

    fn request(tls: Option<TlsConfig>) -> ConnectRequest {
        ConnectRequest {
            spec: ConnectionSpec {
                engine: "mariadb".into(),
                user: "app".into(),
                database: Some("shop".into()),
                tls,
                ..ConnectionSpec::default()
            },
            secrets: ConnectSecrets::password("pw"),
            endpoint: Endpoint {
                host: "db.local".into(),
                port: 3307,
            },
            connect_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn versions_tell_mariadb_from_mysql_and_vector_support() {
        let maria = Server::from_version("11.8.2-MariaDB-ubu2404");
        assert!(maria.mariadb && maria.vector);
        assert_eq!(maria.version, (11, 8, 2));
        assert_eq!(maria.info("11.8.2-MariaDB-ubu2404").label, "MariaDB 11.8.2");
        let old = Server::from_version("5.5.5-10.11.6-MariaDB");
        assert!(old.mariadb && !old.vector);
        assert_eq!(old.version, (10, 11, 6));
        let mysql = Server::from_version("8.4.3");
        assert!(!mysql.mariadb && !mysql.vector);
        assert_eq!(mysql.info("8.4.3").label, "MySQL 8.4.3");
    }

    #[test]
    fn options_come_from_the_endpoint_and_edits_count_matched_rows() {
        let options = build_opts(Flavor::MariaDb, &request(None)).unwrap();
        assert_eq!(options.opts.ip_or_hostname(), "db.local");
        assert_eq!(options.opts.tcp_port(), 3307);
        assert_eq!(options.opts.user(), Some("app"));
        assert_eq!(options.opts.db_name(), Some("shop"));
        assert!(!options.opts.client_found_rows());
        assert!(options.edit_opts.client_found_rows());
    }

    #[test]
    fn required_tls_is_refused_until_a_tls_backend_exists() {
        let require = TlsConfig {
            mode: TlsMode::Require,
            ..TlsConfig::default()
        };
        let error = build_opts(Flavor::MySql, &request(Some(require))).err().expect("refused");
        assert!(error.to_string().contains("TLS for MySQL"), "{error}");
        let prefer = TlsConfig::default();
        assert!(build_opts(Flavor::MySql, &request(Some(prefer))).is_ok());
    }
}
