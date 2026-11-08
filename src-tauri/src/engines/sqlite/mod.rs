//! SQLite engine (rusqlite with the bundled SQLite; extension loading is not compiled in).
//!
//! rusqlite is synchronous, so every call runs on Tokio's blocking pool; the engine keeps one connection for catalog, browse and
//! edits, and each session opens its own connection to the same file so `BEGIN … COMMIT` can span runs. Cancelling uses
//! SQLite's interrupt handle; the statement timeout is a watchdog that interrupts a statement once it runs too long.

pub mod adapter;
pub mod descriptor;
pub mod session;
#[cfg(test)]
mod tests;
pub mod types;

pub use adapter::{SqliteAdapter, SQLITE};
pub use session::SqliteSession;

use crate::engines::common::split::Dialect;
use crate::engines::common::sqlbuild::{self, ParamTarget, SqlDialect};
use crate::engines::common::value::percent_decode;
use crate::engines::common::{
    blocking, classify, BrowseRequest, CatalogNode, ChangeSummary, Classification, ColumnInfo, EngineError, ForeignKeyInfo, IndexInfo,
    KeyInfo, NodeKind, ObjectDescription, ObjectRef, Page, Result, RowChange, Value, ValueKind,
};
use rusqlite::{params_from_iter, Connection, OpenFlags};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The rowid pseudo-column added to browse results of rowid tables without a primary key, so their rows can be edited.
pub const ROWID: &str = "_rowid_";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteOptions {
    /// A file path, or `:memory:`.
    pub path: String,
    pub read_only: bool,
    /// Create the file when it does not exist.
    pub create: bool,
    /// How long to wait for a lock held by another connection.
    pub busy_timeout: Duration,
}

impl SqliteOptions {
    pub fn file(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            read_only: false,
            create: false,
            busy_timeout: Duration::from_secs(5),
        }
    }

    /// Parses `sqlite:///abs/path.db`, `sqlite://rel.db`, `sqlite:path.db`, `file:path.db?mode=ro` or a plain path. `mode` may be
    /// `ro`, `rw` or `rwc` (create); `:memory:` opens a private in-memory database.
    pub fn from_uri(uri: &str) -> Result<Self> {
        let s = uri.trim();
        if s.is_empty() {
            return Err(EngineError::input("Enter the path of the SQLite database file."));
        }
        let rest = s
            .strip_prefix("sqlite://")
            .or_else(|| s.strip_prefix("sqlite:"))
            .or_else(|| s.strip_prefix("file://"))
            .or_else(|| s.strip_prefix("file:"))
            .unwrap_or(s);
        let (path, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };
        // sqlite:///C:/x.db and file:///C:/x.db keep a leading slash before the drive letter.
        let path = match path.as_bytes() {
            [b'/', d, b':', ..] if d.is_ascii_alphabetic() => &path[1..],
            _ => path,
        };
        let mut opts = Self::file(&percent_decode(path));
        for pair in query.unwrap_or("").split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match (k, v) {
                ("mode", "ro") => opts.read_only = true,
                ("mode", "rw") => {}
                ("mode", "rwc") => opts.create = true,
                ("mode", "memory") => opts.path = ":memory:".into(),
                ("busy_timeout", ms) => {
                    let ms: u64 = ms
                        .parse()
                        .map_err(|_| EngineError::input("busy_timeout must be a number of milliseconds."))?;
                    opts.busy_timeout = Duration::from_millis(ms);
                }
                _ => return Err(EngineError::input(format!("Unknown SQLite connection option '{k}'."))),
            }
        }
        if opts.path.is_empty() {
            return Err(EngineError::input("Enter the path of the SQLite database file."));
        }
        Ok(opts)
    }

    fn open(&self) -> Result<Connection> {
        let mut flags = OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        flags |= if self.read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else if self.create {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        if self.path != ":memory:" && !self.create && !PathBuf::from(&self.path).exists() {
            return Err(EngineError::connect(format!(
                "The file '{}' does not exist. Check the path, or allow creating it.",
                self.path
            )));
        }
        let conn = Connection::open_with_flags(&self.path, flags)
            .map_err(|e| EngineError::connect(format!("Could not open '{}': {e}.", self.path)))?;
        conn.busy_timeout(self.busy_timeout).map_err(map_err)?;
        // Foreign keys are off by default in SQLite; a client should enforce what the schema declares.
        conn.execute_batch("PRAGMA foreign_keys = ON").map_err(map_err)?;
        Ok(conn)
    }
}

