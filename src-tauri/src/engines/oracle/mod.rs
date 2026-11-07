//! Oracle engine (the `oracle` crate on ODPI-C).
//!
//! ODPI-C loads Oracle Instant Client (OCI.dll) at runtime, the first time a connection is opened: the app builds and starts
//! without it, and connecting without it returns [`missing_client_error`] with the download link.
//!
//! The crate is synchronous, so every call runs on Tokio's blocking pool. Connections are thread-safe (ODPI-C), which is what
//! makes cancelling possible: `break_execution` is called from another thread while a statement runs. Sessions keep autocommit
//! off; Oracle opens a transaction implicitly on the first change, and the state is read from
//! `DBMS_TRANSACTION.LOCAL_TRANSACTION_ID`.

pub mod session;
#[cfg(test)]
mod tests;
pub mod types;

pub use session::OraSession;

use crate::engines::common::split::Dialect;
use crate::engines::common::sqlbuild::{self, ParamTarget, SqlDialect};
use crate::engines::common::value::percent_decode;
use crate::engines::common::{
    blocking, classify, BrowseRequest, CatalogNode, ChangeSummary, Classification, ColumnInfo, EngineError, ErrorKind, ForeignKeyInfo,
    IndexInfo, KeyInfo, NodeKind, ObjectDescription, ObjectRef, Page, Result, RowChange, TlsMode, Value, ValueKind,
};
use oracle::sql_type::ToSql;
use oracle::{Connection, Connector};
use std::sync::Arc;
use std::time::Duration;

pub const INSTANT_CLIENT_URL: &str = "https://www.oracle.com/database/technologies/instant-client/downloads.html";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OraOptions {
    pub host: String,
    pub port: u16,
    /// Service name (e.g. `FREEPDB1`, `ORCLPDB1`).
    pub service: String,
    pub user: String,
    pub password: Option<String>,
    /// TLS (`tcps`) uses the Instant Client's wallet / system configuration; `SkipVerify` is not supported by Oracle's client.
    pub tls: TlsMode,
    pub connect_timeout: Duration,
    /// A full connect descriptor or TNS alias, used instead of host/port/service when set.
    pub connect_string: Option<String>,
}

impl Default for OraOptions {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 1521,
            service: "FREEPDB1".into(),
            user: String::new(),
            password: None,
            tls: TlsMode::Disable,
            connect_timeout: Duration::from_secs(10),
            connect_string: None,
        }
    }
}

