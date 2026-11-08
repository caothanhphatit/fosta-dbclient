//! MariaDB vector tooling (VECTOR columns, MariaDB 11.7+), served as the engine operations `vector.info`,
//! `vector.fetch`, `vector.search`, `vector.health` and `vector.recall` (`Connection::invoke`). Arguments and results are
//! the JSON shapes of the vector commands. Result sets are typed like query results (contract `ColumnInfo` and `Value`),
//! with vector cells summarised; key cells are typed values as the grid holds them.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use mysql_async::{prelude::*, Column, Conn, QueryResult, Row, Value};
use serde::{Deserialize, Serialize};

use super::{
    catalog::{create_statement, qualified, quote_ident, text_rows},
    values::{
        guess_vector_dims, plain_text, result_column, sql_type_name, to_param, to_value, vector_text as vector_literal, BINARY_CHARSET,
    },
};
use crate::{
    contract::{ColumnInfo, Value as Typed, ValueKind},
    error::{AppError, AppResult},
};

pub use super::values::{decode_vector, parse_vector as parse_literal};

mod catalog;
mod health;
mod rows;
mod search;

pub use catalog::*;
pub use health::*;
use rows::*;
pub use search::*;

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

/// A key cell of a row: the column and its typed value, as the grid holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyCell {
    pub column: String,
    pub value: Typed,
}

/// A result set with typed columns and values.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSet {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<Typed>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_arguments_keep_the_command_shape() {
        let args: SearchArgs = serde_json::from_value(serde_json::json!({
            "database": "shop", "table": "products", "column": "embedding",
            "query": { "kind": "row", "key": [ { "column": "id", "value": { "t": "int", "v": 7 } } ] },
            "k": 5, "metric": "cosine", "filter": "stock > 0"
        }))
        .unwrap();
        assert!(matches!(&args.query, SearchQuery::Row { key } if key[0].value == Typed::Int(7)));
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
        assert_eq!(key_text(Value::Bytes(vec![0xab, 0xff])).as_deref(), Some("0xABFF"));
        assert_eq!(
            bind_values(&[KeyCell {
                column: "id".into(),
                value: Typed::Int(7)
            }]),
            [Value::Int(7)]
        );
        assert!(ensure_dims(3, 4).is_err());
        let line = "VECTOR KEY idx_embedding (embedding) M='6' DISTANCE='cosine',";
        assert_eq!(option_value(line, "DISTANCE="), Some("cosine"));
        assert_eq!(option_value(line, " M="), Some("6"));
        assert_eq!(option_value("KEY i (e) M=8 DISTANCE=euclidean", " M="), Some("8"));
        assert_eq!(key_condition(&[]).unwrap_err().to_string(), "Cannot identify the row");
    }
}
