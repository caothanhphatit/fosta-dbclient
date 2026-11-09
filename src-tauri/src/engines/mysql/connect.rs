//! Connection options from a `ConnectRequest`, and what the server is (MariaDB or MySQL, version, VECTOR support).

use std::{
    future::Future,
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use mysql_async::{prelude::*, ClientIdentity, Conn, Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, SslOpts};

use super::descriptor::Flavor;
use crate::{
    contract::{ConnectRequest, ServerInfo, TlsConfig, TlsMode},
    error::{AppError, AppResult},
    net,
};

/// Browse and catalog requests share this many pooled connections.
pub const POOL_SIZE: usize = 4;
/// Grid saves run one at a time per connection anyway.
const EDIT_POOL_SIZE: usize = 2;
/// How long a pooled connection may take to open after the first connect.
pub const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// A connection from the pool. The count of those in use tells a busy pool from an unreachable server when no
/// connection comes within [`CHECKOUT_TIMEOUT`].
pub struct Pooled {
    /// `None` once [`Pooled::discard`] closed it.
    conn: Option<Conn>,
    in_use: Arc<AtomicUsize>,
}

impl Pooled {
    /// Closes the connection instead of returning it to the pool (for one that may be in a doubtful state).
    pub async fn discard(mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = tokio::time::timeout(Duration::from_secs(2), conn.disconnect()).await;
        }
    }
}

impl Deref for Pooled {
    type Target = Conn;
    fn deref(&self) -> &Conn {
        self.conn.as_ref().expect("a pooled connection is only taken by discard")
    }
}

impl DerefMut for Pooled {
    fn deref_mut(&mut self) -> &mut Conn {
        self.conn.as_mut().expect("a pooled connection is only taken by discard")
    }
}

