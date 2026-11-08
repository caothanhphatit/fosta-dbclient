//! MariaDB / MySQL driver built on mysql_async. Also exposes the SQL helpers
//! the MariaDB-only vector tooling (`crate::vector`) builds on.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use mysql_async::{
    consts::{ColumnFlags, ColumnType},
    prelude::*,
    Column, Conn, Opts, OptsBuilder, Params, Pool, PoolConstraints, PoolOpts, QueryResult, Row, TxOpts, Value,
};
use tokio::sync::Mutex as AsyncMutex;

use super::{
    relations::{self, TableRelations},
    types::{
        elapsed_millis, encode_hex, BrowseRequest, Cell, CellValue, ColumnDesign, ColumnMeta, QueryOutcome, ResultSet, RowChange,
        TableDesign, TableInfo, TablePage, TextRow, TransactionOp, TransactionState,
    },
    Driver, Engine, Limits,
};
use crate::{
    config::ConnectionProfile,
    error::{AppError, AppResult},
    lock::lock,
    schema::{BulkSchema, SchemaColumn},
    vector,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PAGE_SIZE: u64 = 5000;
const POOL_SIZE: usize = 4;
const BINARY_CHARSET: u16 = 63;
const VECTOR_NAME_HINTS: [&str; 3] = ["embed", "vector", "vec"];

pub(crate) enum VectorHint<'a> {
    Known(&'a HashMap<String, u32>),
    Guess,
}

impl VectorHint<'_> {
    fn dims(&self, column: &Column) -> Option<u32> {
        match self {
            Self::Known(columns) => columns.get(column.name_str().as_ref()).copied(),
            Self::Guess => guess_vector_dims(column),
        }
    }
}

#[derive(Clone, Copy)]
enum CellFormat {
    Text,
    Hex,
    Vector,
}

impl CellFormat {
    fn of(column: &ColumnMeta) -> Self {
        if column.vector_dims.is_some() {
            Self::Vector
        } else if column.binary {
            Self::Hex
        } else {
            Self::Text
        }
    }
}

/// A query tab's dedicated connection and the session settings already applied to it.
struct SessionConn {
    conn: Conn,
    /// `SET SESSION TRANSACTION READ ONLY/WRITE` last applied; `None` = not yet.
    read_only: Option<bool>,
    /// Statement timeout (seconds) last applied.
    timeout_secs: Option<u32>,
    /// What the user chose with the auto-commit switch; re-applied after a reconnect.
    auto_commit: bool,
}

struct QuerySession {
    opts: Opts,
    thread_id: AtomicU32,
    conn: AsyncMutex<SessionConn>,
}

impl QuerySession {
    /// Reconnects a dead session. Returns true when it had to (an open transaction is then gone).
    async fn revive(&self, state: &mut SessionConn) -> AppResult<bool> {
        if state.conn.ping().await.is_ok() {
            return Ok(false);
        }
        let mut fresh = within_timeout(Conn::new(self.opts.clone())).await?;
        if !state.auto_commit {
            fresh.query_drop("SET autocommit = 0").await?;
        }
        self.thread_id.store(fresh.id(), Ordering::Relaxed);
        state.conn = fresh;
        state.read_only = None;
        state.timeout_secs = None;
        Ok(true)
    }
}

/// One open MariaDB/MySQL server: a small pool for browsing plus one
/// dedicated connection per query tab, so BEGIN/COMMIT can span several runs.
pub struct MysqlConnection {
    opts: Opts,
    pool: Pool,
    sessions: Mutex<HashMap<String, Arc<QuerySession>>>,
    /// MariaDB and MySQL differ in timeouts and in how they report transactions.
    mariadb: bool,
}