impl OraOptions {
    /// Parses `oracle://user:pass@host:1521/SERVICE`, SQL*Plus style `user/pass@//host:1521/SERVICE` or `user/pass@alias`, and
    /// EZConnect `host:1521/SERVICE`. A `(DESCRIPTION=…)` or anything else that is not EZConnect becomes `connect_string`.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        let mut o = Self::default();
        let target = if let Some(rest) = s.strip_prefix("oracle://") {
            let (auth, hostpart) = match rest.rsplit_once('@') {
                Some((a, h)) => (Some(a), h),
                None => (None, rest),
            };
            if let Some(a) = auth {
                let (u, p) = a.split_once(':').map(|(u, p)| (u, Some(p))).unwrap_or((a, None));
                o.user = percent_decode(u);
                o.password = p.map(percent_decode);
            }
            let (hostpart, query) = hostpart.split_once('?').unwrap_or((hostpart, ""));
            for pair in query.split('&').filter(|p| !p.is_empty()) {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                match k {
                    "ssl" | "tcps" if v == "true" || v == "1" => o.tls = TlsMode::Verify { ca_file: None },
                    "connect_timeout" => {
                        o.connect_timeout = Duration::from_secs(
                            v.parse()
                                .map_err(|_| EngineError::input("connect_timeout must be a number of seconds."))?,
                        )
                    }
                    other => return Err(EngineError::input(format!("Unknown Oracle connection option '{other}'."))),
                }
            }
            hostpart.to_owned()
        } else if let Some((auth, target)) = s.rsplit_once('@').filter(|(a, _)| !a.contains('(')) {
            let (u, p) = auth.split_once('/').map(|(u, p)| (u, Some(p))).unwrap_or((auth, None));
            o.user = u.to_owned();
            o.password = p.map(str::to_owned);
            target.to_owned()
        } else {
            s.to_owned()
        };
        let ez = target.strip_prefix("//").unwrap_or(&target);
        let ez = match ez.strip_prefix("tcps://") {
            Some(rest) => {
                o.tls = TlsMode::Verify { ca_file: None };
                rest
            }
            None => ez,
        };
        let looks_ez = !ez.contains('(') && ez.contains('/');
        if looks_ez {
            let (hostport, service) = ez.split_once('/').unwrap_or((ez, ""));
            match hostport.rsplit_once(':') {
                Some((h, p)) => {
                    o.host = h.to_owned();
                    o.port = p.parse().map_err(|_| EngineError::input(format!("'{p}' is not a valid port.")))?;
                }
                None => o.host = hostport.to_owned(),
            }
            o.service = service.to_owned();
        } else if !ez.is_empty() {
            o.connect_string = Some(ez.to_owned());
        }
        if o.user.is_empty() {
            return Err(EngineError::input("Enter the Oracle user name."));
        }
        Ok(o)
    }

    /// The connect string given to OCI: EZConnect Plus with the connect timeout, or the descriptor / alias as is.
    pub fn connect_string(&self) -> Result<String> {
        if let Some(cs) = &self.connect_string {
            return Ok(cs.clone());
        }
        let proto = match self.tls {
            TlsMode::Disable => "",
            TlsMode::Verify { .. } => "tcps://",
            TlsMode::SkipVerify => {
                return Err(EngineError::input(
                    "Oracle's client cannot skip certificate checks; add the server's certificate to a wallet and use verified TLS.",
                ))
            }
        };
        Ok(format!(
            "{proto}{}:{}/{}?connect_timeout={}",
            self.host,
            self.port,
            self.service,
            self.connect_timeout.as_secs().max(1)
        ))
    }

    fn open(&self) -> Result<Connection> {
        let cs = self.connect_string()?;
        let mut conn = Connector::new(&self.user, self.password.clone().unwrap_or_default(), &cs)
            .connect()
            .map_err(map_err)?;
        conn.set_autocommit(false);
        // Fixed formats so values convert implicitly the same way on every server.
        conn.execute(
            "ALTER SESSION SET NLS_NUMERIC_CHARACTERS = '.,' NLS_DATE_FORMAT = 'YYYY-MM-DD HH24:MI:SS' \
             NLS_TIMESTAMP_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF' NLS_TIMESTAMP_TZ_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF TZH:TZM'",
            &[],
        )
        .map_err(map_err)?;
        Ok(conn)
    }
}

/// The error for a machine without Oracle Instant Client.
pub fn missing_client_error(detail: &str) -> EngineError {
    EngineError::connect(format!(
        "Oracle Instant Client was not found. Install Instant Client 19c or newer for 64-bit Windows (Basic or Basic Light) from \
         {INSTANT_CLIENT_URL}, add its folder to PATH, and reconnect. ({detail})"
    ))
}

pub(crate) fn map_err(e: oracle::Error) -> EngineError {
    let text = e.to_string();
    if text.contains("DPI-1047") {
        return missing_client_error(text.lines().next().unwrap_or(&text));
    }
    if let Some(db) = e.db_error() {
        let code = format!("ORA-{:05}", db.code());
        let message = db.message().trim().to_owned();
        let kind = match db.code() {
            1013 => return EngineError::cancelled(),
            1017 | 12541 | 12514 | 12505 | 12170 | 28000 | 28001 => ErrorKind::Connect,
            _ => ErrorKind::Server,
        };
        return EngineError {
            message,
            code: Some(code),
            kind,
        };
    }
    if text.contains("DPI-1080") || text.contains("DPI-1010") {
        return EngineError::connect(format!("The connection to the server was lost: {text}"));
    }
    EngineError::server(text, None)
}

/// Identifier quoting, `:n` placeholders and conversions for Oracle.
pub struct OraDialect;

fn is_lob(col: &ColumnInfo) -> bool {
    matches!(col.type_name.as_str(), "CLOB" | "NCLOB" | "BLOB")
}