impl Drop for Pooled {
    fn drop(&mut self) {
        self.in_use.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A connection from `pool` (of `size`), or an error saying whether the pool was busy or the server did not answer.
pub async fn checkout(pool: &Pool, in_use: &Arc<AtomicUsize>, size: usize) -> AppResult<Pooled> {
    match tokio::time::timeout(CHECKOUT_TIMEOUT, pool.get_conn()).await {
        Ok(conn) => {
            let conn = conn?;
            in_use.fetch_add(1, Ordering::Relaxed);
            Ok(Pooled {
                conn: Some(conn),
                in_use: Arc::clone(in_use),
            })
        }
        Err(_) => Err(checkout_timeout(in_use.load(Ordering::Relaxed), size)),
    }
}

fn checkout_timeout(in_use: usize, size: usize) -> AppError {
    let seconds = CHECKOUT_TIMEOUT.as_secs();
    let message = if in_use >= size {
        format!(
            "All {size} connections of this server are busy (pages, counts or tree reads still running), and none was free within {seconds} s. Wait for them, or stop the long-running read."
        )
    } else {
        format!(
            "Opening a connection timed out after {seconds} s ({in_use} of {size} in use). Is the server reachable (and any port-forward running)?"
        )
    };
    AppError::coded("TIMEOUT", message).retryable(true)
}

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

/// mysql_async's rustls settings for `spec.tls`. Disable and prefer connect without TLS (mysql_async has no opportunistic
/// TLS); require and verify-full verify the chain and the host name (`server_name` overrides the name, as through an SSH
/// tunnel), verify-ca skips the host name. mysql_async cannot pin a certificate, so a fingerprint is refused, not ignored.
fn ssl_opts(flavor: Flavor, tls: &TlsConfig) -> AppResult<Option<SslOpts>> {
    if matches!(tls.mode, TlsMode::Disable | TlsMode::Prefer) {
        return Ok(None);
    }
    let set = |value: &Option<String>| value.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned);
    if set(&tls.pinned_sha256).is_some() {
        return Err(AppError::coded(
            net::tls::TLS_CONFIG,
            format!(
                "Certificate pinning is not available for {} connections. Use a CA file, or accept invalid certificates for a test server.",
                flavor.label()
            ),
        ));
    }
    let mut ssl = SslOpts::default()
        .with_danger_accept_invalid_certs(tls.accept_invalid_certs)
        .with_danger_skip_domain_validation(tls.mode == TlsMode::VerifyCa)
        .with_danger_tls_hostname_override(set(&tls.server_name));
    if let Some(ca) = set(&tls.ca_file) {
        ssl = ssl.with_root_certs(vec![PathBuf::from(ca).into()]);
    }
    match (set(&tls.client_cert_file), set(&tls.client_key_file)) {
        (Some(cert), Some(key)) => {
            ssl = ssl.with_client_identity(Some(ClientIdentity::new(PathBuf::from(cert).into(), PathBuf::from(key).into())));
        }
        (None, None) => {}
        _ => {
            return Err(AppError::coded(
                net::tls::TLS_CONFIG,
                "A client certificate needs both its certificate file and its key file.",
            ))
        }
    }
    Ok(Some(ssl))
}

pub fn build_opts(flavor: Flavor, request: &ConnectRequest) -> AppResult<ConnectOptions> {
    let ssl = match &request.spec.tls {
        Some(tls) => ssl_opts(flavor, tls)?,
        None => None,
    };
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
            .ssl_opts(ssl.clone())
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

    #[test]
    fn a_busy_pool_is_not_blamed_on_the_network() {
        let busy = checkout_timeout(4, 4).to_string();
        assert!(busy.contains("All 4 connections"), "{busy}");
        assert!(!busy.contains("reachable"), "{busy}");
        let unreachable = checkout_timeout(1, 4).to_string();
        assert!(
            unreachable.contains("reachable") && unreachable.contains("1 of 4 in use"),
            "{unreachable}"
        );
    }

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
    fn tls_settings_map_onto_ssl_opts() {
        let tls = |mode: TlsMode| TlsConfig {
            mode,
            ..TlsConfig::default()
        };
        // Disable and prefer connect in plain text (mysql_async has no opportunistic TLS).
        for mode in [TlsMode::Disable, TlsMode::Prefer] {
            assert!(build_opts(Flavor::MySql, &request(Some(tls(mode))))
                .unwrap()
                .opts
                .ssl_opts()
                .is_none());
        }
        let verify_full = build_opts(Flavor::MySql, &request(Some(tls(TlsMode::VerifyFull)))).unwrap();
        let ssl = verify_full.opts.ssl_opts().expect("TLS on");
        assert!(!ssl.accept_invalid_certs() && !ssl.skip_domain_validation());
        assert!(verify_full.edit_opts.ssl_opts().is_some(), "the edit pool uses TLS too");
        let through_tunnel = TlsConfig {
            mode: TlsMode::VerifyCa,
            ca_file: Some("C:/ca.pem".into()),
            server_name: Some("db.example.com".into()),
            accept_invalid_certs: true,
            ..TlsConfig::default()
        };
        let options = build_opts(Flavor::MariaDb, &request(Some(through_tunnel))).unwrap();
        let ssl = options.opts.ssl_opts().unwrap();
        assert!(ssl.accept_invalid_certs() && ssl.skip_domain_validation());
        assert_eq!(ssl.tls_hostname_override(), Some("db.example.com"));
        assert_eq!(ssl.root_certs().len(), 1);
        // Pinning and a half client identity are refused, not ignored.
        let pinned = TlsConfig {
            pinned_sha256: Some("ab:cd".into()),
            ..tls(TlsMode::Require)
        };
        let error = build_opts(Flavor::MySql, &request(Some(pinned))).err().expect("refused");
        assert_eq!(error.code(), Some(net::tls::TLS_CONFIG));
        assert!(error.to_string().contains("MySQL"), "{error}");
        let half = TlsConfig {
            client_cert_file: Some("client.pem".into()),
            ..tls(TlsMode::Require)
        };
        assert!(build_opts(Flavor::MySql, &request(Some(half))).is_err());
    }
}
