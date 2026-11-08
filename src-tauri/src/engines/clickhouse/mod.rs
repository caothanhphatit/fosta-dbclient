//! ClickHouse engine over the HTTP interface, with the app's existing `reqwest` (no extra client crate).
//!
//! Why HTTP and `JSONCompactEachRowWithNamesAndTypes`: the HTTP interface is ClickHouse's most widely available protocol (it
//! works through proxies and on ClickHouse Cloud), and this format streams one JSON array per line after a header line of names
//! and one of types. Every row can be decoded as soon as its line arrives, the types come with the data, and with 64-bit
//! integers, decimals and non-finite floats quoted nothing loses precision. RowBinary would be faster to parse but needs a
//! decoder for every type (Variant, Dynamic, nested tuples, ...) before anything can be shown; the `clickhouse` crate is built
//! around typed structs known at compile time, which a client running arbitrary queries does not have.
//!
//! Every request carries a `query_id`: cancelling is `KILL QUERY WHERE query_id = …` on a separate request, stopping at the
//! row limit drops the response (`cancel_http_readonly_queries_on_client_close` makes the server stop the query) and kills it
//! too. The statement timeout is the server's `max_execution_time`. ClickHouse has no transactions, so row changes run one by
//! one (inserts, then lightweight `ALTER TABLE … UPDATE`/`DELETE FROM` with `mutations_sync`).

pub mod adapter;
mod browse;
mod connect;
#[cfg(test)]
mod contract_tests;
pub mod descriptor;
mod edit;
mod ops;
pub mod session;
#[cfg(test)]
mod tests;
mod tree;
pub mod types;

pub use adapter::{ClickHouseAdapter, CLICKHOUSE};
pub use session::ChSession;

use crate::engines::common::split::Dialect;
use crate::engines::common::sqlbuild::{self, ParamTarget, SqlDialect};
use crate::engines::common::value::percent_decode;
use crate::engines::common::{
    classify, CatalogNode, Classification, ColumnInfo, EngineError, ErrorKind, IndexInfo, KeyInfo, NodeKind, ObjectDescription, ObjectRef,
    Result, TlsMode, Value,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The result format asked for when a statement has no FORMAT clause of its own.
pub const FORMAT: &str = "JSONCompactEachRowWithNamesAndTypes";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChOptions {
    pub host: String,
    /// HTTP port (8123, or 8443 with TLS).
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: String,
    /// `Disable` = http, otherwise https (verified against the system roots plus `ca_file`, or not verified).
    pub tls: TlsMode,
    pub connect_timeout: Duration,
    /// Where to connect when it is not `host:port` (the local end of an SSH tunnel); `host` stays the URL's host.
    pub connect_to: Option<std::net::SocketAddr>,
}

impl Default for ChOptions {
    fn default() -> Self {
        Self {
            connect_to: None,
            host: "localhost".into(),
            port: 8123,
            user: "default".into(),
            password: None,
            database: "default".into(),
            tls: TlsMode::Disable,
            connect_timeout: Duration::from_secs(10),
        }
    }
}

impl ChOptions {
    /// Parses `clickhouse://user:pass@host:port/db?secure=true`, `http(s)://host:port/?user=…&password=…&database=…`
    /// (the HTTP interface's own parameters) and `ch://…`. `secure=true` or `https` switch to TLS (port 8443 by default);
    /// `skip_verify=true` turns verification off and `ca_file=…` adds a CA certificate.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let (scheme, rest) = s
            .split_once("://")
            .ok_or_else(|| EngineError::input("Use a URL such as clickhouse://user@host:8123/db."))?;
        let mut o = Self::default();
        let mut secure = match scheme {
            "https" => true,
            "http" | "clickhouse" | "ch" => false,
            other => return Err(EngineError::input(format!("Unknown scheme '{other}' for ClickHouse."))),
        };
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
        let mut port = None;
        match hostport.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') || p.chars().all(|c| c.is_ascii_digit()) => {
                o.host = h.trim_matches(['[', ']']).to_owned();
                port = Some(
                    p.parse::<u16>()
                        .map_err(|_| EngineError::input(format!("'{p}' is not a valid port.")))?,
                );
            }
            _ => o.host = hostport.trim_matches(['[', ']']).to_owned(),
        }
        if o.host.is_empty() {
            o.host = "localhost".into();
        }
        if !db.trim_matches('/').is_empty() {
            o.database = percent_decode(db.trim_matches('/'));
        }
        let (mut skip, mut ca) = (false, None);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = percent_decode(v);
            match k {
                "user" => o.user = v,
                "password" => o.password = Some(v),
                "database" => o.database = v,
                "secure" => secure = v == "true" || v == "1",
                "skip_verify" => skip = v == "true" || v == "1",
                "ca_file" => ca = Some(v),
                "connect_timeout" => {
                    o.connect_timeout = Duration::from_secs(
                        v.parse()
                            .map_err(|_| EngineError::input("connect_timeout must be a number of seconds."))?,
                    )
                }
                other => return Err(EngineError::input(format!("Unknown ClickHouse connection option '{other}'."))),
            }
        }
        o.port = port.unwrap_or(if secure { 8443 } else { 8123 });
        o.tls = match (secure, skip) {
            (false, _) => TlsMode::Disable,
            (true, true) => TlsMode::SkipVerify,
            (true, false) => TlsMode::Verify { ca_file: ca },
        };
        Ok(o)
    }

    fn base_url(&self) -> String {
        let scheme = if self.tls == TlsMode::Disable { "http" } else { "https" };
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("{scheme}://{host}:{}/", self.port)
    }
}