impl SqlDialect for OraDialect {
    fn quote_ident(&self, name: &str) -> String {
        sqlbuild::quote_with(name, '"')
    }
    fn placeholder(&self, n: usize, target: &ParamTarget) -> String {
        match target {
            ParamTarget::Column(col) => match col.type_name.as_str() {
                t if t.starts_with("INTERVAL DAY") => format!("TO_DSINTERVAL(:{n})"),
                t if t.starts_with("INTERVAL YEAR") => format!("TO_YMINTERVAL(:{n})"),
                "BOOLEAN" => format!("TO_BOOLEAN(:{n})"),
                "JSON" => format!("JSON(:{n})"),
                _ => format!(":{n}"),
            },
            ParamTarget::Text => format!(":{n}"),
        }
    }
    fn select_expr(&self, col: &ColumnInfo) -> String {
        let q = self.quote_ident(&col.name);
        match col.type_name.as_str() {
            "JSON" => format!("JSON_SERIALIZE({q}) AS {q}"),
            "XMLTYPE" => format!("XMLSERIALIZE(CONTENT {q}) AS {q}"),
            _ => q,
        }
    }
    fn text_expr(&self, col_sql: &str, col: &ColumnInfo) -> String {
        if is_lob(col) {
            format!("DBMS_LOB.SUBSTR({col_sql}, 4000)")
        } else if col.kind == ValueKind::Text {
            col_sql.to_owned()
        } else {
            format!("TO_CHAR({col_sql})")
        }
    }
    fn like(&self, lhs: &str, rhs: &str, ci: bool, negate: bool) -> String {
        let not = if negate { "NOT " } else { "" };
        if ci {
            format!("UPPER({lhs}) {not}LIKE UPPER({rhs}) ESCAPE '\\'")
        } else {
            format!("{lhs} {not}LIKE {rhs} ESCAPE '\\'")
        }
    }
    fn limit_offset(&self, limit: u64, offset: u64) -> String {
        format!("OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
    }
}

fn binds(params: &[sqlbuild::Param]) -> Vec<Box<dyn ToSql>> {
    params.iter().map(|p| types::to_bind(&p.value)).collect()
}

fn refs(b: &[Box<dyn ToSql>]) -> Vec<&dyn ToSql> {
    b.iter().map(|x| x.as_ref()).collect()
}

/// Runs a query and returns its rows as display strings (catalog queries).
fn strings(c: &Connection, sql: &str, params: &[&dyn ToSql]) -> Result<Vec<Vec<Option<String>>>> {
    let rows = c.query(sql, params).map_err(map_err)?;
    let mut out = Vec::new();
    for row in rows {
        let row = row.map_err(map_err)?;
        out.push(row.sql_values().iter().map(|v| types::read(v).display()).collect());
    }
    Ok(out)
}

/// An open Oracle database.
pub struct OraEngine {
    opts: OraOptions,
    conn: Arc<Connection>,
    version: String,
    user: String,
}

impl OraEngine {
    pub async fn connect(opts: OraOptions) -> Result<Self> {
        let o = opts.clone();
        let (conn, version, user) = blocking(move || {
            let conn = o.open()?;
            let (v, _banner) = conn.server_version().map_err(map_err)?;
            let user = strings(&conn, "SELECT USER FROM dual", &[])?
                .into_iter()
                .next()
                .and_then(|r| r.into_iter().next().flatten());
            Ok((conn, v.to_string(), user.unwrap_or_default()))
        })
        .await?;
        Ok(Self {
            opts,
            conn: Arc::new(conn),
            version,
            user,
        })
    }

    pub fn server_version(&self) -> &str {
        &self.version
    }

    /// The schema the user logged in to.
    pub fn current_schema(&self) -> &str {
        &self.user
    }

    pub fn classify(sql: &str) -> Classification {
        classify(Dialect::Oracle, sql)
    }

    pub async fn open_session(&self) -> Result<OraSession> {
        let o = self.opts.clone();
        let conn = blocking(move || o.open()).await?;
        Ok(OraSession::new(conn))
    }

    async fn with_conn<T: Send + 'static>(&self, f: impl FnOnce(&Connection) -> Result<T> + Send + 'static) -> Result<T> {
        let conn = self.conn.clone();
        blocking(move || f(&conn)).await
    }