pub(crate) fn map_err(e: rusqlite::Error) -> EngineError {
    match &e {
        rusqlite::Error::SqliteFailure(f, msg) => {
            if f.code == rusqlite::ErrorCode::OperationInterrupted {
                return EngineError::cancelled();
            }
            let text = msg.clone().unwrap_or_else(|| f.to_string());
            EngineError::server(text, Some(f.extended_code.to_string()))
        }
        _ => EngineError::server(e.to_string(), None),
    }
}

/// Identifier quoting, placeholders and LIKE for SQLite.
pub struct SqliteDialect;

impl SqlDialect for SqliteDialect {
    fn quote_ident(&self, name: &str) -> String {
        if name == ROWID {
            // Unquoted, `_rowid_` is the rowid unless a real column has that name (then it is that column): right either way.
            return ROWID.to_owned();
        }
        sqlbuild::quote_with(name, '"')
    }
    fn placeholder(&self, n: usize, _target: &ParamTarget) -> String {
        format!("?{n}")
    }
    fn text_expr(&self, col_sql: &str, _col: &ColumnInfo) -> String {
        format!("CAST({col_sql} AS TEXT)")
    }
    /// SQLite's LIKE ignores ASCII case either way; GLOB would change the pattern syntax, so `ci` makes no difference.
    fn like(&self, lhs: &str, rhs: &str, _ci: bool, negate: bool) -> String {
        let not = if negate { "NOT " } else { "" };
        format!("{lhs} {not}LIKE {rhs} ESCAPE '\\'")
    }
}

/// An open SQLite database.
pub struct SqliteEngine {
    opts: SqliteOptions,
    conn: Arc<Mutex<Connection>>,
    version: String,
}

fn lock(conn: &Mutex<Connection>) -> std::sync::MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(|p| p.into_inner())
}

impl SqliteEngine {
    pub async fn connect(opts: SqliteOptions) -> Result<Self> {
        let o = opts.clone();
        let conn = blocking(move || o.open()).await?;
        Ok(Self {
            opts,
            conn: Arc::new(Mutex::new(conn)),
            version: rusqlite::version().to_owned(),
        })
    }

    pub fn server_version(&self) -> &str {
        &self.version
    }

    pub fn options(&self) -> &SqliteOptions {
        &self.opts
    }

    pub fn classify(sql: &str) -> Classification {
        classify(Dialect::Sqlite, sql)
    }

    /// Opens a session on its own connection.
    pub async fn open_session(&self) -> Result<SqliteSession> {
        let o = self.opts.clone();
        let conn = blocking(move || o.open()).await?;
        Ok(SqliteSession::new(conn))
    }

    async fn with_conn<T: Send + 'static>(&self, f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static) -> Result<T> {
        let conn = self.conn.clone();
        blocking(move || f(&mut lock(&conn))).await
    }

    /// Attached databases (`main`, `temp` when used, `ATTACH`ed files) with their tables, views, indexes and triggers.
    pub async fn catalog(&self) -> Result<Vec<CatalogNode>> {
        self.with_conn(|c| catalog_sync(c)).await
    }

    pub async fn describe(&self, obj: &ObjectRef) -> Result<ObjectDescription> {
        let obj = obj.clone();
        self.with_conn(move |c| describe_sync(c, &obj)).await
    }

    pub async fn browse(&self, req: &BrowseRequest) -> Result<Page> {
        let req = req.clone();
        self.with_conn(move |c| browse_sync(c, &req)).await
    }

    /// Applies all changes in one transaction; an update or delete that does not hit exactly one row rolls everything back.
    pub async fn apply_changes(&self, obj: &ObjectRef, changes: Vec<RowChange>) -> Result<ChangeSummary> {
        let obj = obj.clone();
        self.with_conn(move |c| apply_sync(c, &obj, &changes)).await
    }
}