/// A new query id: unique per process, readable in `system.processes`.
pub fn new_query_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "fosta-{:x}-{:x}-{}",
        std::process::id(),
        nanos,
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The HTTP transport shared by the engine and its sessions.
#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    base: String,
    user: String,
    password: String,
    pub database: String,
    /// Ask the server for errors as plain text at the end of the stream (`http_write_exception_in_output_format = 0`).
    pub plain_exceptions: bool,
}

/// What came back from one request.
pub struct Response {
    pub inner: reqwest::Response,
    /// The format the server used (`X-ClickHouse-Format`), absent for statements without a result.
    pub format: Option<String>,
}

impl Http {
    pub fn new(o: &ChOptions) -> Result<Self> {
        Self::with_tls(o, None)
    }

    /// `tls` is the rustls configuration the connect code builds from the profile (`net::tls::rustls_client_config`);
    /// it replaces the core's own `TlsMode` handling, which stays for connection URLs and the core's tests.
    pub fn with_tls(o: &ChOptions, tls: Option<rustls::ClientConfig>) -> Result<Self> {
        let mut b = reqwest::Client::builder()
            .connect_timeout(o.connect_timeout)
            .pool_idle_timeout(Duration::from_secs(30));
        let preconfigured = tls.is_some();
        if let Some(config) = tls {
            b = b.use_preconfigured_tls(config);
        }
        // Through an SSH tunnel the URL keeps the database host (certificate names) and connects to the tunnel's end.
        if let Some(addr) = o.connect_to {
            b = b.resolve(&o.host, addr);
        }
        match &o.tls {
            _ if preconfigured => {}
            TlsMode::SkipVerify => b = b.danger_accept_invalid_certs(true),
            TlsMode::Verify { ca_file: Some(path) } => {
                let pem = std::fs::read(path).map_err(|e| EngineError::input(format!("Could not read the CA file '{path}': {e}.")))?;
                let cert = reqwest::Certificate::from_pem(&pem)
                    .map_err(|e| EngineError::input(format!("The CA file '{path}' is not a valid PEM certificate: {e}.")))?;
                b = b.add_root_certificate(cert);
            }
            _ => {}
        }
        let client = b
            .build()
            .map_err(|e| EngineError::internal(format!("HTTP client setup failed: {e}")))?;
        Ok(Self {
            client,
            base: o.base_url(),
            user: o.user.clone(),
            password: o.password.clone().unwrap_or_default(),
            database: o.database.clone(),
            plain_exceptions: false,
        })
    }

