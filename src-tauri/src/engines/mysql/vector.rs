//! MariaDB vector tooling (VECTOR columns, MariaDB 11.7+), served as the engine operations `vector.info`,
//! `vector.fetch`, `vector.search`, `vector.health` and `vector.recall` (`Connection::invoke`). Arguments and results are
//! the JSON shapes of the vector commands; result sets are text cells like the old query results, with vectors
//! summarised.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use mysql_async::{consts::ColumnType, prelude::*, Column, Conn, QueryResult, Row, Value};
use serde::{Deserialize, Serialize};

use super::{
    catalog::{create_statement, qualified, quote_ident, text_rows},
    values::{guess_vector_dims, plain_text, sql_type_name, vector_text as vector_literal, BINARY_CHARSET},
};
use crate::error::{AppError, AppResult};

pub use super::values::{decode_vector, parse_vector as parse_literal};

/// `0x…` with upper-case digits, as the vector view has always shown binary cells.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(2 + bytes.len() * 2);
    text.push_str("0x");
    for byte in bytes {
        let _ = write!(text, "{byte:02X}");
    }
    text
}

/// The engine operations this module answers.
pub const OPERATIONS: [&str; 5] = ["vector.info", "vector.fetch", "vector.search", "vector.health", "vector.recall"];

/// A key cell of a row, as the grid sends it (`0x…` for binary columns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyCell {
    pub column: String,
    pub value: Option<String>,
}

/// A result column as the vector view reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextColumn {
    pub name: String,
    pub type_name: String,
    pub numeric: bool,
    pub binary: bool,
    pub primary: bool,
    pub vector_dims: Option<u32>,
}

/// A result set with text cells.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSet {
    pub columns: Vec<TextColumn>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected_rows: u64,
    pub truncated: bool,
}

const PREVIEW_VALUES: usize = 4;
const NORM_TOLERANCE: f32 = 0.01;
const MAX_K: u32 = 1000;
const MAX_RECALL_K: u32 = 100;
const MAX_RECALL_QUERIES: u32 = 100;
const MAX_SAMPLE: u32 = 50_000;
const KEY_SEPARATOR: &str = "\u{1f}";