fn schema_of(obj: &ObjectRef) -> &str {
    obj.schema.as_deref().unwrap_or("main")
}

fn catalog_sync(c: &Connection) -> Result<Vec<CatalogNode>> {
    let mut dbs: Vec<String> = Vec::new();
    {
        let mut st = c.prepare("SELECT name FROM pragma_database_list ORDER BY seq").map_err(map_err)?;
        let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(map_err)?;
        for r in rows {
            dbs.push(r.map_err(map_err)?);
        }
    }
    let mut out = Vec::new();
    for db in dbs {
        let sql = format!(
            "SELECT type, name, tbl_name FROM {}.sqlite_schema WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY type, name",
            SqliteDialect.quote_ident(&db)
        );
        let mut st = c.prepare(&sql).map_err(map_err)?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
            .map_err(map_err)?;
        let (mut tables, mut views, mut indexes, mut triggers) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for r in rows {
            let (ty, name, tbl) = r.map_err(map_err)?;
            let (kind, bucket, detail) = match ty.as_str() {
                "table" => (NodeKind::Table, &mut tables, None),
                "view" => (NodeKind::View, &mut views, None),
                "index" => (NodeKind::Index, &mut indexes, Some(format!("on {tbl}"))),
                "trigger" => (NodeKind::Trigger, &mut triggers, Some(format!("on {tbl}"))),
                _ => continue,
            };
            bucket.push(CatalogNode::object(ObjectRef::new(None, Some(&db), &name, kind), detail));
        }
        let mut node = CatalogNode::container(
            NodeKind::Database,
            &db,
            vec![
                CatalogNode::container(NodeKind::Folder, "Tables", tables),
                CatalogNode::container(NodeKind::Folder, "Views", views),
                CatalogNode::container(NodeKind::Folder, "Indexes", indexes),
                CatalogNode::container(NodeKind::Folder, "Triggers", triggers),
            ],
        );
        node.object = Some(ObjectRef::new(None, Some(&db), &db, NodeKind::Database));
        out.push(node);
    }
    Ok(out)
}

fn object_sql(c: &Connection, schema: &str, name: &str) -> Result<Option<(String, Option<String>)>> {
    let sql = format!(
        "SELECT type, sql FROM {}.sqlite_schema WHERE name = ?1",
        SqliteDialect.quote_ident(schema)
    );
    let mut st = c.prepare(&sql).map_err(map_err)?;
    let mut rows = st.query([name]).map_err(map_err)?;
    match rows.next().map_err(map_err)? {
        Some(r) => Ok(Some((r.get(0).map_err(map_err)?, r.get(1).map_err(map_err)?))),
        None => Ok(None),
    }
}