    /// Sends one statement. `settings` are ClickHouse settings / HTTP parameters (`max_execution_time`, `session_id`,
    /// `param_p1`, ...).
    pub async fn send(&self, sql: &str, query_id: &str, settings: &[(String, String)]) -> Result<Response> {
        let mut q: Vec<(String, String)> = vec![
            ("database".into(), self.database.clone()),
            ("query_id".into(), query_id.to_owned()),
            ("default_format".into(), FORMAT.into()),
            ("output_format_json_quote_64bit_integers".into(), "1".into()),
            ("output_format_json_quote_decimals".into(), "1".into()),
            ("output_format_json_quote_denormals".into(), "1".into()),
            ("output_format_json_validate_utf8".into(), "1".into()),
            ("cancel_http_readonly_queries_on_client_close".into(), "1".into()),
        ];
        if self.plain_exceptions {
            q.push(("http_write_exception_in_output_format".into(), "0".into()));
        }
        q.extend(settings.iter().cloned());
        let resp = self
            .client
            .post(&self.base)
            .query(&q)
            .header("X-ClickHouse-User", &self.user)
            .header("X-ClickHouse-Key", &self.password)
            .body(sql.to_owned())
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() || e.is_timeout() {
                    EngineError::connect(format!("Could not reach ClickHouse at {}: {e}", self.base))
                } else {
                    EngineError::server(e.to_string(), None)
                }
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(server_error(&text));
        }
        let format = resp
            .headers()
            .get("X-ClickHouse-Format")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        Ok(Response { inner: resp, format })
    }

    /// Runs a statement and reads its whole result (catalog queries and small results).
    pub async fn query(&self, sql: &str, params: &[(String, String)]) -> Result<(Vec<ColumnInfo>, Vec<Vec<Value>>)> {
        let resp = self.send(sql, &new_query_id(), params).await?;
        let mut reader = RowReader::default();
        let mut inner = resp.inner;
        let mut rows = Vec::new();
        loop {
            let chunk = inner
                .chunk()
                .await
                .map_err(|e| EngineError::server(format!("Reading the result failed: {e}"), None))?;
            let done = chunk.is_none();
            reader.feed(chunk.as_deref().unwrap_or_default(), done)?;
            while let Some(row) = reader.next_row()? {
                rows.push(row);
            }
            if done {
                break;
            }
        }
        Ok((reader.columns, rows))
    }

    /// Runs a statement without a result.
    pub async fn execute(&self, sql: &str, params: &[(String, String)]) -> Result<()> {
        let resp = self.send(sql, &new_query_id(), params).await?;
        let text = resp.inner.text().await.unwrap_or_default();
        if text.contains("DB::Exception") {
            return Err(server_error(&text));
        }
        Ok(())
    }

    /// `KILL QUERY` for a query id (the query may already be gone). `sync` waits until it has stopped.
    pub async fn kill(&self, query_id: &str, sync: bool) -> Result<()> {
        let sql = if sync {
            "KILL QUERY WHERE query_id = {qid:String} SYNC"
        } else {
            "KILL QUERY WHERE query_id = {qid:String} ASYNC"
        };
        self.execute(sql, &[("param_qid".into(), query_id.to_owned())]).await
    }
}

