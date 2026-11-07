use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    future::Future,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use mysql_async::{
    consts::{ColumnFlags, ColumnType},
    prelude::*,
    Column, Conn, Opts, OptsBuilder, Params, Pool, PoolConstraints, PoolOpts, QueryResult, Row,
    TextProtocol, TxOpts, Value,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use async_trait::async_trait;

use crate::{
    config::ConnectionProfile,
    drivers::Driver,
    error::{AppError, AppResult},
    lock::lock,
    relations::{self, TableRelations},
    schema::{BulkSchema, SchemaColumn},
    vector,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_QUERY_ROWS: usize = 1000;
const MAX_PAGE_SIZE: u64 = 5000;
const POOL_SIZE: usize = 4;
const BINARY_CHARSET: u16 = 63;
const VECTOR_NAME_HINTS: [&str; 3] = ["embed", "vector", "vec"];

pub type Cell = Option<String>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
    pub numeric: bool,
    pub binary: bool,
    pub primary: bool,
    pub vector_dims: Option<u32>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSet {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub affected_rows: u64,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryOutcome {
    pub sets: Vec<ResultSet>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub name: String,
    pub kind: String,
    pub rows: Option<u64>,
    pub comment: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDesign {
    pub name: String,
    pub column_type: String,
    pub nullable: bool,
    pub key: String,
    pub default_value: Cell,
    pub extra: String,
    pub comment: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDesign {
    pub columns: Vec<ColumnDesign>,
    pub ddl: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePage {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub total: u64,
    pub primary_key: Vec<String>,
    pub elapsed_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseRequest {
    pub connection_id: String,
    pub database: String,
    pub table: String,
    pub offset: u64,
    pub limit: u64,
    pub order_by: Option<String>,
    pub descending: bool,
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CellValue {
    pub column: String,
    pub value: Cell,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RowChange {
    Insert { values: Vec<CellValue> },
    Update { key: Vec<CellValue>, values: Vec<CellValue> },
    Delete { key: Vec<CellValue> },
}

impl RowChange {
    fn statement(&self, target: &str, binary_columns: &HashSet<String>) -> AppResult<(String, Vec<Value>)> {
        match self {
            Self::Insert { values } => {
                let columns = join_mapped(values, ", ", |cell| quote_ident(&cell.column));
                let placeholders = vec!["?"; values.len()].join(", ");
                Ok((
                    format!("INSERT INTO {target} ({columns}) VALUES ({placeholders})"),
                    bind_values(values, binary_columns),
                ))
            }
            Self::Update { key, values } => {
                let assignments = join_mapped(values, ", ", |cell| format!("{} = ?", quote_ident(&cell.column)));
                let mut params = bind_values(values, binary_columns);
                params.extend(bind_values(key, binary_columns));
                Ok((
                    format!("UPDATE {target} SET {assignments} WHERE {} LIMIT 1", key_condition(key)?),
                    params,
                ))
            }
            Self::Delete { key } => Ok((
                format!("DELETE FROM {target} WHERE {} LIMIT 1", key_condition(key)?),
                bind_values(key, binary_columns),
            )),
        }
    }
}

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

struct QuerySession {
    opts: Opts,
    thread_id: AtomicU32,
    conn: AsyncMutex<Conn>,
}

impl QuerySession {
    async fn revive(&self, conn: &mut Conn) -> AppResult<()> {
        if conn.ping().await.is_ok() {
            return Ok(());
        }
        let fresh = within_timeout(Conn::new(self.opts.clone())).await?;
        self.thread_id.store(fresh.id(), Ordering::Relaxed);
        *conn = fresh;
        Ok(())
    }
}

/// One open MariaDB/MySQL server: a small pool for browsing plus one
/// dedicated connection per query tab, so BEGIN/COMMIT can span several runs.
pub struct MysqlConnection {
    opts: Opts,
    pool: Pool,
    sessions: Mutex<HashMap<String, Arc<QuerySession>>>,
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
            },
            version,
        ))
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
    async fn list_databases(&self) -> AppResult<Vec<String>> {
        let mut conn = self.pooled().await?;
        Ok(conn.query("SHOW DATABASES").await?)
    }

    async fn create_database(&self, name: &str, _first_collection: Option<&str>) -> AppResult<()> {
        let mut conn = self.pooled().await?;
        let sql = format!("CREATE DATABASE {} CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci", quote_ident(name));
        conn.query_drop(sql).await?;
        Ok(())
    }

    async fn list_tables(&self, database: &str) -> AppResult<Vec<TableInfo>> {
        let mut conn = self.pooled().await?;
        let rows = text_rows(
            &mut conn,
            "SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS, TABLE_COMMENT FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
            (database.to_owned(),),
        )
        .await?;
        Ok(rows
            .iter()
            .map(|row| TableInfo {
                name: row.text(0),
                kind: if row.text(1).contains("VIEW") { "view" } else { "table" }.to_owned(),
                rows: row.text(2).parse().ok(),
                comment: row.text(3),
            })
            .collect())
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

    async fn browse_table(&self, request: &BrowseRequest) -> AppResult<TablePage> {
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
        let result = conn
            .query_iter(format!(
                "SELECT * FROM {source}{condition}{ordering} LIMIT {limit} OFFSET {}",
                request.offset
            ))
            .await?;
        let page = read_result_sets(result, usize::MAX, &VectorHint::Known(&vectors))
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let elapsed_ms = elapsed_millis(started);
        let total: Option<u64> = conn
            .query_first(format!("SELECT COUNT(*) FROM {source}{condition}"))
            .await?;
        let primary_key = primary_key(&mut conn, &request.database, &request.table).await?;
        Ok(TablePage {
            columns: page.columns,
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
            .map(|change| change.statement(&target, &binary))
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
            conn: AsyncMutex::new(conn),
        };
        lock(&self.sessions).insert(session_id.to_owned(), Arc::new(session));
        Ok(())
    }

    fn close_session(&self, session_id: &str) {
        lock(&self.sessions).remove(session_id);
    }

    async fn run_query(&self, session_id: &str, database: Option<String>, sql: String) -> AppResult<QueryOutcome> {
        let session = self.session(session_id)?;
        let mut conn = session.conn.lock().await;
        session.revive(&mut conn).await?;
        if let Some(database) = database.filter(|name| !name.is_empty()) {
            conn.query_drop(format!("USE {}", quote_ident(&database))).await?;
        }
        let started = Instant::now();
        let result = conn.query_iter(sql).await?;
        let sets = read_result_sets(result, MAX_QUERY_ROWS, &VectorHint::Guess).await?;
        Ok(QueryOutcome {
            sets,
            elapsed_ms: elapsed_millis(started),
        })
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
        relations::table_relations(&mut conn, database, table).await
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
        Ok(Some(BulkSchema { tables, columns, relations }))
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
        Err(_) => Err(AppError::new(
            "Connection timed out. Is the port-forward task running?",
        )),
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

pub(crate) async fn read_result_sets(
    mut result: QueryResult<'_, 'static, TextProtocol>,
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

pub(crate) struct TextRow(pub(crate) Vec<Cell>);

impl TextRow {
    pub(crate) fn text(&self, index: usize) -> String {
        self.optional(index).unwrap_or_default()
    }

    pub(crate) fn optional(&self, index: usize) -> Cell {
        self.0.get(index).cloned().flatten()
    }
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
            format!("{column_type:?}").trim_start_matches("MYSQL_TYPE_").to_lowercase()
        },
        numeric: is_numeric(column_type),
        binary: vector_dims.is_none() && column.character_set() == BINARY_CHARSET && is_byte_string(column_type),
        primary: column.flags().contains(ColumnFlags::PRI_KEY_FLAG),
        vector_dims,
    }
}

fn guess_vector_dims(column: &Column) -> Option<u32> {
    let name = column.name_str().to_lowercase();
    let named_like_vector = VECTOR_NAME_HINTS.iter().any(|hint| name.contains(hint));
    let binary = column.character_set() == BINARY_CHARSET && is_byte_string(column.column_type());
    let length = column.column_length();
    (named_like_vector && binary && length > 0 && length % 4 == 0).then_some(length / 4)
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

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(2 + bytes.len() * 2);
    text.push_str("0x");
    for byte in bytes {
        let _ = write!(text, "{byte:02X}");
    }
    text
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

pub(crate) fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
