//! PostgreSQL engine (tokio-postgres, TLS through rustls with the ring provider).
//!
//! The engine keeps one connection for catalog, browse and edits; each session opens its own connection so `BEGIN … COMMIT`
//! spans runs and a cancel request (PostgreSQL's out-of-band CancelRequest) only hits that session. Another database of the same
//! server is reached with [`PgEngine::connect_database`], since a PostgreSQL connection is bound to one database.

pub mod adapter;
mod catalog;
pub mod connect;
#[cfg(test)]
mod contract_tests;
pub mod descriptor;
mod explain;
mod graph;
mod nodes;
#[cfg(test)]
mod perf_tests;
mod run;
pub mod session;
mod tabular;
#[cfg(test)]
mod tests;
pub mod types;
mod vector;

pub use adapter::{PostgresAdapter, POSTGRES};
pub use session::PgSession;

use crate::engines::common::split::Dialect;
use crate::engines::common::sqlbuild::{self, ParamTarget, SqlDialect};
use crate::engines::common::value::percent_decode;
use crate::engines::common::{
    classify, BrowseRequest, ChangeSummary, Classification, ColumnInfo, EngineError, ErrorKind, ObjectRef, Page, Result, RowChange, TlsMode,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_postgres::config::SslMode;
use tokio_postgres::types::ToSql;
use tokio_postgres::Client;
use tokio_postgres_rustls::MakeRustlsConnect;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: String,
    pub tls: TlsMode,
    pub connect_timeout: Duration,
    pub application_name: String,
    /// TLS settings of the connection form; when set (and enabled) they replace `tls`'s trust rules (CA file, name
    /// override, pinning, client certificate) through `net::tls::rustls_client_config`.
    pub tls_spec: Option<crate::net::TlsSpec>,
}

impl Default for PgOptions {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 5432,
            user: "postgres".into(),
            password: None,
            database: "postgres".into(),
            tls: TlsMode::Disable,
            connect_timeout: Duration::from_secs(10),
            application_name: "Fosta Studio".into(),
            tls_spec: None,
        }
    }
}