/// Parses `Code: 60. DB::Exception: Unknown table … (UNKNOWN_TABLE) (version …)`.
pub fn server_error(text: &str) -> EngineError {
    let text = text.trim();
    // Errors after the headers arrive inside the body, possibly after other output: look for the marker anywhere.
    let mut code = text.find("Code: ").and_then(|i| text[i + 6..].split('.').next()).map(str::to_owned);
    if code.is_none() {
        code = [("(QUERY_WAS_CANCELLED)", "394"), ("(TIMEOUT_EXCEEDED)", "159")]
            .iter()
            .find(|(name, _)| text.contains(name))
            .map(|(_, c)| (*c).to_owned());
    }
    let message = match text.find("DB::Exception: ") {
        Some(i) => text[i + "DB::Exception: ".len()..]
            .split(" (version")
            .next()
            .unwrap_or(text)
            .trim()
            .to_owned(),
        None if text.is_empty() => "ClickHouse returned an error without a message.".to_owned(),
        None => text.to_owned(),
    };
    let kind = match code.as_deref() {
        Some("394") => return EngineError::cancelled(),
        Some("159") => ErrorKind::Timeout,
        Some("516" | "192" | "193" | "194" | "81") => ErrorKind::Connect,
        _ => ErrorKind::Server,
    };
    // "Syntax error: failed at position 8 (FORM) (line 1, col 8): … Expected one of: …": the position is 1-based, and the
    // list of expected tokens is long, so it moves to the detail.
    let position = message
        .split_once("failed at position ")
        .and_then(|(_, rest)| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse::<u32>().ok())
        .map(|position| position.saturating_sub(1));
    let (message, detail) = match message.find(". Expected ") {
        Some(i) => (format!("{}.", &message[..i]), Some(message[i + 2..].trim().to_owned())),
        None => (message, None),
    };
    EngineError {
        message,
        code,
        kind,
        detail,
        hint: None,
        position,
    }
}

/// Splits a `JSONCompactEachRowWithNamesAndTypes` body into the header and rows as bytes arrive.
#[derive(Default)]
pub struct RowReader {
    buf: Vec<u8>,
    names: Option<Vec<String>>,
    types: Vec<String>,
    pub columns: Vec<ColumnInfo>,
    done: bool,
}

impl RowReader {
    pub fn feed(&mut self, bytes: &[u8], done: bool) -> Result<()> {
        self.buf.extend_from_slice(bytes);
        self.done = done;
        Ok(())
    }

    fn next_line(&mut self) -> Option<Vec<u8>> {
        if let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            return Some(line[..line.len() - 1].to_vec());
        }
        if self.done && !self.buf.is_empty() {
            return Some(std::mem::take(&mut self.buf));
        }
        None
    }

    /// True once the names and types lines were read.
    pub fn has_header(&self) -> bool {
        !self.columns.is_empty() || (self.names.is_some() && !self.types.is_empty())
    }

    /// The next complete row; reads the two header lines first.
    pub fn next_row(&mut self) -> Result<Option<Vec<Value>>> {
        loop {
            let Some(line) = self.next_line() else { return Ok(None) };
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let json: serde_json::Value = match serde_json::from_slice(&line) {
                Ok(j) => j,
                // A failure after the first rows: `__exception__`, then the message (the setting `plain_exceptions` asks for this).
                Err(_) if line.trim_ascii() == b"__exception__" => continue,
                Err(_) => return Err(server_error(&String::from_utf8_lossy(&line))),
            };
            if let Some(e) = json.get("exception").and_then(|e| e.as_str()) {
                return Err(server_error(e));
            }
            let serde_json::Value::Array(items) = json else {
                return Err(server_error(&String::from_utf8_lossy(&line)));
            };
            if self.names.is_none() {
                self.names = Some(items.iter().map(|v| v.as_str().unwrap_or_default().to_owned()).collect());
                continue;
            }
            if self.columns.is_empty() && self.types.is_empty() {
                self.types = items.iter().map(|v| v.as_str().unwrap_or_default().to_owned()).collect();
                let names = self.names.clone().unwrap_or_default();
                self.columns = names
                    .iter()
                    .zip(&self.types)
                    .map(|(n, t)| {
                        let mut c = ColumnInfo::result(n, t, types::kind_of(t));
                        c.nullable = types::is_nullable(t);
                        c
                    })
                    .collect();
                continue;
            }
            return Ok(Some(items.iter().zip(&self.types).map(|(v, t)| types::read(v, t)).collect()));
        }
    }
}