impl MysqlConnection {
    pub async fn open(profile: &ConnectionProfile, password: &str) -> AppResult<(Self, String)> {
        let opts = build_opts(profile, password);
        let pool = Pool::new(opts.clone());
        let mut conn = within_timeout(pool.get_conn()).await?;
        let version = server_version(&mut conn).await?;
        drop(conn);
        Ok((
            Self {
                opts,
                pool,
                sessions: Mutex::default(),
                mariadb: version.to_lowercase().contains("mariadb"),
            },
            version,
        ))
    }

    /// Wraps an app-built SELECT so the server stops it after `seconds` (0 = no limit).
    fn with_timeout(&self, select: String, seconds: u32) -> String {
        if seconds == 0 {
            select
        } else if self.mariadb {
            format!("SET STATEMENT max_statement_time = {seconds} FOR {select}")
        } else {
            let millis = u64::from(seconds) * 1000;
            match select.strip_prefix("SELECT ") {
                Some(rest) => format!("SELECT /*+ MAX_EXECUTION_TIME({millis}) */ {rest}"),
                None => select,
            }
        }
    }

    /// Applies the read-only mode and the statement timeout of the rules to a session.
    async fn apply_limits(&self, state: &mut SessionConn, limits: &Limits) -> AppResult<()> {
        if state.read_only != Some(limits.read_only) {
            let mode = if limits.read_only { "READ ONLY" } else { "READ WRITE" };
            state.conn.query_drop(format!("SET SESSION TRANSACTION {mode}")).await?;
            state.read_only = Some(limits.read_only);
        }
        if state.timeout_secs != Some(limits.timeout_secs) {
            let sql = if self.mariadb {
                format!("SET SESSION max_statement_time = {}", limits.timeout_secs)
            } else {
                // MySQL only limits SELECT statements.
                format!("SET SESSION max_execution_time = {}", u64::from(limits.timeout_secs) * 1000)
            };
            state.conn.query_drop(sql).await?;
            state.timeout_secs = Some(limits.timeout_secs);
        }
        Ok(())
    }

    /// The session's transaction state as the server reports it.
    async fn transaction_state(&self, conn: &mut Conn) -> TransactionState {
        let sql = if self.mariadb {
            "SELECT @@in_transaction, @@autocommit"
        } else {
            "SELECT (SELECT COUNT(*) FROM performance_schema.events_transactions_current \
             WHERE THREAD_ID = PS_CURRENT_THREAD_ID() AND STATE = 'ACTIVE'), @@autocommit"
        };
        match conn.query_first::<(i64, i64), _>(sql).await {
            Ok(Some((open, auto_commit))) => TransactionState {
                supported: true,
                open: open > 0,
                auto_commit: auto_commit != 0,
                failed: false,
            },
            _ => TransactionState::unsupported(),
        }
    }

    pub(crate) async fn pooled(&self) -> AppResult<Conn> {
        within_timeout(self.pool.get_conn()).await
    }

    fn session(&self, session_id: &str) -> AppResult<Arc<QuerySession>> {
        lock(&self.sessions)
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::new("This query tab lost its session. Close it and open a new query."))
    }
}

#[async_trait]
impl Driver for MysqlConnection {
    fn engine(&self) -> Engine {
        if self.mariadb {
            Engine::MariaDb
        } else {
            Engine::MySql
        }
    }

    async fn list_databases(&self) -> AppResult<Vec<String>> {
        let mut conn = self.pooled().await?;
        Ok(conn.query("SHOW DATABASES").await?)
    }

    async fn create_database(&self, name: &str, _first_collection: Option<&str>) -> AppResult<()> {
        let mut conn = self.pooled().await?;
        let sql = format!(
            "CREATE DATABASE {} CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci",
            quote_ident(name)
        );
        conn.query_drop(sql).await?;
        Ok(())
    }