fn describe_sync(c: &Connection, obj: &ObjectRef) -> Result<ObjectDescription> {
    let schema = schema_of(obj);
    let Some((ty, create_sql)) = object_sql(c, schema, &obj.name)? else {
        return Err(EngineError::input(format!(
            "There is no table or view named '{}' in '{schema}'.",
            obj.name
        )));
    };
    let kind = match ty.as_str() {
        "view" => NodeKind::View,
        "index" => NodeKind::Index,
        "trigger" => NodeKind::Trigger,
        _ => NodeKind::Table,
    };
    let mut d = ObjectDescription::new(ObjectRef::new(None, Some(schema), &obj.name, kind));
    let create_sql = create_sql.unwrap_or_default();
    if matches!(kind, NodeKind::Index | NodeKind::Trigger) {
        d.ddl = Some(format!("{create_sql};"));
        return Ok(d);
    }
    let upper = create_sql.to_ascii_uppercase();

    // Columns
    let mut pk: Vec<(i64, String)> = Vec::new();
    {
        let mut st = c
            .prepare("SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?1, ?2)")
            .map_err(map_err)?;
        let rows = st
            .query_map([&obj.name, schema], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(map_err)?;
        for r in rows {
            let (name, decl, notnull, default, pkpos, hidden) = r.map_err(map_err)?;
            if hidden == 1 {
                continue; // hidden column of a virtual table
            }
            if pkpos > 0 {
                pk.push((pkpos, name.clone()));
            }
            d.columns.push(ColumnInfo {
                hints: Default::default(),
                kind: types::kind_of(&decl),
                name,
                type_name: decl,
                nullable: notnull == 0 && pkpos == 0,
                default,
                comment: None,
                identity: None,
                generated: match hidden {
                    2 => Some("VIRTUAL".to_owned()),
                    3 => Some("STORED".to_owned()),
                    _ => None,
                },
            });
        }
    }
    pk.sort();
    let (without_rowid, strict) = table_flags(c, schema, &obj.name);
    if !pk.is_empty() {
        let cols: Vec<String> = pk.into_iter().map(|(_, n)| n).collect();
        if cols.len() == 1 && !without_rowid {
            if let Some(col) = d
                .columns
                .iter_mut()
                .find(|c| c.name == cols[0] && c.type_name.eq_ignore_ascii_case("INTEGER"))
            {
                col.identity = Some(
                    if upper.contains("AUTOINCREMENT") {
                        "AUTOINCREMENT"
                    } else {
                        "ROWID"
                    }
                    .to_owned(),
                );
            }
        }
        d.primary_key = Some(KeyInfo { name: None, columns: cols });
    }
    if kind == NodeKind::Table {
        d.extra.push(("withoutRowid".into(), without_rowid.to_string()));
        d.extra.push(("strict".into(), strict.to_string()));
    }

    // Foreign keys
    {
        let mut st = c
            .prepare(
                "SELECT id, seq, \"table\", \"from\", \"to\", on_update, on_delete FROM pragma_foreign_key_list(?1, ?2) ORDER BY id, seq",
            )
            .map_err(map_err)?;
        let rows = st
            .query_map([&obj.name, schema], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(map_err)?;
        let mut last_id = -1;
        for r in rows {
            let (id, table, from, to, on_update, on_delete) = r.map_err(map_err)?;
            if id != last_id {
                d.foreign_keys.push(ForeignKeyInfo {
                    ref_database: None,
                    name: None,
                    columns: Vec::new(),
                    ref_schema: Some(schema.to_owned()),
                    ref_table: table,
                    ref_columns: Vec::new(),
                    on_update: Some(on_update),
                    on_delete: Some(on_delete),
                });
                last_id = id;
            }
            let fk = d.foreign_keys.last_mut().expect("pushed above");
            fk.columns.push(from);
            // `to` is NULL when the reference is the parent's primary key.
            fk.ref_columns.push(to.unwrap_or_default());
        }
    }

    // Indexes
    let mut index_sql = Vec::new();
    {
        let mut st = c
            .prepare("SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1, ?2) ORDER BY seq")
            .map_err(map_err)?;
        let list: Vec<(String, i64, String, i64)> = st
            .query_map([&obj.name, schema], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(map_err)?
            .collect::<std::result::Result<_, _>>()
            .map_err(map_err)?;
        for (name, unique, origin, _partial) in list {
            let mut cols = Vec::new();
            let mut st = c
                .prepare("SELECT cid, name, \"desc\" FROM pragma_index_xinfo(?1, ?2) WHERE key = 1 ORDER BY seqno")
                .map_err(map_err)?;
            let rows = st
                .query_map([&name, schema], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?))
                })
                .map_err(map_err)?;
            for r in rows {
                let (cid, col, desc) = r.map_err(map_err)?;
                let mut label = match (cid, col) {
                    (-2, _) => "<expression>".to_owned(),
                    (-1, _) => "rowid".to_owned(),
                    (_, Some(n)) => n,
                    (_, None) => "?".to_owned(),
                };
                if desc == 1 {
                    label.push_str(" DESC");
                }
                cols.push(label);
            }
            let definition = object_sql(c, schema, &name)?.and_then(|(_, s)| s);
            if let Some(s) = &definition {
                index_sql.push(format!("{s};"));
            }
            if origin == "u" {
                d.unique_keys.push(KeyInfo {
                    name: Some(name.clone()),
                    columns: cols.clone(),
                });
            }
            d.indexes.push(IndexInfo {
                name,
                columns: cols,
                unique: unique == 1,
                primary: origin == "pk",
                method: None,
                definition,
            });
        }
    }

    // Statistics: the first number of sqlite_stat1.stat is the table's row count when ANALYZE ran.
    d.row_estimate = row_estimate(c, schema, &obj.name);

    // DDL: the stored CREATE statement, then indexes and triggers.
    let mut ddl = format!("{create_sql};");
    for s in index_sql {
        ddl.push_str("\n\n");
        ddl.push_str(&s);
    }
    let trig = format!(
        "SELECT sql FROM {}.sqlite_schema WHERE type = 'trigger' AND tbl_name = ?1 ORDER BY name",
        SqliteDialect.quote_ident(schema)
    );
    if let Ok(mut st) = c.prepare(&trig) {
        if let Ok(rows) = st.query_map([&obj.name], |r| r.get::<_, String>(0)) {
            for s in rows.flatten() {
                ddl.push_str("\n\n");
                ddl.push_str(&s);
                ddl.push(';');
            }
        }
    }
    d.ddl = Some(ddl);
    Ok(d)
}