impl PgOptions {
    /// Parses a `postgres://user:pass@host:port/db?sslmode=…` URL or a libpq key/value string (`host=… port=… dbname=…`).
    /// `sslmode`: `disable`/`allow`/`prefer` → no TLS, `require` → TLS without verification (libpq's meaning), `verify-ca` and
    /// `verify-full` → verified TLS; `sslrootcert` adds a CA file. The password is returned inside the options; callers move it to
    /// the secret store.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let mut o = Self::default();
        let mut params: Vec<(String, String)> = Vec::new();
        if let Some(rest) = s.strip_prefix("postgresql://").or_else(|| s.strip_prefix("postgres://")) {
            let (main, query) = rest.split_once('?').unwrap_or((rest, ""));
            let (auth_host, db) = main.split_once('/').unwrap_or((main, ""));
            let (auth, hostport) = match auth_host.rsplit_once('@') {
                Some((a, h)) => (Some(a), h),
                None => (None, auth_host),
            };
            if let Some(a) = auth {
                let (u, p) = a.split_once(':').map(|(u, p)| (u, Some(p))).unwrap_or((a, None));
                if !u.is_empty() {
                    o.user = percent_decode(u);
                }
                o.password = p.map(percent_decode);
            }
            let (host, port) = split_host_port(hostport)?;
            if !host.is_empty() {
                o.host = percent_decode(&host);
            }
            if let Some(p) = port {
                o.port = p;
            }
            if !db.is_empty() {
                o.database = percent_decode(db);
            }
            for pair in query.split('&').filter(|p| !p.is_empty()) {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                params.push((k.to_owned(), percent_decode(v)));
            }
        } else {
            params = parse_key_values(s)?;
        }
        let mut sslmode = "disable".to_owned();
        let mut root: Option<String> = None;
        for (k, v) in params {
            match k.as_str() {
                "host" | "hostaddr" => o.host = v,
                "port" => o.port = v.parse().map_err(|_| EngineError::input(format!("'{v}' is not a valid port.")))?,
                "user" => o.user = v,
                "password" => o.password = Some(v),
                "dbname" => o.database = v,
                "sslmode" => sslmode = v,
                "sslrootcert" => root = Some(v),
                "connect_timeout" => {
                    let secs: u64 = v
                        .parse()
                        .map_err(|_| EngineError::input("connect_timeout must be a number of seconds."))?;
                    o.connect_timeout = Duration::from_secs(secs.max(1));
                }
                "application_name" => o.application_name = v,
                other => return Err(EngineError::input(format!("Unknown PostgreSQL connection option '{other}'."))),
            }
        }
        o.tls = match sslmode.as_str() {
            "disable" | "allow" | "prefer" => TlsMode::Disable,
            "require" if root.is_none() => TlsMode::SkipVerify,
            "require" | "verify-ca" | "verify-full" => TlsMode::Verify { ca_file: root },
            other => return Err(EngineError::input(format!("Unknown sslmode '{other}'."))),
        };
        Ok(o)
    }

    fn config(&self) -> tokio_postgres::Config {
        let mut c = tokio_postgres::Config::new();
        // Through a tunnel the connect host is a local IP: the TLS server name (set by the manager to the database host)
        // becomes the host, so it is also the name sent as SNI, and the IP stays the address connected to.
        let name = self
            .tls_spec
            .as_ref()
            .filter(|spec| spec.is_enabled())
            .and_then(|spec| spec.server_name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty());
        match (name, self.host.parse::<std::net::IpAddr>()) {
            (Some(name), Ok(address)) => {
                c.host(name).hostaddr(address);
            }
            _ => {
                c.host(&self.host);
            }
        }
        c.port(self.port)
            .user(&self.user)
            .dbname(&self.database)
            .application_name(&self.application_name)
            .connect_timeout(self.connect_timeout)
            .ssl_mode(if self.tls == TlsMode::Disable {
                SslMode::Disable
            } else {
                SslMode::Require
            });
        if let Some(p) = &self.password {
            c.password(p);
        }
        c
    }
}