    async fn list_tables(&self, database: &str) -> AppResult<Vec<TableInfo>> {
        let mut conn = self.pooled().await?;
        let rows = text_rows(
            &mut conn,
            "SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS, TABLE_COMMENT, DATA_LENGTH FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
            (database.to_owned(),),
        )
        .await?;
        let mut tables: Vec<TableInfo> = rows
            .iter()
            .map(|row| TableInfo {
                name: row.text(0),
                kind: if row.text(1).contains("VIEW") { "view" } else { "table" }.to_owned(),
                rows: row_estimate(row.optional(2), row.optional(4)),
                comment: row.text(3),
            })
            .collect();
        // InnoDB statistics can claim 0 rows (and an empty page) long after a bulk load. A cheap EXISTS
        // tells a really empty table from a stale estimate, which then shows as unknown.
        let zero: Vec<usize> = (0..tables.len())
            .filter(|&index| tables[index].kind == "table" && tables[index].rows == Some(0))
            .take(MAX_EMPTY_PROBES)
            .collect();
        for index in zero {
            let probe = format!("SELECT EXISTS(SELECT 1 FROM {})", qualified(database, &tables[index].name));
            if let Ok(Some(1_i64)) = conn.query_first::<i64, _>(probe).await {
                tables[index].rows = None;
            }
        }
        Ok(tables)
    }

    async fn describe_table(&self, database: &str, table: &str) -> AppResult<TableDesign> {
        let mut conn = self.pooled().await?;
        let rows = text_rows(
            &mut conn,
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT \
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
            (database.to_owned(), table.to_owned()),
        )
        .await?;
        let columns = rows
            .iter()
            .map(|row| ColumnDesign {
                name: row.text(0),
                column_type: row.text(1),
                nullable: row.text(2) == "YES",
                key: row.text(3),
                default_value: row.optional(4),
                extra: row.text(5),
                comment: row.text(6),
            })
            .collect();
        let ddl = create_statement(&mut conn, database, table).await?;
        Ok(TableDesign { columns, ddl })
    }

    /// Runs as prepared statements (`exec`), so even a filter that slipped past the gateway
    /// could not stack a second statement: the server prepares exactly one.
    async fn browse_table(&self, request: &BrowseRequest, limits: &Limits) -> AppResult<TablePage> {
        let mut conn = self.pooled().await?;
        let vectors = vector::vector_columns(&mut conn, &request.database, &request.table).await?;
        let source = qualified(&request.database, &request.table);
        let condition = where_clause(request.filter.as_deref());
        let ordering = request
            .order_by
            .as_deref()
            .map(|column| {
                let direction = if request.descending { "DESC" } else { "ASC" };
                format!(" ORDER BY {} {direction}", quote_ident(column))
            })
            .unwrap_or_default();
        let limit = request.limit.clamp(1, MAX_PAGE_SIZE);
        let started = Instant::now();
        let page_sql = self.with_timeout(
            format!(
                "SELECT * FROM {source}{condition}{ordering} LIMIT {limit} OFFSET {}",
                request.offset
            ),
            limits.timeout_secs,
        );
        let result = conn.exec_iter(page_sql, ()).await?;
        let page = read_result_sets(result, usize::MAX, &VectorHint::Known(&vectors))
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let elapsed_ms = elapsed_millis(started);
        let count_sql = self.with_timeout(format!("SELECT COUNT(*) FROM {source}{condition}"), limits.timeout_secs);
        let total: Option<u64> = conn.exec_first(count_sql, ()).await?;
        let primary_key = primary_key(&mut conn, &request.database, &request.table).await?;
        let declared = declared_types(&mut conn, &request.database, &request.table).await?;
        let mut columns = page.columns;
        for column in columns.iter_mut().filter(|column| column.vector_dims.is_none()) {
            if let Some(declared) = declared.get(&column.name) {
                column.type_name.clone_from(declared);
            }
        }
        Ok(TablePage {
            columns,
            rows: page.rows,
            total: total.unwrap_or(0),
            primary_key,
            elapsed_ms,
        })
    }