    /// Schemas (users that own objects and are not Oracle-maintained, the login schema first) with tables, views, materialized
    /// views, sequences, packages, procedures, functions and synonyms.
    pub async fn catalog(&self) -> Result<Vec<CatalogNode>> {
        let me = self.user.clone();
        self.with_conn(move |c| {
            let owners = strings(
                c,
                "SELECT username FROM all_users WHERE oracle_maintained = 'N' OR username = USER \
                 ORDER BY CASE WHEN username = USER THEN 0 ELSE 1 END, username",
                &[],
            )?;
            let objects = strings(
                c,
                "SELECT o.owner, o.object_name, o.object_type FROM all_objects o JOIN all_users u ON u.username = o.owner \
                 WHERE (u.oracle_maintained = 'N' OR o.owner = USER) \
                 AND o.object_type IN ('TABLE', 'VIEW', 'MATERIALIZED VIEW', 'SEQUENCE', 'PACKAGE', 'PROCEDURE', 'FUNCTION', 'SYNONYM') \
                 AND o.object_name NOT LIKE 'BIN$%' AND o.generated = 'N' \
                 AND NOT (o.object_type = 'TABLE' AND EXISTS (SELECT 1 FROM all_mviews m WHERE m.owner = o.owner AND m.mview_name = o.object_name)) \
                 ORDER BY o.owner, o.object_name",
                &[],
            )?;
            const LABELS: [&str; 8] = ["Tables", "Views", "Materialized Views", "Sequences", "Packages", "Procedures", "Functions", "Synonyms"];
            let mut out = Vec::new();
            for owner in owners.into_iter().filter_map(|r| r.into_iter().next().flatten()) {
                let mut folders: [Vec<CatalogNode>; 8] = Default::default();
                for o in objects.iter().filter(|o| o[0].as_deref() == Some(owner.as_str())) {
                    let (Some(name), Some(ty)) = (&o[1], &o[2]) else { continue };
                    let (i, kind) = match ty.as_str() {
                        "TABLE" => (0, NodeKind::Table),
                        "VIEW" => (1, NodeKind::View),
                        "MATERIALIZED VIEW" => (2, NodeKind::MaterializedView),
                        "SEQUENCE" => (3, NodeKind::Sequence),
                        "PACKAGE" => (4, NodeKind::Package),
                        "PROCEDURE" => (5, NodeKind::Procedure),
                        "FUNCTION" => (6, NodeKind::Function),
                        _ => (7, NodeKind::Synonym),
                    };
                    folders[i].push(CatalogNode::object(ObjectRef::new(None, Some(&owner), name, kind), None));
                }
                if owner != me && folders.iter().all(Vec::is_empty) {
                    continue;
                }
                let kids = folders.into_iter().zip(LABELS).map(|(items, l)| CatalogNode::container(NodeKind::Folder, l, items)).collect();
                let mut node = CatalogNode::container(NodeKind::Schema, &owner, kids);
                node.object = Some(ObjectRef::new(None, Some(&owner), &owner, NodeKind::Schema));
                out.push(node);
            }
            Ok(out)
        })
        .await
    }

    pub async fn describe(&self, obj: &ObjectRef) -> Result<ObjectDescription> {
        let obj = obj.clone();
        let me = self.user.clone();
        self.with_conn(move |c| describe_sync(c, &obj, &me)).await
    }

    pub async fn browse(&self, req: &BrowseRequest) -> Result<Page> {
        let req = req.clone();
        let me = self.user.clone();
        self.with_conn(move |c| {
            let d = describe_sync(c, &req.object, &me)?;
            let key = d.primary_key.as_ref().map(|k| k.columns.clone()).unwrap_or_default();
            let mut req = req.clone();
            req.object = d.object.clone();
            let built = sqlbuild::build_browse(&OraDialect, &req, &d.columns, &key)?;
            let b = binds(&built.params);
            let rows = c.query(&built.sql, &refs(&b)).map_err(map_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(map_err)?.sql_values().iter().map(types::read).collect());
            }
            let estimated_total = if !req.estimate_total {
                None
            } else if built.where_sql.is_empty() {
                d.row_estimate
            } else {
                estimate(c, &d.object, &built, req.after.as_ref().map_or(0, Vec::len))
            };
            Ok(sqlbuild::finish_page(d.columns, out, req.limit, &built, key, estimated_total))
        })
        .await
    }

    /// Applies all changes in one transaction; an update or delete that does not hit exactly one row rolls everything back.
    pub async fn apply_changes(&self, obj: &ObjectRef, changes: Vec<RowChange>) -> Result<ChangeSummary> {
        let obj = obj.clone();
        let me = self.user.clone();
        self.with_conn(move |c| {
            let d = describe_sync(c, &obj, &me)?;
            let result = (|| {
                let mut summary = ChangeSummary::default();
                for ch in &changes {
                    let (built, expect_one) = match ch {
                        RowChange::Insert { values } => (sqlbuild::build_insert(&OraDialect, &d.object, values, &d.columns)?, false),
                        RowChange::Update { key, values } => {
                            (sqlbuild::build_update(&OraDialect, &d.object, key, values, &d.columns)?, true)
                        }
                        RowChange::Delete { key } => (sqlbuild::build_delete(&OraDialect, &d.object, key, &d.columns)?, true),
                    };
                    let b = binds(&built.params);
                    let stmt = c.execute(&built.sql, &refs(&b)).map_err(map_err)?;
                    let n = stmt.row_count().map_err(map_err)?;
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
                Ok(summary)
            })();
            match result {
                Ok(s) => {
                    c.commit().map_err(map_err)?;
                    Ok(s)
                }
                Err(e) => {
                    let _ = c.rollback();
                    Err(e)
                }
            }
        })
        .await
    }
}