/// Identifier quoting, `{pN:Type}` placeholders and ILIKE for ClickHouse.
pub struct ChDialect;

impl SqlDialect for ChDialect {
    fn quote_ident(&self, name: &str) -> String {
        format!("`{}`", name.replace('\\', "\\\\").replace('`', "\\`"))
    }
    fn table_sql(&self, obj: &ObjectRef) -> String {
        match obj.database.as_deref().or(obj.schema.as_deref()) {
            Some(db) => format!("{}.{}", self.quote_ident(db), self.quote_ident(&obj.name)),
            None => self.quote_ident(&obj.name),
        }
    }
    fn placeholder(&self, n: usize, target: &ParamTarget) -> String {
        match target {
            ParamTarget::Column(col) => format!("{{p{n}:{}}}", param_type(&col.type_name)),
            ParamTarget::Text => format!("{{p{n}:String}}"),
        }
    }
    fn text_expr(&self, col_sql: &str, _col: &ColumnInfo) -> String {
        format!("toString({col_sql})")
    }
    /// ClickHouse LIKE uses backslash escapes and has no ESCAPE clause.
    fn like(&self, lhs: &str, rhs: &str, ci: bool, negate: bool) -> String {
        format!(
            "{lhs} {}{} {rhs}",
            if negate { "NOT " } else { "" },
            if ci { "ILIKE" } else { "LIKE" }
        )
    }
}

/// The type of a query parameter bound to a column: LowCardinality is a storage detail, not a parameter type.
fn param_type(ty: &str) -> String {
    let t = ty.trim();
    if let Some(inner) = t.strip_prefix("LowCardinality(").and_then(|s| s.strip_suffix(')')) {
        return param_type(inner);
    }
    t.to_owned()
}

fn params_of(params: &[sqlbuild::Param]) -> Vec<(String, String)> {
    params
        .iter()
        .enumerate()
        .map(|(i, p)| (format!("param_p{}", i + 1), types::to_param_text(&p.value)))
        .collect()
}

/// An open ClickHouse server.
pub struct ChEngine {
    opts: ChOptions,
    http: Http,
    version: String,
}

impl ChEngine {
    pub async fn connect(opts: ChOptions) -> Result<Self> {
        Self::connect_with(opts, None).await
    }

    /// Connects with a rustls configuration built from the profile (see [`Http::with_tls`]).
    pub async fn connect_with(opts: ChOptions, tls: Option<rustls::ClientConfig>) -> Result<Self> {
        let http = Http::with_tls(&opts, tls)?;
        let (_, rows) = http.query("SELECT version()", &[]).await?;
        let version = rows.first().and_then(|r| r.first()).and_then(Value::display).unwrap_or_default();
        let mut engine = Self { opts, http, version };
        // Since 23.3 a failure after the first rows is written into the result as one more JSON row, which looks like data;
        // asking for the plain text keeps errors recognisable.
        engine.http.plain_exceptions = engine.version_numbers() >= (23, 3);
        Ok(engine)
    }

    pub fn server_version(&self) -> &str {
        &self.version
    }

    /// The server's `(major, minor)`, for settings that only newer servers know.
    pub fn version_numbers(&self) -> (u32, u32) {
        let mut parts = self.version.split('.').map(|part| part.parse::<u32>().unwrap_or(0));
        (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
    }

    /// An engine that is not connected to anything, for tests of statement building.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        let opts = ChOptions::default();
        Self {
            http: Http::new(&opts).expect("an HTTP client"),
            opts,
            version: "25.8.1.1".into(),
        }
    }

    pub fn http(&self) -> &Http {
        &self.http
    }

    pub fn default_database(&self) -> &str {
        &self.opts.database
    }

    pub fn classify(sql: &str) -> Classification {
        classify(Dialect::ClickHouse, sql)
    }