    async fn apply_changes(&self, database: &str, table: &str, changes: Vec<RowChange>) -> AppResult<u64> {
        let mut conn = self.pooled().await?;
        let binary = binary_columns(&mut conn, database, table).await?;
        let target = qualified(database, table);
        let statements = changes
            .iter()
            .map(|change| statement(change, &target, &binary))
            .collect::<AppResult<Vec<_>>>()?;
        let mut transaction = conn.start_transaction(TxOpts::default()).await?;
        let mut affected = 0;
        for (sql, params) in statements {
            let result = transaction.exec_iter(sql, params).await?;
            affected += result.affected_rows();
            result.drop_result().await?;
        }
        transaction.commit().await?;
        Ok(affected)
    }

    async fn open_session(&self, session_id: &str) -> AppResult<()> {
        let conn = within_timeout(Conn::new(self.opts.clone())).await?;
        let session = QuerySession {
            opts: self.opts.clone(),
            thread_id: AtomicU32::new(conn.id()),
            conn: AsyncMutex::new(SessionConn {
                conn,
                read_only: None,
                timeout_secs: None,
                auto_commit: true,
            }),
        };
        lock(&self.sessions).insert(session_id.to_owned(), Arc::new(session));
        Ok(())
    }

    fn close_session(&self, session_id: &str) {
        lock(&self.sessions).remove(session_id);
    }

    async fn run_query(&self, session_id: &str, database: Option<String>, sql: String, limits: &Limits) -> AppResult<QueryOutcome> {
        let session = self.session(session_id)?;
        let mut state = session.conn.lock().await;
        session.revive(&mut state).await?;
        self.apply_limits(&mut state, limits).await?;
        if let Some(database) = database.filter(|name| !name.is_empty()) {
            state.conn.query_drop(format!("USE {}", quote_ident(&database))).await?;
        }
        let started = Instant::now();
        let result = state.conn.query_iter(sql).await?;
        let sets = read_result_sets(result, limits.max_rows, &VectorHint::Guess).await?;
        let elapsed_ms = elapsed_millis(started);
        let transaction = self.transaction_state(&mut state.conn).await;
        Ok(QueryOutcome {
            sets,
            elapsed_ms,
            transaction,
        })
    }

    async fn transaction(&self, session_id: &str, op: TransactionOp, limits: &Limits) -> AppResult<TransactionState> {
        let session = self.session(session_id)?;
        let mut state = session.conn.lock().await;
        if session.revive(&mut state).await? && matches!(op, TransactionOp::Commit | TransactionOp::Rollback) {
            return Err(AppError::new(
                "The connection of this tab was lost and has been reopened; the server rolled back the open transaction.",
            ));
        }
        match op {
            TransactionOp::Begin => {
                // START TRANSACTION would implicitly commit an open transaction.
                self.transaction_state(&mut state.conn).await.ensure_can_begin()?;
                self.apply_limits(&mut state, limits).await?;
                let sql = if limits.read_only {
                    "START TRANSACTION READ ONLY"
                } else {
                    "START TRANSACTION"
                };
                state.conn.query_drop(sql).await?;
            }
            TransactionOp::Commit => state.conn.query_drop("COMMIT").await?,
            TransactionOp::Rollback => state.conn.query_drop("ROLLBACK").await?,
            TransactionOp::AutoCommitOn | TransactionOp::AutoCommitOff => {
                let on = op == TransactionOp::AutoCommitOn;
                state.conn.query_drop(format!("SET autocommit = {}", u8::from(on))).await?;
                state.auto_commit = on;
            }
            TransactionOp::Status => {}
        }
        Ok(self.transaction_state(&mut state.conn).await)
    }

    async fn cancel_query(&self, session_id: &str) -> AppResult<()> {
        let session = self.session(session_id)?;
        let mut conn = self.pooled().await?;
        let thread_id = session.thread_id.load(Ordering::Relaxed);
        conn.query_drop(format!("KILL QUERY {thread_id}")).await?;
        Ok(())
    }

    async fn table_relations(&self, database: &str, table: &str) -> AppResult<TableRelations> {
        let mut conn = self.pooled().await?;
        table_relations(&mut conn, database, table).await
    }