fn table_flags(c: &Connection, schema: &str, name: &str) -> (bool, bool) {
    c.query_row(
        "SELECT wr, strict FROM pragma_table_list WHERE name = ?1 AND schema = ?2",
        [name, schema],
        |r| Ok((r.get::<_, i64>(0)? == 1, r.get::<_, i64>(1)? == 1)),
    )
    .unwrap_or((false, false))
}

fn row_estimate(c: &Connection, schema: &str, name: &str) -> Option<i64> {
    let sql = format!(
        "SELECT stat FROM {}.sqlite_stat1 WHERE tbl = ?1 LIMIT 1",
        SqliteDialect.quote_ident(schema)
    );
    let stat: String = c.query_row(&sql, [name], |r| r.get(0)).ok()?;
    stat.split_whitespace().next()?.parse().ok()
}

/// Columns as browsed and edited: the table's columns, plus the rowid pseudo-column for rowid tables without a primary key.
fn browse_columns(c: &Connection, obj: &ObjectRef) -> Result<(ObjectDescription, Vec<ColumnInfo>, Vec<String>)> {
    let d = describe_sync(c, obj)?;
    let mut columns = d.columns.clone();
    let key = match &d.primary_key {
        Some(pk) => pk.columns.clone(),
        None if d.object.kind == NodeKind::Table && !d.extra.iter().any(|(k, v)| k == "withoutRowid" && v == "true") => {
            let mut rowid = ColumnInfo::result(ROWID, "INTEGER", ValueKind::Int);
            rowid.nullable = false;
            rowid.identity = Some("ROWID".into());
            columns.insert(0, rowid);
            vec![ROWID.to_owned()]
        }
        None => Vec::new(),
    };
    Ok((d, columns, key))
}

fn bind_params(params: &[sqlbuild::Param]) -> Vec<rusqlite::types::Value> {
    params
        .iter()
        .map(|p| match &p.target {
            ParamTarget::Column(col) => types::to_sql(&p.value, &col.kind),
            ParamTarget::Text => types::to_sql(&p.value, &ValueKind::Text),
        })
        .collect()
}