pub struct TableColumn {
    pub name: String,
    pub vector_dims: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorColumn {
    pub name: String,
    pub dims: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorIndex {
    pub name: String,
    pub column: String,
    pub metric: String,
    pub m: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorInfo {
    pub supported: bool,
    pub columns: Vec<VectorColumn>,
    pub indexes: Vec<VectorIndex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    Auto,
    Cosine,
    Euclidean,
}

impl Metric {
    fn function(self) -> &'static str {
        match self {
            Self::Auto => "VEC_DISTANCE",
            Self::Cosine => "VEC_DISTANCE_COSINE",
            Self::Euclidean => "VEC_DISTANCE_EUCLIDEAN",
        }
    }

    fn of_index(index: &VectorIndex) -> Self {
        if index.metric.eq_ignore_ascii_case("cosine") {
            Self::Cosine
        } else {
            Self::Euclidean
        }
    }
}

/// The query vector of a search: a row's vector, a typed literal, or floats already computed (an embedded text).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SearchQuery {
    Row { key: Vec<KeyCell> },
    Literal { text: String },
    Vector { values: Vec<f32> },
}

/// `vector.search` arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchArgs {
    pub database: String,
    pub table: String,
    pub column: String,
    pub query: SearchQuery,
    pub k: u32,
    pub metric: Metric,
    pub filter: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TableArgs {
    database: String,
    table: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchArgs {
    database: String,
    table: String,
    column: String,
    key: Vec<KeyCell>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthArgs {
    database: String,
    table: String,
    column: String,
    sample: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecallArgs {
    database: String,
    table: String,
    column: String,
    k: u32,
    queries: u32,
}

fn args<T: serde::de::DeserializeOwned>(name: &str, value: serde_json::Value) -> AppResult<T> {
    serde_json::from_value(value).map_err(|error| AppError::new(format!("Invalid arguments for {name}: {error}")))
}

fn json<T: Serialize>(value: T) -> AppResult<serde_json::Value> {
    Ok(serde_json::to_value(value)?)
}

/// Runs one vector operation on `conn`.
pub async fn invoke(conn: &mut Conn, name: &str, value: serde_json::Value) -> AppResult<serde_json::Value> {
    match name {
        "vector.info" => {
            let a: TableArgs = args(name, value)?;
            json(vector_info(conn, &a.database, &a.table).await?)
        }
        "vector.fetch" => {
            let a: FetchArgs = args(name, value)?;
            json(fetch_vector(conn, &a.database, &a.table, &a.column, &a.key).await?)
        }
        "vector.search" => {
            let a: SearchArgs = args(name, value)?;
            let query = match &a.query {
                SearchQuery::Row { key } => fetch_vector(conn, &a.database, &a.table, &a.column, key).await?,
                SearchQuery::Literal { text } => parse_literal(text)?,
                SearchQuery::Vector { values } => values.clone(),
            };
            json(search(conn, &a, &query).await?)
        }
        "vector.health" => {
            let a: HealthArgs = args(name, value)?;
            json(health(conn, &a.database, &a.table, &a.column, a.sample).await?)
        }
        "vector.recall" => {
            let a: RecallArgs = args(name, value)?;
            json(index_recall(conn, &a.database, &a.table, &a.column, a.k, a.queries).await?)
        }
        other => Err(crate::contract::unsupported_capability(other)),
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

/// Columns of a table whose vectors are known (`DATA_TYPE = 'vector'`), or a guess from result metadata.
pub enum VectorHint<'a> {
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

fn text_column(column: &Column, hint: &VectorHint<'_>) -> TextColumn {
    let column_type = column.column_type();
    let vector_dims = hint.dims(column);
    let binary_charset = column.character_set() == BINARY_CHARSET;
    let byte_string = matches!(
        column_type,
        ColumnType::MYSQL_TYPE_STRING
            | ColumnType::MYSQL_TYPE_VAR_STRING
            | ColumnType::MYSQL_TYPE_VARCHAR
            | ColumnType::MYSQL_TYPE_TINY_BLOB
            | ColumnType::MYSQL_TYPE_BLOB
            | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
            | ColumnType::MYSQL_TYPE_LONG_BLOB
            | ColumnType::MYSQL_TYPE_BIT
    );
    TextColumn {
        name: column.name_str().into_owned(),
        type_name: if vector_dims.is_some() {
            "vector".to_owned()
        } else {
            sql_type_name(column_type, binary_charset, column.flags()).to_owned()
        },
        numeric: is_numeric(column_type),
        binary: vector_dims.is_none() && binary_charset && byte_string,
        primary: column.flags().contains(mysql_async::consts::ColumnFlags::PRI_KEY_FLAG),
        vector_dims,
    }
}

/// How a cell is shown.
#[derive(Clone, Copy)]
enum CellFormat {
    Text,
    Hex,
    Vector,
}

impl CellFormat {
    fn of(column: &TextColumn) -> Self {
        if column.vector_dims.is_some() {
            Self::Vector
        } else if column.binary {
            Self::Hex
        } else {
            Self::Text
        }
    }
}

fn cell_text(value: Value, format: CellFormat) -> Option<String> {
    match value {
        Value::Bytes(bytes) => Some(match format {
            CellFormat::Vector => decode_vector(&bytes)
                .map(|values| summarize(&values))
                .unwrap_or_else(|| hex(&bytes)),
            CellFormat::Hex => hex(&bytes),
            CellFormat::Text => String::from_utf8(bytes).unwrap_or_else(|error| hex(error.as_bytes())),
        }),
        other => plain_text(other),
    }
}

fn row_cells(row: Row, formats: &[CellFormat]) -> Vec<Option<String>> {
    row.unwrap()
        .into_iter()
        .enumerate()
        .map(|(index, value)| cell_text(value, formats.get(index).copied().unwrap_or(CellFormat::Text)))
        .collect()
}

/// Every result set of a query, as text cells.
async fn read_result_sets<P: Protocol>(mut result: QueryResult<'_, 'static, P>, hint: &VectorHint<'_>) -> AppResult<Vec<ResultSet>> {
    let mut sets = Vec::new();
    while let Some(columns) = result.columns() {
        let columns: Vec<TextColumn> = columns.iter().map(|column| text_column(column, hint)).collect();
        let affected_rows = result.affected_rows();
        let formats: Vec<CellFormat> = columns.iter().map(CellFormat::of).collect();
        let mut rows = Vec::new();
        while let Some(row) = result.next().await? {
            rows.push(row_cells(row, &formats));
        }
        sets.push(ResultSet {
            columns,
            rows,
            affected_rows,
            truncated: false,
        });
    }
    // A failed later statement is held as a pending error that `columns()` does not report.
    result.next().await?;
    Ok(sets)
}

fn where_clause(filter: Option<&str>) -> String {
    filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .map(|filter| format!(" WHERE {filter}"))
        .unwrap_or_default()
}

async fn primary_key(conn: &mut Conn, database: &str, table: &str) -> AppResult<Vec<String>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY' ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows.iter().map(|row| row.text(0)).collect())
}

async fn binary_columns(conn: &mut Conn, database: &str, table: &str) -> AppResult<HashSet<String>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         AND DATA_TYPE IN ('binary', 'varbinary', 'tinyblob', 'blob', 'mediumblob', 'longblob', 'bit')",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows.iter().map(|row| row.text(0)).collect())
}

fn key_condition(key: &[KeyCell]) -> AppResult<String> {
    if key.is_empty() {
        return Err(AppError::new("Cannot identify the row"));
    }
    Ok(key
        .iter()
        .map(|cell| format!("{} <=> ?", quote_ident(&cell.column)))
        .collect::<Vec<_>>()
        .join(" AND "))
}

fn bind_values(key: &[KeyCell], binary: &HashSet<String>) -> Vec<Value> {
    key.iter()
        .map(|cell| match &cell.value {
            None => Value::NULL,
            Some(text) => {
                let decoded = if binary.contains(&cell.column) {
                    crate::contract::value::unhex(text, false)
                } else {
                    None
                };
                Value::Bytes(decoded.unwrap_or_else(|| text.as_bytes().to_vec()))
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSearchResult {
    pub result: ResultSet,
    pub plan: ResultSet,
    pub index_used: Option<String>,
    pub sql: String,
    pub runnable_sql: String,
    pub elapsed_ms: u64,
    pub query_dims: usize,
    pub primary_key: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheck {
    pub label: String,
    pub status: CheckStatus,
    pub value: String,
    pub detail: String,
}

impl HealthCheck {
    fn new(label: &str, status: CheckStatus, value: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            status,
            value: value.into(),
            detail: detail.into(),
        }
    }

    fn failed(label: &str, error: &AppError) -> Self {
        Self::new(label, CheckStatus::Error, "check failed", error.to_string())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallReport {
    pub index: String,
    pub metric: String,
    pub k: u32,
    pub queries: usize,
    pub recall: f64,
    pub worst_recall: f64,
    pub ann_ms: f64,
    pub exact_ms: f64,
}

#[derive(Default)]
struct SampleStats {
    total: usize,
    invalid_length: usize,
    non_finite: usize,
    zero: usize,
    duplicates: usize,
    norm_min: f32,
    norm_max: f32,
    norm_sum: f64,
}

impl SampleStats {
    fn collect(vectors: &[Vec<u8>], dims: u32) -> Self {
        let mut stats = Self {
            norm_min: f32::INFINITY,
            norm_max: 0.0,
            ..Self::default()
        };
        let mut seen = HashSet::new();
        for bytes in vectors {
            stats.total += 1;
            if !seen.insert(bytes.as_slice()) {
                stats.duplicates += 1;
            }
            let Some(values) = decode_vector(bytes).filter(|values| values.len() == dims as usize) else {
                stats.invalid_length += 1;
                continue;
            };
            if values.iter().any(|value| !value.is_finite()) {
                stats.non_finite += 1;
                continue;
            }
            let length = norm(&values);
            if length == 0.0 {
                stats.zero += 1;
            }
            stats.norm_min = stats.norm_min.min(length);
            stats.norm_max = stats.norm_max.max(length);
            stats.norm_sum += f64::from(length);
        }
        stats
    }

    fn measured(&self) -> usize {
        self.total - self.invalid_length - self.non_finite
    }

    fn normalized(&self) -> bool {
        (self.norm_min - 1.0).abs() <= NORM_TOLERANCE && (self.norm_max - 1.0).abs() <= NORM_TOLERANCE
    }
}

pub async fn table_columns(conn: &mut Conn, database: &str, table: &str) -> AppResult<Vec<TableColumn>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows
        .iter()
        .map(|row| TableColumn {
            name: row.text(0),
            vector_dims: row
                .text(1)
                .eq_ignore_ascii_case("vector")
                .then(|| parse_dims(&row.text(2)))
                .flatten(),
        })
        .collect())
}

pub async fn vector_columns(conn: &mut Conn, database: &str, table: &str) -> AppResult<HashMap<String, u32>> {
    Ok(table_columns(conn, database, table)
        .await?
        .into_iter()
        .filter_map(|column| Some((column.name, column.vector_dims?)))
        .collect())
}

pub async fn vector_info(conn: &mut Conn, database: &str, table: &str) -> AppResult<VectorInfo> {
    let supported = conn.query_drop("SELECT VEC_FromText('[1]')").await.is_ok();
    let mut columns: Vec<VectorColumn> = vector_columns(conn, database, table)
        .await?
        .into_iter()
        .map(|(name, dims)| VectorColumn { name, dims })
        .collect();
    columns.sort_by(|left, right| left.name.cmp(&right.name));
    let indexes = vector_indexes(conn, database, table).await?;
    Ok(VectorInfo {
        supported,
        columns,
        indexes,
    })
}

async fn vector_indexes(conn: &mut Conn, database: &str, table: &str) -> AppResult<Vec<VectorIndex>> {
    let rows = text_rows(
        conn,
        "SELECT INDEX_NAME, COLUMN_NAME FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND INDEX_TYPE = 'VECTOR'",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ddl = create_statement(conn, database, table).await?.replace('`', "");
    Ok(rows
        .iter()
        .map(|row| {
            let name = row.text(0);
            let line = ddl.lines().find(|line| line.contains(&format!("KEY {name} ("))).unwrap_or_default();
            VectorIndex {
                metric: option_value(line, "DISTANCE=").unwrap_or("euclidean").to_lowercase(),
                m: option_value(line, " M=").and_then(|value| value.parse().ok()),
                column: row.text(1),
                name,
            }
        })
        .collect())
}

pub async fn fetch_vector(conn: &mut Conn, database: &str, table: &str, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
    let binary = binary_columns(conn, database, table).await?;
    let sql = format!(
        "SELECT {} FROM {} WHERE {} LIMIT 1",
        quote_ident(column),
        qualified(database, table),
        key_condition(key)?
    );
    let row: Option<Row> = conn.exec_first(sql, bind_values(key, &binary)).await?;
    match row.and_then(|row| row.unwrap().into_iter().next()) {
        Some(Value::Bytes(bytes)) => decode_vector(&bytes).ok_or_else(|| AppError::new("The value is not a valid vector")),
        Some(Value::NULL) => Err(AppError::new("This row has no vector (NULL)")),
        Some(_) => Err(AppError::new("The value is not a vector")),
        None => Err(AppError::new("Row not found")),
    }
}

pub async fn search(conn: &mut Conn, request: &SearchArgs, query: &[f32]) -> AppResult<VectorSearchResult> {
    let columns = table_columns(conn, &request.database, &request.table).await?;
    let dims = columns
        .iter()
        .find(|column| column.name == request.column)
        .and_then(|column| column.vector_dims)
        .ok_or_else(|| AppError::new(format!("{} is not a VECTOR column", request.column)))?;
    ensure_dims(query.len(), dims)?;
    let vectors: HashMap<String, u32> = columns
        .iter()
        .filter_map(|column| Some((column.name.clone(), column.vector_dims?)))
        .collect();
    let select_list = columns
        .iter()
        .filter(|column| column.vector_dims.is_none())
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>();
    let statement = SearchStatement {
        target: qualified(&request.database, &request.table),
        column: quote_ident(&request.column),
        function: request.metric.function(),
        select_list: select_list.join(", "),
        filter: where_clause(request.filter.as_deref()),
        k: request.k.clamp(1, MAX_K),
    };
    let runnable_sql = statement.render(&format!("VEC_FromText('{}')", vector_literal(query)));
    let plan = first_set(read_result_sets(conn.query_iter(format!("EXPLAIN {runnable_sql}")).await?, &VectorHint::Guess).await?);
    let started = Instant::now();
    let result = first_set(read_result_sets(conn.query_iter(runnable_sql.clone()).await?, &VectorHint::Known(&vectors)).await?);
    let elapsed_ms = elapsed_millis(started);
    let primary_key = primary_key(conn, &request.database, &request.table).await?;
    Ok(VectorSearchResult {
        index_used: index_in_plan(&plan),
        plan,
        result,
        sql: statement.render("VEC_FromText(?)"),
        runnable_sql,
        elapsed_ms,
        query_dims: query.len(),
        primary_key,
    })
}

pub async fn health(conn: &mut Conn, database: &str, table: &str, column: &str, sample: u32) -> AppResult<Vec<HealthCheck>> {
    let dims = vector_columns(conn, database, table)
        .await?
        .get(column)
        .copied()
        .ok_or_else(|| AppError::new(format!("{column} is not a VECTOR column")))?;
    let index = vector_indexes(conn, database, table)
        .await?
        .into_iter()
        .find(|index| index.column == column);
    let mut checks = vec![
        HealthCheck::new("Column", CheckStatus::Info, format!("VECTOR({dims})"), ""),
        index_check(index.as_ref()),
        count_check(conn, database, table, column).await,
    ];
    match sample_vectors(conn, database, table, column, sample).await {
        Ok(vectors) => checks.extend(sample_checks(&SampleStats::collect(&vectors, dims), dims, index.as_ref())),
        Err(error) => checks.push(HealthCheck::failed("Sample", &error)),
    }
    Ok(checks)
}

pub async fn index_recall(conn: &mut Conn, database: &str, table: &str, column: &str, k: u32, queries: u32) -> AppResult<RecallReport> {
    let index = vector_indexes(conn, database, table)
        .await?
        .into_iter()
        .find(|index| index.column == column)
        .ok_or_else(|| AppError::new("There is no vector index on this column, so there is nothing to compare"))?;
    let key = primary_key(conn, database, table).await?;
    if key.is_empty() {
        return Err(AppError::new("Measuring recall needs a primary key to compare results"));
    }
    let k = k.clamp(1, MAX_RECALL_K);
    let target = qualified(database, table);
    let vector_column = quote_ident(column);
    let key_list = key.iter().map(|name| quote_ident(name)).collect::<Vec<_>>().join(", ");
    let function = Metric::of_index(&index).function();
    let samples: Vec<Row> = conn
        .query(format!(
            "SELECT {vector_column} FROM {target} WHERE {vector_column} IS NOT NULL ORDER BY RAND() LIMIT {}",
            queries.clamp(1, MAX_RECALL_QUERIES)
        ))
        .await?;
    let probes: Vec<Vec<f32>> = samples.into_iter().filter_map(first_vector).collect();
    if probes.is_empty() {
        return Err(AppError::new("The table has no vectors to sample"));
    }
    let mut recalls = Vec::with_capacity(probes.len());
    let (mut ann_total, mut exact_total) = (0.0, 0.0);
    for probe in &probes {
        let ordering = format!(
            "ORDER BY {function}({vector_column}, VEC_FromText('{}')) LIMIT {k}",
            vector_literal(probe)
        );
        let (approximate, ann_ms) = timed_keys(conn, format!("SELECT {key_list} FROM {target} {ordering}")).await?;
        let (exact, exact_ms) = timed_keys(
            conn,
            format!(
                "SELECT {key_list} FROM {target} IGNORE INDEX ({}) {ordering}",
                quote_ident(&index.name)
            ),
        )
        .await?;
        let hits = exact.iter().filter(|row_key| approximate.contains(*row_key)).count();
        recalls.push(if exact.is_empty() { 1.0 } else { hits as f64 / exact.len() as f64 });
        ann_total += ann_ms;
        exact_total += exact_ms;
    }
    let count = recalls.len() as f64;
    Ok(RecallReport {
        metric: index.metric.clone(),
        index: index.name,
        k,
        queries: recalls.len(),
        recall: recalls.iter().sum::<f64>() / count,
        worst_recall: recalls.iter().copied().fold(1.0, f64::min),
        ann_ms: ann_total / count,
        exact_ms: exact_total / count,
    })
}

pub fn summarize(values: &[f32]) -> String {
    let preview = values
        .iter()
        .take(PREVIEW_VALUES)
        .map(|value| format!("{value:.4}"))
        .collect::<Vec<_>>()
        .join(", ");
    let more = if values.len() > PREVIEW_VALUES { ", …" } else { "" };
    format!("[{}] ‖{:.3}‖ {preview}{more}", values.len(), norm(values))
}

pub fn ensure_dims(actual: usize, expected: u32) -> AppResult<()> {
    if actual == expected as usize {
        Ok(())
    } else {
        Err(AppError::new(format!(
            "The query vector has {actual} dimensions but the column stores {expected}"
        )))
    }
}

fn norm(values: &[f32]) -> f32 {
    values.iter().map(|value| value * value).sum::<f32>().sqrt()
}

fn parse_dims(column_type: &str) -> Option<u32> {
    column_type.split_once('(')?.1.trim_end_matches(')').trim().parse().ok()
}

/// `DISTANCE=cosine` or, as MariaDB 11.8 writes it, `DISTANCE='cosine'` (backticks already removed).
fn option_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let start = line.find(name)? + name.len();
    line[start..]
        .trim_start_matches(['\'', '"'])
        .split(|character: char| !character.is_ascii_alphanumeric())
        .next()
        .filter(|value| !value.is_empty())
}

fn first_set(sets: Vec<ResultSet>) -> ResultSet {
    sets.into_iter().next().unwrap_or_default()
}

fn index_in_plan(plan: &ResultSet) -> Option<String> {
    let key_column = plan.columns.iter().position(|column| column.name.eq_ignore_ascii_case("key"))?;
    plan.rows.iter().find_map(|row| row.get(key_column).cloned().flatten())
}

fn first_vector(row: Row) -> Option<Vec<f32>> {
    match row.unwrap().into_iter().next() {
        Some(Value::Bytes(bytes)) => decode_vector(&bytes),
        _ => None,
    }
}

async fn timed_keys(conn: &mut Conn, sql: String) -> AppResult<(HashSet<String>, f64)> {
    let started = Instant::now();
    let rows: Vec<Row> = conn.query(sql).await?;
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let keys = rows
        .into_iter()
        .map(|row| {
            row_cells(row, &[])
                .into_iter()
                .map(Option::unwrap_or_default)
                .collect::<Vec<_>>()
                .join(KEY_SEPARATOR)
        })
        .collect();
    Ok((keys, elapsed))
}

async fn sample_vectors(conn: &mut Conn, database: &str, table: &str, column: &str, sample: u32) -> AppResult<Vec<Vec<u8>>> {
    let vector_column = quote_ident(column);
    let rows: Vec<Row> = conn
        .query(format!(
            "SELECT {vector_column} FROM {} WHERE {vector_column} IS NOT NULL LIMIT {}",
            qualified(database, table),
            sample.clamp(1, MAX_SAMPLE)
        ))
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match row.unwrap().into_iter().next() {
            Some(Value::Bytes(bytes)) => Some(bytes),
            _ => None,
        })
        .collect())
}

async fn count_check(conn: &mut Conn, database: &str, table: &str, column: &str) -> HealthCheck {
    let sql = format!(
        "SELECT COUNT(*), COALESCE(SUM({} IS NULL), 0) FROM {}",
        quote_ident(column),
        qualified(database, table)
    );
    let counts: AppResult<Option<(u64, u64)>> = conn.query_first(sql).await.map_err(AppError::from);
    match counts {
        Ok(Some((total, missing))) if missing > 0 => HealthCheck::new(
            "Rows without a vector",
            CheckStatus::Warn,
            format!("{missing} of {total}"),
            "These rows never appear in similarity search results.",
        ),
        Ok(Some((total, _))) => HealthCheck::new("Rows", CheckStatus::Ok, format!("{total} rows, all with a vector"), ""),
        Ok(None) => HealthCheck::new("Rows", CheckStatus::Info, "0", "The table is empty."),
        Err(error) => HealthCheck::failed("Rows", &error),
    }
}

fn index_check(index: Option<&VectorIndex>) -> HealthCheck {
    match index {
        Some(index) => {
            let m = index.m.map_or_else(|| "default".to_owned(), |m| m.to_string());
            HealthCheck::new(
                "Vector index",
                CheckStatus::Ok,
                format!("{} · {} · M={m}", index.name, index.metric),
                "Searches must ORDER BY the matching distance function and use LIMIT to use this index.",
            )
        }
        None => HealthCheck::new(
            "Vector index",
            CheckStatus::Warn,
            "none",
            "Every similarity search scans the whole table. Add one with: ALTER TABLE … ADD VECTOR INDEX (column) DISTANCE=cosine",
        ),
    }
}

fn sample_checks(stats: &SampleStats, dims: u32, index: Option<&VectorIndex>) -> Vec<HealthCheck> {
    let mut checks = vec![HealthCheck::new(
        "Sample",
        CheckStatus::Info,
        format!("{} vectors read", stats.total),
        "",
    )];
    checks.push(counted_check(
        "Wrong length",
        stats.invalid_length,
        CheckStatus::Error,
        &format!("Vectors whose size is not {dims} floats."),
    ));
    checks.push(counted_check(
        "NaN or Infinity",
        stats.non_finite,
        CheckStatus::Error,
        "Distances to these vectors are meaningless.",
    ));
    checks.push(counted_check(
        "Zero vectors",
        stats.zero,
        CheckStatus::Error,
        "Cosine distance is undefined for all-zero vectors.",
    ));
    checks.push(counted_check(
        "Identical vectors",
        stats.duplicates,
        CheckStatus::Warn,
        "Exact duplicates in the sample, usually the same text embedded twice.",
    ));
    if stats.measured() > 0 {
        checks.push(norm_check(stats, index));
    }
    checks
}

fn counted_check(label: &str, count: usize, failing: CheckStatus, detail: &str) -> HealthCheck {
    if count == 0 {
        HealthCheck::new(label, CheckStatus::Ok, "none", "")
    } else {
        HealthCheck::new(label, failing, count.to_string(), detail)
    }
}

fn norm_check(stats: &SampleStats, index: Option<&VectorIndex>) -> HealthCheck {
    let mean = stats.norm_sum / stats.measured() as f64;
    let value = format!("min {:.3} · mean {mean:.3} · max {:.3}", stats.norm_min, stats.norm_max);
    let euclidean_index = index.is_some_and(|index| Metric::of_index(index) == Metric::Euclidean);
    if stats.normalized() {
        HealthCheck::new(
            "Vector length",
            CheckStatus::Ok,
            value,
            "Unit length: cosine and euclidean rank the same way.",
        )
    } else if euclidean_index {
        HealthCheck::new(
            "Vector length",
            CheckStatus::Warn,
            value,
            "Vectors are not unit length and the index uses euclidean distance, so longer vectors are penalised. Normalize them or use DISTANCE=cosine.",
        )
    } else {
        HealthCheck::new(
            "Vector length",
            CheckStatus::Info,
            value,
            "Not unit length. Fine for cosine distance.",
        )
    }
}

struct SearchStatement {
    target: String,
    column: String,
    function: &'static str,
    select_list: String,
    filter: String,
    k: u32,
}

impl SearchStatement {
    fn render(&self, query_vector: &str) -> String {
        let distance = format!("{}({}, {query_vector})", self.function, self.column);
        let separator = if self.select_list.is_empty() { "" } else { ", " };
        format!(
            "SELECT {distance} AS `vec_distance`{separator}{} FROM {}{} ORDER BY {distance} LIMIT {}",
            self.select_list, self.target, self.filter, self.k
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_arguments_keep_the_command_shape() {
        let args: SearchArgs = serde_json::from_value(serde_json::json!({
            "database": "shop", "table": "products", "column": "embedding",
            "query": { "kind": "row", "key": [ { "column": "id", "value": "7" } ] },
            "k": 5, "metric": "cosine", "filter": "stock > 0"
        }))
        .unwrap();
        assert!(matches!(&args.query, SearchQuery::Row { key } if key[0].value.as_deref() == Some("7")));
        assert_eq!(args.metric, Metric::Cosine);
        let vector: SearchQuery = serde_json::from_value(serde_json::json!({ "kind": "vector", "values": [0.5, 1.0] })).unwrap();
        assert!(matches!(vector, SearchQuery::Vector { values } if values == [0.5, 1.0]));
    }

    #[test]
    fn statements_and_cells_render_like_the_vector_view_expects() {
        let statement = SearchStatement {
            target: qualified("shop", "products"),
            column: quote_ident("embedding"),
            function: Metric::Cosine.function(),
            select_list: "`id`".into(),
            filter: where_clause(Some(" stock > 0 ")),
            k: 3,
        };
        assert_eq!(
            statement.render("VEC_FromText(?)"),
            "SELECT VEC_DISTANCE_COSINE(`embedding`, VEC_FromText(?)) AS `vec_distance`, `id` FROM `shop`.`products` \
             WHERE stock > 0 ORDER BY VEC_DISTANCE_COSINE(`embedding`, VEC_FromText(?)) LIMIT 3"
        );
        assert_eq!(vector_literal(&[0.5, 1.0]), "[0.5,1]");
        assert_eq!(summarize(&[3.0, 4.0]), "[2] ‖5.000‖ 3.0000, 4.0000");
        assert_eq!(cell_text(Value::Bytes(vec![0xab, 1]), CellFormat::Hex).as_deref(), Some("0xAB01"));
        assert!(ensure_dims(3, 4).is_err());
        let line = "VECTOR KEY idx_embedding (embedding) M='6' DISTANCE='cosine',";
        assert_eq!(option_value(line, "DISTANCE="), Some("cosine"));
        assert_eq!(option_value(line, " M="), Some("6"));
        assert_eq!(option_value("KEY i (e) M=8 DISTANCE=euclidean", " M="), Some("8"));
        assert_eq!(key_condition(&[]).unwrap_err().to_string(), "Cannot identify the row");
    }
}