    async fn schema_bulk(&self, database: &str) -> AppResult<Option<BulkSchema>> {
        let tables = self.list_tables(database).await?;
        let mut conn = self.pooled().await?;
        let column_rows = text_rows(
            &mut conn,
            "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY \
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION",
            (database.to_owned(),),
        )
        .await?;
        let mut columns: HashMap<String, Vec<SchemaColumn>> = HashMap::new();
        for row in &column_rows {
            columns.entry(row.text(0)).or_default().push(SchemaColumn {
                name: row.text(1),
                column_type: row.text(2),
                nullable: row.text(3) == "YES",
                primary: row.text(4).eq_ignore_ascii_case("PRI"),
            });
        }
        let relation_rows = text_rows(
            &mut conn,
            "SELECT CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, \
                    REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
             FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL \
             ORDER BY TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
            (database.to_owned(),),
        )
        .await?;
        let relations = relations::group_relations(&relation_rows);
        Ok(Some(BulkSchema {
            tables,
            columns,
            relations,
        }))
    }

    async fn close(&self) {
        let _ = self.pool.clone().disconnect().await;
    }

    fn as_mysql(&self) -> Option<&MysqlConnection> {
        Some(self)
    }
}

fn build_opts(profile: &ConnectionProfile, password: &str) -> Opts {
    let constraints = PoolConstraints::new(0, POOL_SIZE).unwrap_or_default();
    OptsBuilder::default()
        .ip_or_hostname(profile.host.clone())
        .tcp_port(profile.port)
        .user(Some(profile.user.clone()))
        .pass(Some(password.to_owned()))
        .db_name(profile.database.clone().filter(|name| !name.is_empty()))
        .pool_opts(PoolOpts::default().with_constraints(constraints))
        .into()
}

async fn within_timeout<T>(future: impl Future<Output = mysql_async::Result<T>>) -> AppResult<T> {
    match tokio::time::timeout(CONNECT_TIMEOUT, future).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(AppError::new("Connection timed out. Is the port-forward task running?")),
    }
}

async fn server_version(conn: &mut Conn) -> AppResult<String> {
    let version: Option<String> = conn.query_first("SELECT VERSION()").await?;
    Ok(version.unwrap_or_default())
}

pub(crate) async fn create_statement(conn: &mut Conn, database: &str, table: &str) -> AppResult<String> {
    let create: Option<Row> = conn
        .query_first(format!("SHOW CREATE TABLE {}", qualified(database, table)))
        .await?;
    Ok(create.map(|row| TextRow(row_cells(row, &[])).text(1)).unwrap_or_default())
}

pub(crate) async fn primary_key(conn: &mut Conn, database: &str, table: &str) -> AppResult<Vec<String>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY' ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows.iter().map(|row| row.text(0)).collect())
}

pub(crate) async fn binary_columns(conn: &mut Conn, database: &str, table: &str) -> AppResult<HashSet<String>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         AND DATA_TYPE IN ('binary', 'varbinary', 'tinyblob', 'blob', 'mediumblob', 'longblob', 'bit')",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows.iter().map(|row| row.text(0)).collect())
}

pub(crate) async fn read_result_sets<P: Protocol>(
    mut result: QueryResult<'_, 'static, P>,
    max_rows: usize,
    hint: &VectorHint<'_>,
) -> AppResult<Vec<ResultSet>> {
    let first_affected = result.affected_rows();
    let mut sets = Vec::new();
    while !result.is_empty() {
        let columns: Vec<ColumnMeta> = result
            .columns()
            .map(|columns| columns.iter().map(|column| column_meta(column, hint)).collect())
            .unwrap_or_default();
        let affected_rows = result.affected_rows();
        let formats: Vec<CellFormat> = columns.iter().map(CellFormat::of).collect();
        let mut rows = Vec::new();
        let mut truncated = false;
        result
            .for_each(|row: Row| {
                if rows.len() < max_rows {
                    rows.push(row_cells(row, &formats));
                } else {
                    truncated = true;
                }
            })
            .await?;
        sets.push(ResultSet {
            columns,
            rows,
            affected_rows,
            truncated,
        });
    }
    if sets.is_empty() {
        sets.push(ResultSet {
            affected_rows: first_affected,
            ..ResultSet::default()
        });
    }
    Ok(sets)
}