    /// A session: its own `session_id`, so `SET` and temporary tables carry over between runs.
    pub fn open_session(&self) -> Result<ChSession> {
        Ok(ChSession::new(self.http.clone()))
    }

    pub(super) async fn strings(&self, sql: &str, params: &[(&str, &str)]) -> Result<Vec<Vec<String>>> {
        let p: Vec<(String, String)> = params.iter().map(|(k, v)| (format!("param_{k}"), (*v).to_owned())).collect();
        let (_, rows) = self.http.query(sql, &p).await?;
        Ok(rows
            .into_iter()
            .map(|r| r.into_iter().map(|v| v.display().unwrap_or_default()).collect())
            .collect())
    }

    /// Databases (system ones last) with tables (and their engine), views, materialized views and dictionaries.
    pub async fn catalog(&self) -> Result<Vec<CatalogNode>> {
        let dbs = self
            .strings(
                "SELECT name FROM system.databases ORDER BY name IN ('system', 'INFORMATION_SCHEMA', 'information_schema'), name",
                &[],
            )
            .await?;
        let tables = self
            .strings(
                "SELECT database, name, engine, toString(total_rows), comment FROM system.tables WHERE NOT is_temporary \
                 AND database NOT IN ('INFORMATION_SCHEMA', 'information_schema') ORDER BY database, name",
                &[],
            )
            .await?;
        let mut out = Vec::new();
        for db in dbs {
            let db = &db[0];
            let mut folders: [Vec<CatalogNode>; 4] = Default::default();
            for t in tables.iter().filter(|t| t[0] == *db) {
                let (name, engine) = (&t[1], &t[2]);
                let (i, kind) = match engine.as_str() {
                    "View" => (1, NodeKind::View),
                    "MaterializedView" => (2, NodeKind::MaterializedView),
                    "Dictionary" => (3, NodeKind::Dictionary),
                    _ => (0, NodeKind::Table),
                };
                let detail = if kind == NodeKind::Table { Some(engine.clone()) } else { None };
                folders[i].push(CatalogNode::object(ObjectRef::new(Some(db), None, name, kind), detail));
            }
            let labels = ["Tables", "Views", "Materialized Views", "Dictionaries"];
            let kids = folders
                .into_iter()
                .zip(labels)
                .map(|(items, l)| CatalogNode::container(NodeKind::Folder, l, items))
                .collect();
            let mut node = CatalogNode::container(NodeKind::Database, db, kids);
            node.object = Some(ObjectRef::new(Some(db), None, db, NodeKind::Database));
            out.push(node);
        }
        Ok(out)
    }

    fn db_of<'a>(&'a self, obj: &'a ObjectRef) -> &'a str {
        obj.database.as_deref().or(obj.schema.as_deref()).unwrap_or(&self.opts.database)
    }