/// The optimizer's cardinality for the filter (EXPLAIN PLAN into the session's PLAN_TABLE).
fn estimate(c: &Connection, obj: &ObjectRef, built: &sqlbuild::BrowseSql, keyset_len: usize) -> Option<i64> {
    let n = built.params.len() - keyset_len * (keyset_len + 1) / 2;
    let id = format!("fosta{}", std::process::id());
    let sql = format!(
        "EXPLAIN PLAN SET STATEMENT_ID = '{id}' FOR SELECT 1 FROM {} WHERE {}",
        OraDialect.table_sql(obj),
        built.where_sql
    );
    let b = binds(&built.params[..n]);
    c.execute(&sql, &refs(&b)).ok()?;
    let rows = strings(c, "SELECT cardinality FROM plan_table WHERE statement_id = :1 AND id = 0", &[&id]).ok()?;
    let _ = c.execute("DELETE FROM plan_table WHERE statement_id = :1", &[&id]);
    let _ = c.commit();
    rows.into_iter().next()?.into_iter().next().flatten()?.parse().ok()
}

fn one(c: &Connection, sql: &str, params: &[&dyn ToSql]) -> Result<Option<Vec<Option<String>>>> {
    Ok(strings(c, sql, params)?.into_iter().next())
}

fn describe_sync(c: &Connection, obj: &ObjectRef, me: &str) -> Result<ObjectDescription> {
    let owner = obj.schema.clone().unwrap_or_else(|| me.to_owned());
    let found = one(
        c,
        "SELECT object_type FROM all_objects WHERE owner = :1 AND object_name = :2 \
         AND object_type IN ('TABLE','VIEW','MATERIALIZED VIEW','SEQUENCE','PACKAGE','PROCEDURE','FUNCTION','SYNONYM') \
         ORDER BY CASE object_type WHEN 'MATERIALIZED VIEW' THEN 0 ELSE 1 END",
        &[&owner, &obj.name],
    )?
    .and_then(|r| r.into_iter().next().flatten())
    .ok_or_else(|| EngineError::input(format!("There is no object named '{}' in schema '{owner}'.", obj.name)))?;
    let kind = match found.as_str() {
        "VIEW" => NodeKind::View,
        "MATERIALIZED VIEW" => NodeKind::MaterializedView,
        "SEQUENCE" => NodeKind::Sequence,
        "PACKAGE" => NodeKind::Package,
        "PROCEDURE" => NodeKind::Procedure,
        "FUNCTION" => NodeKind::Function,
        "SYNONYM" => NodeKind::Synonym,
        _ => NodeKind::Table,
    };
    let mut d = ObjectDescription::new(ObjectRef::new(None, Some(&owner), &obj.name, kind));
    if kind.is_relation() {
        describe_relation(c, &mut d, &owner)?;
    }
    if kind == NodeKind::Synonym {
        if let Some(r) = one(
            c,
            "SELECT table_owner, table_name, db_link FROM all_synonyms WHERE owner = :1 AND synonym_name = :2",
            &[&owner, &obj.name],
        )? {
            d.extra.push((
                "target".into(),
                format!("{}.{}", r[0].clone().unwrap_or_default(), r[1].clone().unwrap_or_default()),
            ));
        }
    }
    if kind == NodeKind::Sequence {
        if let Some(r) = one(
            c,
            "SELECT TO_CHAR(min_value), TO_CHAR(max_value), TO_CHAR(increment_by), cycle_flag, TO_CHAR(cache_size), TO_CHAR(last_number) \
             FROM all_sequences WHERE sequence_owner = :1 AND sequence_name = :2",
            &[&owner, &obj.name],
        )? {
            for (k, v) in ["minValue", "maxValue", "incrementBy", "cycle", "cacheSize", "lastNumber"]
                .iter()
                .zip(r)
            {
                d.extra.push(((*k).into(), v.unwrap_or_default()));
            }
        }
    }
    d.ddl = ddl(c, &owner, &obj.name, kind, &found);
    if d.ddl.is_none() {
        d.extra.push((
            "ddl".into(),
            "DBMS_METADATA could not read this object's DDL (it needs SELECT_CATALOG_ROLE for other schemas).".into(),
        ));
    }
    Ok(d)
}