pub(crate) async fn text_rows(conn: &mut Conn, sql: &str, params: impl Into<Params>) -> AppResult<Vec<TextRow>> {
    let params: Params = params.into();
    let rows: Vec<Row> = conn.exec(sql, params).await?;
    Ok(rows.into_iter().map(|row| TextRow(row_cells(row, &[]))).collect())
}

pub(crate) fn row_texts(row: Row) -> Vec<Cell> {
    row_cells(row, &[])
}

fn column_meta(column: &Column, hint: &VectorHint<'_>) -> ColumnMeta {
    let column_type = column.column_type();
    let vector_dims = hint.dims(column);
    ColumnMeta {
        name: column.name_str().into_owned(),
        type_name: if vector_dims.is_some() {
            "vector".to_owned()
        } else {
            sql_type_name(column_type, column.character_set() == BINARY_CHARSET, column.flags()).to_owned()
        },
        numeric: is_numeric(column_type),
        binary: vector_dims.is_none() && column.character_set() == BINARY_CHARSET && is_byte_string(column_type),
        primary: column.flags().contains(ColumnFlags::PRI_KEY_FLAG),
        vector_dims,
    }
}

/// The SQL type a result column was declared with, from its wire type (`MYSQL_TYPE_LONG` is an `int`,
/// `NEWDECIMAL` a `decimal`). Table pages replace it with the exact `COLUMN_TYPE` (`int(11)`, `decimal(10,2)`).
fn sql_type_name(column_type: ColumnType, binary: bool, flags: ColumnFlags) -> &'static str {
    use ColumnType::*;
    if flags.contains(ColumnFlags::ENUM_FLAG) {
        return "enum";
    }
    if flags.contains(ColumnFlags::SET_FLAG) {
        return "set";
    }
    match column_type {
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_NULL => "null",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING if binary => "varbinary",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => "varchar",
        MYSQL_TYPE_STRING if binary => "binary",
        MYSQL_TYPE_STRING => "char",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB if binary => "blob",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB => "text",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_GEOMETRY => "geometry",
        MYSQL_TYPE_VECTOR => "vector",
        _ => "unknown",
    }
}

/// Tables reporting 0 rows that `list_tables` checks for data; beyond that a 0 is shown as reported.
const MAX_EMPTY_PROBES: usize = 32;

/// Size of one InnoDB page: what an empty table reports as `DATA_LENGTH`.
const EMPTY_TABLE_BYTES: u64 = 16 * 1024;

/// The tree's row estimate. InnoDB's `TABLE_ROWS` stays 0 until statistics are refreshed (right after a
/// bulk load, for example), so a table that holds data but claims 0 rows reports "unknown" instead.
fn row_estimate(table_rows: Cell, data_length: Cell) -> Option<u64> {
    let rows: u64 = table_rows?.parse().ok()?;
    let bytes: u64 = data_length.and_then(|text| text.parse().ok()).unwrap_or(0);
    (rows > 0 || bytes <= EMPTY_TABLE_BYTES).then_some(rows)
}

/// `COLUMN_TYPE` of each column of a table, e.g. `int(11) unsigned`, keyed by column name.
async fn declared_types(conn: &mut Conn, database: &str, table: &str) -> AppResult<HashMap<String, String>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME, COLUMN_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows.iter().map(|row| (row.text(0), row.text(1))).collect())
}