fn split_host_port(s: &str) -> Result<(String, Option<u16>)> {
    // [::1]:5432, host:5432, host
    if let Some(rest) = s.strip_prefix('[') {
        let (h, after) = rest
            .split_once(']')
            .ok_or_else(|| EngineError::input("The IPv6 address is missing its closing ']'."))?;
        let port = after
            .strip_prefix(':')
            .map(|p| p.parse::<u16>())
            .transpose()
            .map_err(|_| EngineError::input("The port is not valid."))?;
        return Ok((h.to_owned(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) => Ok((
            h.to_owned(),
            Some(p.parse().map_err(|_| EngineError::input(format!("'{p}' is not a valid port.")))?),
        )),
        None => Ok((s.to_owned(), None)),
    }
}

/// libpq `key=value` pairs; values may be single-quoted with `\'` and `\\` escapes.
fn parse_key_values(s: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            chars.next();
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.next() != Some('=') {
            return Err(EngineError::input(format!("Expected '=' after '{key}' in the connection string.")));
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut val = String::new();
        if chars.peek() == Some(&'\'') {
            chars.next();
            loop {
                match chars.next() {
                    Some('\\') => val.extend(chars.next()),
                    Some('\'') => break,
                    Some(c) => val.push(c),
                    None => return Err(EngineError::input("A quoted value in the connection string is not closed.")),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                val.push(c);
                chars.next();
            }
        }
        out.push((key, val));
    }
    Ok(out)
}

/// Accepts any server certificate: `TlsMode::SkipVerify` (self-signed test servers, libpq's `sslmode=require`).
#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

pub(crate) fn tls_connector(opts: &PgOptions) -> Result<MakeRustlsConnect> {
    if let Some(spec) = opts.tls_spec.as_ref().filter(|spec| spec.is_enabled()) {
        let config = crate::net::tls::rustls_client_config(spec).map_err(|error| EngineError::connect(error.to_string()))?;
        return Ok(MakeRustlsConnect::new(config));
    }
    let mode = &opts.tls;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| EngineError::internal(format!("TLS setup failed: {e}")))?;
    let config = match mode {
        TlsMode::SkipVerify => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
            .with_no_client_auth(),
        _ => {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            if let TlsMode::Verify { ca_file: Some(path) } = mode {
                use rustls::pki_types::pem::PemObject;
                let certs = CertificateDer::pem_file_iter(path)
                    .map_err(|e| EngineError::input(format!("Could not read the CA file '{path}': {e}.")))?;
                for cert in certs {
                    let cert = cert.map_err(|e| EngineError::input(format!("The CA file '{path}' is not valid PEM: {e}.")))?;
                    roots
                        .add(cert)
                        .map_err(|e| EngineError::input(format!("The CA file '{path}' has an unusable certificate: {e}.")))?;
                }
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        }
    };
    Ok(MakeRustlsConnect::new(config))
}

pub(crate) fn map_err(e: tokio_postgres::Error) -> EngineError {
    if let Some(db) = e.as_db_error() {
        let code = db.code().code().to_owned();
        if code == "57014" {
            return if db.message().contains("timeout") {
                EngineError::new(
                    ErrorKind::Timeout,
                    "The statement ran longer than the statement timeout and was cancelled.",
                )
            } else {
                EngineError::cancelled()
            };
        }
        let msg = db.message().to_owned();
        let detail = match (db.detail(), db.where_()) {
            (Some(d), Some(w)) => Some(format!(
                "{d}
Where: {w}"
            )),
            (Some(d), None) => Some(d.to_owned()),
            (None, Some(w)) => Some(format!("Where: {w}")),
            (None, None) => None,
        };
        let position = match db.position() {
            Some(tokio_postgres::error::ErrorPosition::Original(p)) => Some(p.saturating_sub(1)),
            _ => None,
        };
        let kind = if code.starts_with("28") || code.starts_with("3D") || code.starts_with("08") {
            ErrorKind::Connect
        } else {
            ErrorKind::Server
        };
        return EngineError {
            code: Some(code),
            detail,
            hint: db.hint().map(str::to_owned),
            position,
            ..EngineError::new(kind, msg)
        };
    }
    if e.is_closed() {
        return EngineError::connect("The connection to the server was closed. Reconnect and try again.");
    }
    EngineError::server(e.to_string(), None)
}

/// Connects and spawns the connection task. Returns the client and the TLS connector (needed again to send cancel requests).
pub(crate) async fn open(opts: &PgOptions) -> Result<(Client, MakeRustlsConnect)> {
    let tls = tls_connector(opts)?;
    let config = opts.config();
    let fut = config.connect(tls.clone());
    let (client, connection) = match tokio::time::timeout(opts.connect_timeout + Duration::from_secs(1), fut).await {
        Err(_) => {
            return Err(EngineError::timeout(
                &format!("Connecting to {}:{}", opts.host, opts.port),
                opts.connect_timeout,
            ))
        }
        Ok(Err(e)) => {
            let mut err = map_err(e);
            if err.kind == ErrorKind::Server {
                err.kind = ErrorKind::Connect;
                err.message = format!("Could not connect to {}:{}: {}", opts.host, opts.port, err.message);
            }
            return Err(err);
        }
        Ok(Ok(pair)) => pair,
    };
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, tls))
}

/// Identifier quoting, text-cast placeholders and ILIKE for PostgreSQL.
pub struct PgDialect;

impl SqlDialect for PgDialect {
    fn quote_ident(&self, name: &str) -> String {
        sqlbuild::quote_with(name, '"')
    }
    /// Every value is sent as text and cast by the server to the column's type, so enums, domains, arrays, ranges and
    /// extension types all work without client-side encoders.
    fn placeholder(&self, n: usize, target: &ParamTarget) -> String {
        match target {
            ParamTarget::Column(col) => format!("(${n}::text)::{}", col.type_name),
            ParamTarget::Text => format!("${n}::text"),
        }
    }
    /// Types without a binary decoder (full-text search, LSNs, multiranges, PostGIS) are selected as text.
    fn select_expr(&self, col: &ColumnInfo) -> String {
        let q = self.quote_ident(&col.name);
        if col.kind == crate::engines::common::ValueKind::Other && catalog::TEXT_CAST.iter().any(|t| col.type_name.starts_with(t)) {
            format!("{q}::text AS {q}")
        } else {
            q
        }
    }
    fn text_expr(&self, col_sql: &str, _col: &ColumnInfo) -> String {
        format!("{col_sql}::text")
    }
    fn like(&self, lhs: &str, rhs: &str, ci: bool, negate: bool) -> String {
        format!(
            "{lhs} {}{} {rhs}",
            if negate { "NOT " } else { "" },
            if ci { "ILIKE" } else { "LIKE" }
        )
    }
    fn null_safe_eq(&self) -> Option<&'static str> {
        Some("IS NOT DISTINCT FROM")
    }
    /// `json` and `xml` have no equality operator, so row checks compare their text.
    fn compare_column(&self, col_sql: String, col: &ColumnInfo) -> String {
        if is_text_compared(col) {
            format!("{col_sql}::text")
        } else {
            col_sql
        }
    }
    fn compare_param(&self, placeholder: String, col: &ColumnInfo) -> String {
        if is_text_compared(col) {
            format!("({placeholder})::text")
        } else {
            placeholder
        }
    }
}