    pub async fn describe(&self, obj: &ObjectRef) -> Result<ObjectDescription> {
        let db = self.db_of(obj).to_owned();
        let info = self
            .strings(
                "SELECT engine, toString(total_rows), comment, partition_key, sorting_key, primary_key FROM system.tables \
                 WHERE database = {db:String} AND name = {t:String}",
                &[("db", &db), ("t", &obj.name)],
            )
            .await?;
        let Some(info) = info.into_iter().next() else {
            return Err(EngineError::input(format!(
                "There is no table named '{}' in database '{db}'.",
                obj.name
            )));
        };
        let kind = match info[0].as_str() {
            "View" => NodeKind::View,
            "MaterializedView" => NodeKind::MaterializedView,
            "Dictionary" => NodeKind::Dictionary,
            _ => NodeKind::Table,
        };
        let mut d = ObjectDescription::new(ObjectRef::new(Some(&db), None, &obj.name, kind));
        d.row_estimate = info[1].parse().ok();
        d.comment = Some(info[2].clone()).filter(|c| !c.is_empty());
        for (k, v) in [
            ("engine", &info[0]),
            ("partitionKey", &info[3]),
            ("sortingKey", &info[4]),
            ("primaryKey", &info[5]),
        ] {
            if !v.is_empty() {
                d.extra.push((k.into(), v.clone()));
            }
        }
        let cols = self
            .strings(
                "SELECT name, type, default_kind, default_expression, comment, toString(is_in_primary_key) FROM system.columns \
                 WHERE database = {db:String} AND table = {t:String} ORDER BY position",
                &[("db", &db), ("t", &obj.name)],
            )
            .await?;
        let mut in_pk = Vec::new();
        for c in cols {
            let (default, generated) = match c[2].as_str() {
                "MATERIALIZED" | "ALIAS" | "EPHEMERAL" => (None, Some(format!("{} {}", c[2], c[3]))),
                "DEFAULT" => (Some(c[3].clone()), None),
                _ => (None, None),
            };
            if c[5] == "1" || c[5] == "true" {
                in_pk.push(c[0].clone());
            }
            d.columns.push(ColumnInfo {
                hints: Default::default(),
                kind: types::kind_of(&c[1]),
                nullable: types::is_nullable(&c[1]),
                name: c[0].clone(),
                type_name: c[1].clone(),
                default,
                comment: Some(c[4].clone()).filter(|s| !s.is_empty()),
                identity: None,
                generated,
            });
        }
        // Key order is the PRIMARY KEY expression order; expressions that are not plain columns are left out.
        let pk: Vec<String> = info[5]
            .split(',')
            .map(|p| p.trim().trim_matches('`').to_owned())
            .filter(|p| in_pk.contains(p))
            .collect();
        if !pk.is_empty() {
            // ClickHouse's primary key orders data; it is not unique.
            d.primary_key = Some(KeyInfo {
                name: Some("PRIMARY KEY (not unique)".into()),
                columns: pk.clone(),
            });
            d.indexes.push(IndexInfo {
                name: "primary".into(),
                columns: pk,
                unique: false,
                primary: true,
                method: Some("sparse".into()),
                definition: Some(info[5].clone()),
            });
        }
        let skipping = self
            .strings(
                "SELECT name, type_full, expr, toString(granularity) FROM system.data_skipping_indices \
                 WHERE database = {db:String} AND table = {t:String} ORDER BY name",
                &[("db", &db), ("t", &obj.name)],
            )
            .await?;
        for s in skipping {
            d.indexes.push(IndexInfo {
                name: s[0].clone(),
                columns: vec![s[2].clone()],
                unique: false,
                primary: false,
                method: Some(s[1].clone()),
                definition: Some(format!("INDEX {} {} TYPE {} GRANULARITY {}", s[0], s[2], s[1], s[3])),
            });
        }
        let what = if kind == NodeKind::Dictionary { "DICTIONARY" } else { "TABLE" };
        let ddl = self
            .strings(&format!("SHOW CREATE {what} {}", ChDialect.table_sql(&d.object)), &[])
            .await?;
        d.ddl = ddl.into_iter().next().and_then(|r| r.into_iter().next());
        Ok(d)
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let o = ChOptions::parse("clickhouse://studio:p%40ss@db.local:9123/shop").unwrap();
        assert_eq!(
            (o.user.as_str(), o.password.as_deref(), o.host.as_str(), o.port, o.database.as_str()),
            ("studio", Some("p@ss"), "db.local", 9123, "shop")
        );
        assert_eq!(o.tls, TlsMode::Disable);
        let s = ChOptions::parse("https://ch.example.com/?user=bob&password=x&database=logs").unwrap();
        assert_eq!((s.port, s.user.as_str(), s.database.as_str()), (8443, "bob", "logs"));
        assert_eq!(s.tls, TlsMode::Verify { ca_file: None });
        let k = ChOptions::parse("clickhouse://h?secure=true&skip_verify=1").unwrap();
        assert_eq!((k.port, k.tls), (8443, TlsMode::SkipVerify));
        assert_eq!(ChOptions::parse("http://[::1]:8123").unwrap().host, "::1");
        assert!(ChOptions::parse("mysql://h").is_err());
        assert!(ChOptions::parse("clickhouse://h?bogus=1").is_err());
        assert_eq!(ChOptions::parse("http://[::1]:8123").unwrap().base_url(), "http://[::1]:8123/");
    }