fn guess_vector_dims(column: &Column) -> Option<u32> {
    let name = column.name_str().to_lowercase();
    let named_like_vector = VECTOR_NAME_HINTS.iter().any(|hint| name.contains(hint));
    let binary = column.character_set() == BINARY_CHARSET && is_byte_string(column.column_type());
    let length = column.column_length();
    (named_like_vector && binary && length > 0 && length.is_multiple_of(4)).then_some(length / 4)
}

fn is_numeric(column_type: ColumnType) -> bool {
    use ColumnType::*;
    matches!(
        column_type,
        MYSQL_TYPE_DECIMAL
            | MYSQL_TYPE_NEWDECIMAL
            | MYSQL_TYPE_TINY
            | MYSQL_TYPE_SHORT
            | MYSQL_TYPE_LONG
            | MYSQL_TYPE_INT24
            | MYSQL_TYPE_LONGLONG
            | MYSQL_TYPE_FLOAT
            | MYSQL_TYPE_DOUBLE
            | MYSQL_TYPE_YEAR
    )
}

fn is_byte_string(column_type: ColumnType) -> bool {
    use ColumnType::*;
    matches!(
        column_type,
        MYSQL_TYPE_STRING
            | MYSQL_TYPE_VAR_STRING
            | MYSQL_TYPE_VARCHAR
            | MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
            | MYSQL_TYPE_BIT
    )
}

fn row_cells(row: Row, formats: &[CellFormat]) -> Vec<Cell> {
    row.unwrap()
        .into_iter()
        .enumerate()
        .map(|(index, value)| value_text(value, formats.get(index).copied().unwrap_or(CellFormat::Text)))
        .collect()
}

fn value_text(value: Value, format: CellFormat) -> Cell {
    match value {
        Value::NULL => None,
        Value::Bytes(bytes) => Some(bytes_text(bytes, format)),
        Value::Int(number) => Some(number.to_string()),
        Value::UInt(number) => Some(number.to_string()),
        Value::Float(number) => Some(number.to_string()),
        Value::Double(number) => Some(number.to_string()),
        Value::Date(year, month, day, hour, minute, second, micros) => {
            let date = format!("{year:04}-{month:02}-{day:02}");
            if (hour, minute, second, micros) == (0, 0, 0, 0) {
                Some(date)
            } else {
                Some(format!("{date} {hour:02}:{minute:02}:{second:02}{}", fraction(micros)))
            }
        }
        Value::Time(negative, days, hours, minutes, seconds, micros) => {
            let sign = if negative { "-" } else { "" };
            let total_hours = days * 24 + u32::from(hours);
            Some(format!("{sign}{total_hours:02}:{minutes:02}:{seconds:02}{}", fraction(micros)))
        }
    }
}

fn bytes_text(bytes: Vec<u8>, format: CellFormat) -> String {
    match format {
        CellFormat::Vector => vector::decode_vector(&bytes)
            .map(|values| vector::summarize(&values))
            .unwrap_or_else(|| encode_hex(&bytes)),
        CellFormat::Hex => encode_hex(&bytes),
        CellFormat::Text => String::from_utf8(bytes).unwrap_or_else(|error| encode_hex(error.as_bytes())),
    }
}

fn fraction(micros: u32) -> String {
    if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    }
}

/// The parameterised statement for one grid edit against `target` (already quoted).
fn statement(change: &RowChange, target: &str, binary_columns: &HashSet<String>) -> AppResult<(String, Vec<Value>)> {
    match change {
        RowChange::Insert { values } => {
            let columns = join_mapped(values, ", ", |cell| quote_ident(&cell.column));
            let placeholders = vec!["?"; values.len()].join(", ");
            Ok((
                format!("INSERT INTO {target} ({columns}) VALUES ({placeholders})"),
                bind_values(values, binary_columns),
            ))
        }
        RowChange::Update { key, values } => {
            let assignments = join_mapped(values, ", ", |cell| format!("{} = ?", quote_ident(&cell.column)));
            let mut params = bind_values(values, binary_columns);
            params.extend(bind_values(key, binary_columns));
            Ok((
                format!("UPDATE {target} SET {assignments} WHERE {} LIMIT 1", key_condition(key)?),
                params,
            ))
        }
        RowChange::Delete { key } => Ok((
            format!("DELETE FROM {target} WHERE {} LIMIT 1", key_condition(key)?),
            bind_values(key, binary_columns),
        )),
    }
}