fn is_text_compared(col: &ColumnInfo) -> bool {
    matches!(col.type_name.as_str(), "json" | "xml")
}

pub(crate) fn text_params(params: &[sqlbuild::Param]) -> Vec<Option<String>> {
    params.iter().map(|p| types::to_param_text(&p.value)).collect()
}

pub(crate) fn as_sql(params: &[Option<String>]) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(|p| p as &(dyn ToSql + Sync)).collect()
}

/// An open PostgreSQL database.
pub struct PgEngine {
    opts: PgOptions,
    client: Mutex<Client>,
    /// For the CancelRequest that stops a page read.
    tls: MakeRustlsConnect,
    version: String,
}

impl PgEngine {
    pub async fn connect(opts: PgOptions) -> Result<Self> {
        let (client, tls) = open(&opts).await?;
        let version: String = client
            .query_one("SELECT current_setting('server_version')", &[])
            .await
            .map_err(map_err)?
            .get(0);
        Ok(Self {
            opts,
            client: Mutex::new(client),
            tls,
            version,
        })
    }

    /// A second engine on another database of the same server (same credentials and TLS).
    pub async fn connect_database(&self, database: &str) -> Result<Self> {
        let mut o = self.opts.clone();
        o.database = database.to_owned();
        Self::connect(o).await
    }

    pub fn server_version(&self) -> &str {
        &self.version
    }

    pub async fn ping(&self) -> Result<()> {
        self.client.lock().await.simple_query("SELECT 1").await.map_err(map_err)?;
        Ok(())
    }

    /// Runs statements that cannot sit in a transaction block (`CREATE DATABASE`).
    pub async fn batch(&self, sql: &str) -> Result<()> {
        self.client.lock().await.batch_execute(sql).await.map_err(map_err)
    }

    /// Whether the server closed this engine's connection (the caller reconnects). A connection that is busy with a
    /// long statement is alive: this never waits for it.
    pub fn is_closed(&self) -> bool {
        self.client.try_lock().is_ok_and(|client| client.is_closed())
    }

    pub fn database(&self) -> &str {
        &self.opts.database
    }