fn browse_sync(c: &Connection, req: &BrowseRequest) -> Result<Page> {
    let (d, columns, key) = browse_columns(c, &req.object)?;
    let mut req = req.clone();
    req.object = d.object.clone();
    let built = sqlbuild::build_browse(&SqliteDialect, &req, &columns, &key)?;
    let mut st = c.prepare(&built.sql).map_err(map_err)?;
    let kinds: Vec<ValueKind> = columns.iter().map(|c| c.kind.clone()).collect();
    let mut rows_out = Vec::new();
    let mut rows = st.query(params_from_iter(bind_params(&built.params))).map_err(map_err)?;
    while let Some(r) = rows.next().map_err(map_err)? {
        rows_out.push(row_values(r, &kinds)?);
    }
    // SQLite keeps no planner estimates; only an unfiltered table has a statistics-based total.
    let estimated_total = if req.estimate_total && req.filter.is_empty() {
        d.row_estimate
    } else {
        None
    };
    Ok(sqlbuild::finish_page(columns, rows_out, req.limit, &built, key, estimated_total))
}

fn apply_sync(c: &mut Connection, obj: &ObjectRef, changes: &[RowChange]) -> Result<ChangeSummary> {
    let (d, columns, _key) = browse_columns(c, obj)?;
    let tx = c.transaction().map_err(map_err)?;
    let mut summary = ChangeSummary::default();
    for ch in changes {
        let (built, expect_one) = match ch {
            RowChange::Insert { values } => (sqlbuild::build_insert(&SqliteDialect, &d.object, values, &columns)?, false),
            RowChange::Update { key, values } => (sqlbuild::build_update(&SqliteDialect, &d.object, key, values, &columns)?, true),
            RowChange::Delete { key } => (sqlbuild::build_delete(&SqliteDialect, &d.object, key, &columns)?, true),
        };
        let n = tx
            .execute(&built.sql, params_from_iter(bind_params(&built.params)))
            .map_err(map_err)?;
        if expect_one && n != 1 {
            return Err(EngineError::input(format!(
                "A change matched {n} rows instead of one, so nothing was saved. Reload the table and try again."
            )));
        }
        match ch {
            RowChange::Insert { .. } => summary.inserted += n as u64,
            RowChange::Update { .. } => summary.updated += n as u64,
            RowChange::Delete { .. } => summary.deleted += n as u64,
        }
    }
    tx.commit().map_err(map_err)?;
    Ok(summary)
}

/// Reads one row with the given column kinds.
pub(crate) fn row_values(r: &rusqlite::Row<'_>, kinds: &[ValueKind]) -> Result<Vec<Value>> {
    let mut row = Vec::with_capacity(kinds.len());
    for (i, k) in kinds.iter().enumerate() {
        row.push(types::read(r.get_ref(i).map_err(map_err)?, k));
    }
    Ok(row)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parses_connection_strings() {
        assert_eq!(SqliteOptions::from_uri("C:\\data\\shop.db").unwrap().path, "C:\\data\\shop.db");
        assert_eq!(
            SqliteOptions::from_uri("sqlite:///C:/data/shop.db").unwrap().path,
            "C:/data/shop.db"
        );
        assert_eq!(SqliteOptions::from_uri("sqlite:///var/db/shop.db").unwrap().path, "/var/db/shop.db");
        let ro = SqliteOptions::from_uri("file:my%20shop.db?mode=ro&busy_timeout=250").unwrap();
        assert_eq!(ro.path, "my shop.db");
        assert!(ro.read_only);
        assert_eq!(ro.busy_timeout, Duration::from_millis(250));
        assert!(SqliteOptions::from_uri("sqlite:x.db?mode=rwc").unwrap().create);
        assert_eq!(SqliteOptions::from_uri("sqlite::memory:").unwrap().path, ":memory:");
        assert!(SqliteOptions::from_uri("x.db?bogus=1").is_err());
        assert!(SqliteOptions::from_uri("  ").is_err());
    }
}