/// Foreign keys leaving or pointing at one table.
async fn table_relations(conn: &mut Conn, database: &str, table: &str) -> AppResult<TableRelations> {
    let rows = text_rows(
        conn,
        "SELECT CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, \
                REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
         FROM information_schema.KEY_COLUMN_USAGE \
         WHERE REFERENCED_TABLE_NAME IS NOT NULL \
           AND ((TABLE_SCHEMA = ? AND TABLE_NAME = ?) OR (REFERENCED_TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME = ?)) \
         ORDER BY TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
        (database.to_owned(), table.to_owned(), database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(relations::split_relations(relations::group_relations(&rows), database, table))
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))?;
    if digits.len() % 2 != 0 {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(digits.get(index..index + 2)?, 16).ok())
        .collect()
}

pub(crate) fn bind_values(cells: &[CellValue], binary_columns: &HashSet<String>) -> Vec<Value> {
    cells
        .iter()
        .map(|cell| bind_value(cell, binary_columns.contains(&cell.column)))
        .collect()
}

fn bind_value(cell: &CellValue, binary: bool) -> Value {
    match &cell.value {
        None => Value::NULL,
        Some(text) => {
            let decoded = if binary { decode_hex(text) } else { None };
            Value::Bytes(decoded.unwrap_or_else(|| text.as_bytes().to_vec()))
        }
    }
}

pub(crate) fn key_condition(key: &[CellValue]) -> AppResult<String> {
    if key.is_empty() {
        return Err(AppError::new("Cannot identify the row"));
    }
    Ok(join_mapped(key, " AND ", |cell| format!("{} <=> ?", quote_ident(&cell.column))))
}

pub(crate) fn where_clause(filter: Option<&str>) -> String {
    filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .map(|filter| format!(" WHERE {filter}"))
        .unwrap_or_default()
}

fn join_mapped<T>(items: &[T], separator: &str, render: impl Fn(&T) -> String) -> String {
    items.iter().map(render).collect::<Vec<_>>().join(separator)
}

pub(crate) fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

pub(crate) fn qualified(database: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(database), quote_ident(table))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Cell {
        Some(value.to_owned())
    }

    #[test]
    fn stale_zero_row_estimates_are_unknown() {
        assert_eq!(row_estimate(text("16001"), text("1589248")), Some(16001));
        assert_eq!(row_estimate(text("0"), text("16384")), Some(0), "an empty table");
        assert_eq!(row_estimate(text("0"), text("1589248")), None, "data but no statistics yet");
        assert_eq!(row_estimate(None, None), None, "views");
    }

    #[test]
    fn wire_types_read_as_sql_types() {
        let none = ColumnFlags::empty();
        assert_eq!(sql_type_name(ColumnType::MYSQL_TYPE_LONG, false, none), "int");
        assert_eq!(sql_type_name(ColumnType::MYSQL_TYPE_LONGLONG, false, none), "bigint");
        assert_eq!(sql_type_name(ColumnType::MYSQL_TYPE_NEWDECIMAL, false, none), "decimal");
        assert_eq!(sql_type_name(ColumnType::MYSQL_TYPE_VAR_STRING, false, none), "varchar");
        assert_eq!(sql_type_name(ColumnType::MYSQL_TYPE_VAR_STRING, true, none), "varbinary");
        assert_eq!(sql_type_name(ColumnType::MYSQL_TYPE_BLOB, false, none), "text");
        assert_eq!(sql_type_name(ColumnType::MYSQL_TYPE_STRING, false, ColumnFlags::ENUM_FLAG), "enum");
    }
}