    pub fn classify(sql: &str) -> Classification {
        classify(Dialect::Postgres, sql)
    }

    pub async fn open_session(&self) -> Result<PgSession> {
        let (client, tls) = open(&self.opts).await?;
        Ok(PgSession::new(client, tls))
    }

    /// Databases that accept connections (templates hidden).
    pub async fn list_databases(&self) -> Result<Vec<String>> {
        let c = self.client.lock().await;
        let rows = c
            .query(
                "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY datname",
                &[],
            )
            .await
            .map_err(map_err)?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    pub async fn browse(&self, req: &BrowseRequest) -> Result<Page> {
        let c = self.client.lock().await;
        let d = catalog::describe(&c, &req.object).await?;
        let key = d.primary_key.as_ref().map(|k| k.columns.clone()).unwrap_or_default();
        let mut req = req.clone();
        req.object = d.object.clone();
        let built = sqlbuild::build_browse(&PgDialect, &req, &d.columns, &key)?;
        let params = text_params(&built.params);
        let rows = c.query(&built.sql, &as_sql(&params)).await.map_err(map_err)?;
        let rows: Vec<_> = rows.iter().map(types::row_values).collect();
        let estimated_total = if !req.estimate_total {
            None
        } else if built.where_sql.is_empty() {
            d.row_estimate
        } else {
            // The planner's estimate for the filter, with the same bound parameters (only the filter's ones come first).
            let n = built.params.len() - req.after.as_ref().map_or(0, |a| a.len() * (a.len() + 1) / 2);
            let sql = format!(
                "EXPLAIN (FORMAT JSON) SELECT 1 FROM {} WHERE {}",
                PgDialect.table_sql(&d.object),
                built.where_sql
            );
            let p = &params[..n];
            match c.query_one(&sql, &as_sql(p)).await {
                Ok(row) => plan_rows(&types::row_values(&row)[0]),
                Err(_) => None,
            }
        };
        Ok(sqlbuild::finish_page(d.columns, rows, req.limit, &built, key, estimated_total))
    }

    /// Applies all changes in one transaction; an update or delete that does not hit exactly one row rolls everything back.
    pub async fn apply_changes(&self, obj: &ObjectRef, changes: Vec<RowChange>) -> Result<ChangeSummary> {
        let mut c = self.client.lock().await;
        let d = catalog::describe(&c, obj).await?;
        let tx = c.transaction().await.map_err(map_err)?;
        let mut summary = ChangeSummary::default();
        for ch in &changes {
            let (built, expect_one) = match ch {
                RowChange::Insert { values } => (sqlbuild::build_insert(&PgDialect, &d.object, values, &d.columns)?, false),
                RowChange::Update { key, values } => (sqlbuild::build_update(&PgDialect, &d.object, key, values, &d.columns)?, true),
                RowChange::Delete { key } => (sqlbuild::build_delete(&PgDialect, &d.object, key, &d.columns)?, true),
            };
            let params = text_params(&built.params);
            let n = tx.execute(&built.sql, &as_sql(&params)).await.map_err(map_err)?;
            if expect_one && n != 1 {
                return Err(EngineError::input(format!(
                    "A change matched {n} rows instead of one, so nothing was saved. Reload the table and try again."
                )));
            }
            match ch {
                RowChange::Insert { .. } => summary.inserted += n,
                RowChange::Update { .. } => summary.updated += n,
                RowChange::Delete { .. } => summary.deleted += n,
            }
        }
        tx.commit().await.map_err(map_err)?;
        Ok(summary)
    }

    /// The current database with its schemas and their objects, and extensions.
    pub async fn catalog(&self) -> Result<crate::engines::common::CatalogNode> {
        let c = self.client.lock().await;
        catalog::tree(&c, &self.opts.database).await
    }

    pub async fn describe(&self, obj: &ObjectRef) -> Result<crate::engines::common::ObjectDescription> {
        let c = self.client.lock().await;
        catalog::describe(&c, obj).await
    }
}

fn plan_rows(v: &crate::engines::common::Value) -> Option<i64> {
    let crate::engines::common::Value::Json(text) = v else {
        return None;
    };
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    json.get(0)?.get("Plan")?.get("Plan Rows")?.as_f64().map(|f| f as i64)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parses_urls_and_key_values() {
        let o =
            PgOptions::parse("postgres://bob:p%40ss@db.example.com:6543/shop?sslmode=verify-full&sslrootcert=C:/ca.pem&connect_timeout=3")
                .unwrap();
        assert_eq!(
            (o.user.as_str(), o.password.as_deref(), o.host.as_str(), o.port),
            ("bob", Some("p@ss"), "db.example.com", 6543)
        );
        assert_eq!(o.database, "shop");
        assert_eq!(
            o.tls,
            TlsMode::Verify {
                ca_file: Some("C:/ca.pem".into())
            }
        );
        assert_eq!(o.connect_timeout, Duration::from_secs(3));
        let k = PgOptions::parse("host=localhost port=5433 user=postgres password='it''s' dbname=x sslmode=require").unwrap_err();
        assert!(k.message.contains("Unknown") || k.message.contains("'"), "{k}");
        let k = PgOptions::parse(r"host=localhost port=5433 user=postgres password='it\'s me' dbname=x sslmode=require").unwrap();
        assert_eq!(
            (k.port, k.password.as_deref(), k.tls.clone()),
            (5433, Some("it's me"), TlsMode::SkipVerify)
        );
        let v6 = PgOptions::parse("postgresql://[::1]:5432").unwrap();
        assert_eq!((v6.host.as_str(), v6.database.as_str()), ("::1", "postgres"));
        assert!(PgOptions::parse("postgres://h/db?bogus=1").is_err());
        assert!(PgOptions::parse("postgres://h:notaport/db").is_err());
    }

    #[test]
    fn placeholders_cast_text_to_the_column_type() {
        let col = ColumnInfo::result("price", "numeric(12,2)", crate::engines::common::ValueKind::Decimal);
        assert_eq!(PgDialect.placeholder(3, &ParamTarget::Column(col)), "($3::text)::numeric(12,2)");
        assert_eq!(PgDialect.placeholder(1, &ParamTarget::Text), "$1::text");
    }

    #[test]
    fn a_tunnelled_tls_connection_sends_the_database_name_and_connects_to_the_tunnel() {
        use tokio_postgres::config::Host;
        let tunnelled = PgOptions {
            host: "127.0.0.1".into(),
            port: 15432,
            tls: TlsMode::Verify { ca_file: None },
            tls_spec: Some(crate::net::TlsSpec {
                mode: crate::net::TlsMode::VerifyFull,
                server_name: Some("db.internal".into()),
                ..Default::default()
            }),
            ..PgOptions::default()
        };
        let config = tunnelled.config();
        assert_eq!(config.get_hosts(), [Host::Tcp("db.internal".into())]);
        assert_eq!(config.get_hostaddrs(), ["127.0.0.1".parse::<std::net::IpAddr>().unwrap()]);
        let plain = PgOptions {
            host: "127.0.0.1".into(),
            ..PgOptions::default()
        };
        assert_eq!(plain.config().get_hosts(), [Host::Tcp("127.0.0.1".into())]);
        assert!(plain.config().get_hostaddrs().is_empty());
    }

    #[test]
    fn tls_connectors_build() {
        let with = |tls: TlsMode| PgOptions {
            tls,
            ..PgOptions::default()
        };
        assert!(tls_connector(&with(TlsMode::SkipVerify)).is_ok());
        assert!(tls_connector(&with(TlsMode::Verify { ca_file: None })).is_ok());
        assert!(tls_connector(&with(TlsMode::Verify {
            ca_file: Some("Z:/missing.pem".into())
        }))
        .is_err());
    }
}