fn describe_relation(c: &Connection, d: &mut ObjectDescription, owner: &str) -> Result<()> {
    let name = d.object.name.clone();
    let cols = strings(
        c,
        "SELECT c.column_name, c.data_type, TO_CHAR(c.data_precision), TO_CHAR(c.data_scale), TO_CHAR(c.char_length), c.char_used, \
                TO_CHAR(c.data_length), c.nullable, c.data_default_vc, c.identity_column, c.virtual_column, cc.comments, \
                (SELECT i.generation_type FROM all_tab_identity_cols i WHERE i.owner = c.owner AND i.table_name = c.table_name AND i.column_name = c.column_name) \
         FROM all_tab_cols c LEFT JOIN all_col_comments cc ON cc.owner = c.owner AND cc.table_name = c.table_name AND cc.column_name = c.column_name \
         WHERE c.owner = :1 AND c.table_name = :2 AND c.hidden_column = 'NO' ORDER BY c.column_id",
        &[&owner, &name],
    )?;
    for r in cols {
        let g = |i: usize| r[i].clone().unwrap_or_default();
        let (dt, prec, scale) = (
            g(1),
            r[2].as_deref().and_then(|s| s.parse::<i64>().ok()),
            r[3].as_deref().and_then(|s| s.parse::<i64>().ok()),
        );
        let type_name = match dt.as_str() {
            "NUMBER" => match (prec, scale) {
                (Some(p), Some(0)) => format!("NUMBER({p})"),
                (Some(p), Some(s)) => format!("NUMBER({p},{s})"),
                (None, Some(0)) => "INTEGER".to_owned(),
                _ => "NUMBER".to_owned(),
            },
            "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" => {
                let unit = if g(5) == "C" && !dt.starts_with('N') { " CHAR" } else { "" };
                format!("{dt}({}{unit})", g(4))
            }
            "RAW" => format!("RAW({})", g(6)),
            "FLOAT" => format!("FLOAT({})", prec.unwrap_or(126)),
            _ => dt.clone(),
        };
        let virt = g(10) == "YES";
        let default = r[8].clone().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        d.columns.push(ColumnInfo {
            kind: types::kind_by_name(&dt, prec, scale),
            name: g(0),
            type_name,
            nullable: g(7) == "Y",
            default: if virt || g(9) == "YES" { None } else { default.clone() },
            comment: r[11].clone(),
            identity: r[12].clone(),
            generated: if virt { default } else { None },
        });
    }
    if let Some(r) = one(
        c,
        "SELECT comments FROM all_tab_comments WHERE owner = :1 AND table_name = :2",
        &[&owner, &name],
    )? {
        d.comment = r[0].clone();
    }
    if let Some(r) = one(
        c,
        "SELECT TO_CHAR(num_rows) FROM all_tables WHERE owner = :1 AND table_name = :2",
        &[&owner, &name],
    )? {
        d.row_estimate = r[0].as_deref().and_then(|s| s.parse().ok());
    }

    // Constraints with their columns in order.
    let cons = strings(
        c,
        "SELECT k.constraint_name, k.constraint_type, k.delete_rule, r.owner, r.table_name, \
                LISTAGG(cc.column_name, ',') WITHIN GROUP (ORDER BY cc.position), \
                (SELECT LISTAGG(rc.column_name, ',') WITHIN GROUP (ORDER BY rc.position) FROM all_cons_columns rc \
                  WHERE rc.owner = r.owner AND rc.constraint_name = r.constraint_name) \
         FROM all_constraints k JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name \
         LEFT JOIN all_constraints r ON r.owner = k.r_owner AND r.constraint_name = k.r_constraint_name \
         WHERE k.owner = :1 AND k.table_name = :2 AND k.constraint_type IN ('P', 'U', 'R') \
         GROUP BY k.constraint_name, k.constraint_type, k.delete_rule, r.owner, r.table_name, r.constraint_name ORDER BY k.constraint_type, k.constraint_name",
        &[&owner, &name],
    )?;
    let split = |s: &Option<String>| {
        s.as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for r in &cons {
        let cname = r[0].clone();
        match r[1].as_deref() {
            Some("P") => {
                d.primary_key = Some(KeyInfo {
                    name: cname,
                    columns: split(&r[5]),
                })
            }
            Some("U") => d.unique_keys.push(KeyInfo {
                name: cname,
                columns: split(&r[5]),
            }),
            _ => d.foreign_keys.push(ForeignKeyInfo {
                name: cname,
                columns: split(&r[5]),
                ref_schema: r[3].clone(),
                ref_table: r[4].clone().unwrap_or_default(),
                ref_columns: split(&r[6]),
                on_update: None,
                on_delete: r[2].clone(),
            }),
        }
    }

    // Indexes; descending and function-based columns show their expression.
    let idx = strings(
        c,
        "SELECT i.index_name, i.uniqueness, i.index_type, ic.column_name, ic.descend, TO_CHAR(ic.column_position), \
                (SELECT 'P' FROM all_constraints k WHERE k.owner = i.table_owner AND k.index_name = i.index_name AND k.constraint_type = 'P' AND ROWNUM = 1) \
         FROM all_indexes i JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
         WHERE i.table_owner = :1 AND i.table_name = :2 AND i.index_type <> 'LOB' ORDER BY i.index_name, ic.column_position",
        &[&owner, &name],
    )?;
    let exprs = strings(
        c,
        "SELECT index_name, TO_CHAR(column_position), column_expression FROM all_ind_expressions WHERE table_owner = :1 AND table_name = :2",
        &[&owner, &name],
    )
    .unwrap_or_default();
    for r in idx {
        let iname = r[0].clone().unwrap_or_default();
        let mut col = r[3].clone().unwrap_or_default();
        if let Some(e) = exprs.iter().find(|e| e[0].as_deref() == Some(iname.as_str()) && e[1] == r[5]) {
            col = e[2].clone().unwrap_or(col).trim_matches('"').to_owned();
        }
        if r[4].as_deref() == Some("DESC") {
            col.push_str(" DESC");
        }
        match d.indexes.iter_mut().find(|i| i.name == iname) {
            Some(i) => i.columns.push(col),
            None => d.indexes.push(IndexInfo {
                name: iname,
                columns: vec![col],
                unique: r[1].as_deref() == Some("UNIQUE"),
                primary: r[6].is_some(),
                method: r[2].clone(),
                definition: None,
            }),
        }
    }
    Ok(())
}

/// DDL from DBMS_METADATA (works for the user's own objects; other schemas need SELECT_CATALOG_ROLE), with ALL_VIEWS and
/// ALL_SOURCE as fallbacks.
fn ddl(c: &Connection, owner: &str, name: &str, kind: NodeKind, object_type: &str) -> Option<String> {
    let md_type = match kind {
        NodeKind::MaterializedView => "MATERIALIZED_VIEW".to_owned(),
        _ => object_type.replace(' ', "_"),
    };
    let _ = c.execute(
        "BEGIN DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'SQLTERMINATOR', TRUE); \
         DBMS_METADATA.SET_TRANSFORM_PARAM(DBMS_METADATA.SESSION_TRANSFORM, 'SEGMENT_ATTRIBUTES', FALSE); END;",
        &[],
    );
    let mut text = one(c, "SELECT DBMS_METADATA.GET_DDL(:1, :2, :3) FROM dual", &[&md_type, &name, &owner])
        .ok()
        .flatten()
        .and_then(|r| r.into_iter().next().flatten());
    if let (Some(t), NodeKind::Table | NodeKind::MaterializedView) = (&mut text, kind) {
        for dep in ["INDEX", "COMMENT"] {
            if let Ok(Some(r)) = one(
                c,
                "SELECT DBMS_METADATA.GET_DEPENDENT_DDL(:1, :2, :3) FROM dual",
                &[&dep, &name, &owner],
            ) {
                if let Some(extra) = r.into_iter().next().flatten() {
                    t.push_str(&extra);
                }
            }
        }
    }
    if text.is_none() {
        text = match kind {
            NodeKind::View => one(
                c,
                "SELECT text_vc FROM all_views WHERE owner = :1 AND view_name = :2",
                &[&owner, &name],
            )
            .ok()
            .flatten()
            .and_then(|r| r.into_iter().next().flatten())
            .map(|t| format!("CREATE OR REPLACE VIEW \"{owner}\".\"{name}\" AS\n{t};")),
            NodeKind::Package | NodeKind::Procedure | NodeKind::Function => {
                let lines = strings(
                    c,
                    "SELECT text FROM all_source WHERE owner = :1 AND name = :2 ORDER BY CASE type WHEN 'PACKAGE BODY' THEN 1 ELSE 0 END, line",
                    &[&owner, &name],
                )
                .ok()?;
                let body: String = lines.into_iter().filter_map(|r| r.into_iter().next().flatten()).collect();
                (!body.is_empty()).then(|| format!("CREATE OR REPLACE {body}"))
            }
            _ => None,
        };
    }
    text.map(|t| t.trim().to_owned())
}

/// Reads one row of a result set.
pub(crate) fn row_values(row: &oracle::Row) -> Vec<Value> {
    row.sql_values().iter().map(types::read).collect()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parses_connection_strings() {
        let o = OraOptions::parse("oracle://shop:p%40ss@db.example.com:1522/ORCLPDB1?connect_timeout=3").unwrap();
        assert_eq!(
            (o.user.as_str(), o.password.as_deref(), o.host.as_str(), o.port, o.service.as_str()),
            ("shop", Some("p@ss"), "db.example.com", 1522, "ORCLPDB1")
        );
        assert_eq!(o.connect_string().unwrap(), "db.example.com:1522/ORCLPDB1?connect_timeout=3");
        let s = OraOptions::parse("scott/tiger@//localhost:1521/FREEPDB1").unwrap();
        assert_eq!(
            (s.user.as_str(), s.password.as_deref(), s.service.as_str()),
            ("scott", Some("tiger"), "FREEPDB1")
        );
        let alias = OraOptions::parse("scott/tiger@PRODDB").unwrap();
        assert_eq!(alias.connect_string().unwrap(), "PRODDB");
        let desc =
            OraOptions::parse("scott/tiger@(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=h)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=s)))")
                .unwrap();
        assert!(desc.connect_string().unwrap().starts_with("(DESCRIPTION="));
        let tls = OraOptions::parse("scott@tcps://h:2484/S").unwrap();
        assert!(tls.connect_string().unwrap().starts_with("tcps://h:2484/S"));
        let mut skip = tls.clone();
        skip.tls = TlsMode::SkipVerify;
        assert!(skip.connect_string().is_err());
        assert!(OraOptions::parse("localhost:1521/FREEPDB1").is_err(), "a user is required");
    }

    #[test]
    fn dialect_placeholders_and_paging() {
        let mut c = ColumnInfo::result("d", "INTERVAL DAY(2) TO SECOND(6)", ValueKind::Interval);
        assert_eq!(OraDialect.placeholder(2, &ParamTarget::Column(c.clone())), "TO_DSINTERVAL(:2)");
        c.type_name = "NUMBER(10)".into();
        assert_eq!(OraDialect.placeholder(1, &ParamTarget::Column(c)), ":1");
        assert_eq!(OraDialect.limit_offset(51, 100), "OFFSET 100 ROWS FETCH NEXT 51 ROWS ONLY");
        assert_eq!(
            OraDialect.like("\"N\"", ":1", true, false),
            "UPPER(\"N\") LIKE UPPER(:1) ESCAPE '\\'"
        );
    }
}