    #[test]
    fn parses_server_errors() {
        let e = server_error("Code: 60. DB::Exception: Unknown table expression identifier 'nope'. (UNKNOWN_TABLE) (version 25.8.1.1)\n");
        assert_eq!(e.code.as_deref(), Some("60"));
        assert_eq!(e.message, "Unknown table expression identifier 'nope'. (UNKNOWN_TABLE)");
        assert_eq!(
            server_error("Code: 394. DB::Exception: Query was cancelled.").kind,
            ErrorKind::Cancelled
        );
        assert_eq!(server_error("Code: 159. DB::Exception: Timeout exceeded").kind, ErrorKind::Timeout);
    }

    #[test]
    fn syntax_errors_carry_a_position_and_move_the_expected_list_to_the_detail() {
        let text = "Code: 62. DB::Exception: Syntax error: failed at position 12 (FORM) (line 1, col 12): FORM t. Expected one of: FROM, WHERE. (SYNTAX_ERROR) (version 25.8.1.1)";
        let e = server_error(text);
        assert_eq!((e.code.as_deref(), e.position), (Some("62"), Some(11)));
        assert!(
            e.message.starts_with("Syntax error: failed at position 12") && e.message.ends_with("FORM t."),
            "{}",
            e.message
        );
        assert!(
            e.detail.as_deref().is_some_and(|d| d.starts_with("Expected one of: FROM")),
            "{:?}",
            e.detail
        );
    }

    #[test]
    fn a_failure_after_the_first_rows_is_an_error_not_a_row() {
        let body = b"[\"n\"]\n[\"UInt8\"]\n[0]\n__exception__\r\nCode: 159. DB::Exception: Timeout exceeded. (TIMEOUT_EXCEEDED)\n";
        let mut reader = RowReader::default();
        reader.feed(body, true).unwrap();
        assert_eq!(reader.next_row().unwrap(), Some(vec![Value::Int(0)]));
        let error = reader.next_row().unwrap_err();
        assert_eq!((error.code.as_deref(), error.kind), (Some("159"), ErrorKind::Timeout));
    }

    #[test]
    fn reads_rows_across_chunks() {
        let body = b"[\"id\",\"name\"]\n[\"UInt64\",\"Nullable(String)\"]\n[\"1\",\"a\"]\n[\"2\",null]\n";
        let mut r = RowReader::default();
        let mut rows = Vec::new();
        for (i, chunk) in body.chunks(7).enumerate() {
            r.feed(chunk, (i + 1) * 7 >= body.len()).unwrap();
            while let Some(row) = r.next_row().unwrap() {
                rows.push(row);
            }
        }
        assert_eq!(r.columns.len(), 2);
        assert!(r.columns[1].nullable);
        assert_eq!(
            rows,
            vec![vec![Value::Int(1), Value::Text("a".into())], vec![Value::Int(2), Value::Null]]
        );
        let mut bad = RowReader::default();
        bad.feed(b"[\"x\"]\n[\"UInt8\"]\nCode: 241. DB::Exception: Memory limit exceeded\n", true)
            .unwrap();
        assert_eq!(bad.next_row().unwrap_err().code.as_deref(), Some("241"));
    }

    #[test]
    fn placeholders_use_the_column_type() {
        let col = ColumnInfo::result("c", "LowCardinality(Nullable(String))", crate::engines::common::ValueKind::Text);
        assert_eq!(ChDialect.placeholder(2, &ParamTarget::Column(col)), "{p2:Nullable(String)}");
        assert_eq!(ChDialect.quote_ident("a`b"), "`a\\`b`");
    }
}
