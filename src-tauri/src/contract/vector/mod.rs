//! Vector tooling shared by every engine with vector columns: the engine operations `vector.info`, `vector.fetch`,
//! `vector.search`, `vector.health` and `vector.recall` (answered through `Connection::invoke`), their JSON shapes, and
//! everything that does not depend on the database: argument checks, health statistics on sampled vectors and the
//! recall measurement. An engine implements [`VectorStore`] (list vector columns and indexes, fetch, sample, search,
//! nearest keys) and hands it to [`invoke`].

mod health;
mod recall;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use health::{CheckStatus, HealthCheck, SampleStats};
pub use recall::{pick_probes, recall_at_k, RecallReport, SplitMix64};

use super::{errors::unsupported_capability, ColumnInfo, Value};
use crate::error::{AppError, AppResult};

/// The engine operations [`invoke`] answers.
pub const OPERATIONS: [&str; 5] = ["vector.info", "vector.fetch", "vector.search", "vector.health", "vector.recall"];

pub const MAX_K: u32 = 1000;
pub const MAX_RECALL_K: u32 = 100;
pub const MAX_RECALL_QUERIES: u32 = 100;
pub const MAX_SAMPLE: u32 = 50_000;
/// Rows read (in key order) to pick seeded recall queries from.
pub const RECALL_POOL: u32 = 1000;
/// Joins the cells of a composite key in [`Neighbour::key`].
pub const KEY_SEPARATOR: &str = "\u{1f}";
const PREVIEW_VALUES: usize = 4;

/// A key cell of a row: the column and its typed value, as the grid holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyCell {
    pub column: String,
    pub value: Value,
}

/// The table an operation targets. The UI sends `schema.table` for engines with schemas; [`TableRef::parse`] splits it
/// when the engine says so ([`VectorProfile::schemas`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    pub database: String,
    pub schema: Option<String>,
    pub table: String,
}

impl TableRef {
    pub fn parse(database: &str, table: &str, schemas: bool) -> Self {
        let (schema, table) = match table.split_once('.') {
            Some((schema, name)) if schemas && !schema.is_empty() && !name.is_empty() => (Some(schema.to_owned()), name.to_owned()),
            _ => (None, table.to_owned()),
        };
        Self {
            database: database.to_owned(),
            schema,
            table,
        }
    }
}

/// A result set with typed columns and values (vector cells summarised).
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowSet {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorColumn {
    pub name: String,
    pub dims: u32,
    /// The column type as the engine writes it (`VECTOR(4)`, `vector(4)`, `halfvec(768)`).
    pub type_name: String,
    /// The distances this column can be searched with, for engines whose metric is fixed per column (a search index
    /// built for one similarity). Empty: the profile's metrics ([`VectorInfo::metrics`]).
    #[serde(default)]
    pub metrics: Vec<Metric>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorIndex {
    pub name: String,
    pub column: String,
    /// `cosine`, `euclidean` or `dot` (see [`Metric::of_name`]); engines may report others.
    pub metric: String,
    /// The index method (`mhnsw`, `hnsw`, `ivfflat`).
    pub kind: String,
    /// HNSW `M` when the engine has it.
    pub m: Option<u32>,
    /// The build options, for display (`M=6`, `m=16, ef_construction=64`).
    pub params: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorInfo {
    pub supported: bool,
    pub columns: Vec<VectorColumn>,
    pub indexes: Vec<VectorIndex>,
    /// The distance functions a search may ask for (besides `auto`).
    pub metrics: Vec<Metric>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Metric {
    /// The index's metric (the engine decides without an index).
    Auto,
    Cosine,
    Euclidean,
    /// Inner product.
    Dot,
}

impl Metric {
    pub fn of_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "cosine" => Some(Self::Cosine),
            "euclidean" | "l2" => Some(Self::Euclidean),
            "dot" | "ip" | "inner_product" => Some(Self::Dot),
            _ => None,
        }
    }

    /// The metric of an index (euclidean unless it says otherwise).
    pub fn of_index(index: &VectorIndex) -> Self {
        Self::of_name(&index.metric).unwrap_or(Self::Euclidean)
    }

    /// `Auto` becomes the index's metric, or `fallback` without an index.
    pub fn resolve(self, index: Option<&VectorIndex>, fallback: Metric) -> Self {
        match self {
            Self::Auto => index.map_or(fallback, Self::of_index),
            other => other,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cosine => "cosine",
            Self::Euclidean => "euclidean",
            Self::Dot => "inner product",
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

/// `vector.recall` arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallArgs {
    pub database: String,
    pub table: String,
    pub column: String,
    pub k: u32,
    pub queries: u32,
    /// Picks the queries from the first rows in key order with this seed, so a run can be repeated. Random rows without.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Compares exact search with itself: the recall must be 1.0 (a check of the measurement, for tests).
    #[serde(default)]
    pub self_check: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSearchResult {
    pub result: RowSet,
    pub plan: RowSet,
    pub index_used: Option<String>,
    pub sql: String,
    pub runnable_sql: String,
    pub elapsed_ms: u64,
    pub query_dims: usize,
    pub primary_key: Vec<String>,
}

/// A search for [`VectorStore::search`], checked: the column is a vector column of the query's dimensions, the metric
/// is one the engine has, `k` is clamped and the filter is trimmed (the gateway checked it is one condition).
pub struct SearchRequest<'a> {
    pub column: &'a VectorColumn,
    pub query: &'a [f32],
    pub k: u32,
    /// As asked; `Auto` may need [`Metric::resolve`] with `index`.
    pub metric: Metric,
    /// The vector index on the column, if any.
    pub index: Option<&'a VectorIndex>,
    pub filter: Option<&'a str>,
}

/// A top-k request for the recall measurement.
pub struct NearestRequest<'a> {
    pub column: &'a VectorColumn,
    /// The primary key columns ([`Neighbour::key`] joins their values).
    pub key: &'a [String],
    /// The index under test; its metric is the distance.
    pub index: &'a VectorIndex,
    pub query: &'a [f32],
    pub k: u32,
    /// Forces a brute-force scan (no index); results are ordered by distance, then key.
    pub exact: bool,
}

/// One row of a top-k result: its key cells as text (joined with [`KEY_SEPARATOR`]) and its distance.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    pub key: String,
    pub distance: f64,
}

/// Which rows [`VectorStore::sample`] reads (always rows whose vector is not NULL).
#[derive(Debug, Clone, Copy)]
pub enum SampleOrder<'a> {
    /// Whatever is cheapest.
    Any,
    Random,
    /// Ordered by these (primary key) columns, so the sample is stable.
    ByKey(&'a [String]),
}

/// What differs between engines in the shared texts and arguments.
pub struct VectorProfile {
    /// The metrics searches may name (besides `auto`).
    pub metrics: &'static [Metric],
    /// Whether table names arrive as `schema.table`.
    pub schemas: bool,
    /// How a search uses the index (health check detail).
    pub index_usage: &'static str,
    /// How to add an index (shown when there is none).
    pub add_index: &'static str,
    /// What to do about non-unit vectors under a euclidean index.
    pub euclidean_advice: &'static str,
}

/// An engine's access to its vector columns; [`invoke`] builds the `vector.*` operations on it.
#[async_trait]
pub trait VectorStore: Send {
    fn profile(&self) -> &'static VectorProfile;

    /// Whether this database can run vector functions at all (a server too old, an extension not installed here).
    async fn supported(&mut self, table: &TableRef) -> AppResult<bool>;

    /// The table's vector columns, in column order.
    async fn columns(&mut self, table: &TableRef) -> AppResult<Vec<VectorColumn>>;

    /// The table's vector (ANN) indexes.
    async fn indexes(&mut self, table: &TableRef) -> AppResult<Vec<VectorIndex>>;

    async fn primary_key(&mut self, table: &TableRef) -> AppResult<Vec<String>>;

    /// One row's vector, by key.
    async fn fetch(&mut self, table: &TableRef, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>>;

    /// All rows, and rows without a vector.
    async fn count(&mut self, table: &TableRef, column: &VectorColumn) -> AppResult<(u64, u64)>;

    /// Up to `limit` non-NULL vectors; `None` for a value that does not decode as a vector.
    async fn sample(
        &mut self,
        table: &TableRef,
        column: &VectorColumn,
        limit: u32,
        order: SampleOrder<'_>,
    ) -> AppResult<Vec<Option<Vec<f32>>>>;

    /// A nearest-neighbour search as the user asked it (through the index when the planner can), with its plan.
    async fn search(&mut self, table: &TableRef, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult>;

    /// Top-k keys and distances, through the index or exact.
    async fn nearest(&mut self, table: &TableRef, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>>;
}

fn args<T: serde::de::DeserializeOwned>(name: &str, value: serde_json::Value) -> AppResult<T> {
    serde_json::from_value(value).map_err(|error| AppError::new(format!("Invalid arguments for {name}: {error}")))
}

fn json<T: Serialize>(value: T) -> AppResult<serde_json::Value> {
    Ok(serde_json::to_value(value)?)
}

/// Runs one vector operation on `store`.
pub async fn invoke<S: VectorStore + ?Sized>(store: &mut S, name: &str, value: serde_json::Value) -> AppResult<serde_json::Value> {
    let schemas = store.profile().schemas;
    match name {
        "vector.info" => {
            let a: TableArgs = args(name, value)?;
            json(info(store, &TableRef::parse(&a.database, &a.table, schemas)).await?)
        }
        "vector.fetch" => {
            let a: FetchArgs = args(name, value)?;
            json(
                store
                    .fetch(&TableRef::parse(&a.database, &a.table, schemas), &a.column, &a.key)
                    .await?,
            )
        }
        "vector.search" => {
            let a: SearchArgs = args(name, value)?;
            json(search(store, &a).await?)
        }
        "vector.health" => {
            let a: HealthArgs = args(name, value)?;
            let table = TableRef::parse(&a.database, &a.table, schemas);
            json(health::health(store, &table, &a.column, a.sample).await?)
        }
        "vector.recall" => {
            let a: RecallArgs = args(name, value)?;
            json(recall::index_recall(store, &a).await?)
        }
        other => Err(unsupported_capability(other)),
    }
}

pub async fn info<S: VectorStore + ?Sized>(store: &mut S, table: &TableRef) -> AppResult<VectorInfo> {
    let supported = store.supported(table).await?;
    if !supported {
        return Ok(VectorInfo {
            supported,
            columns: Vec::new(),
            indexes: Vec::new(),
            metrics: Vec::new(),
        });
    }
    let mut columns = store.columns(table).await?;
    columns.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(VectorInfo {
        supported,
        columns,
        indexes: store.indexes(table).await?,
        metrics: store.profile().metrics.to_vec(),
    })
}

/// The vector column `name` of the table, or an error naming it.
pub async fn find_column<S: VectorStore + ?Sized>(store: &mut S, table: &TableRef, name: &str) -> AppResult<VectorColumn> {
    store
        .columns(table)
        .await?
        .into_iter()
        .find(|column| column.name == name)
        .ok_or_else(|| AppError::new(format!("{name} is not a vector column")))
}

/// Refuses a metric the engine does not have, or one outside the column's own list ([`VectorColumn::metrics`]).
pub fn check_metric(profile: &VectorProfile, column: &VectorColumn, metric: Metric) -> AppResult<()> {
    if metric == Metric::Auto {
        return Ok(());
    }
    if !profile.metrics.contains(&metric) {
        return Err(AppError::new(format!("This engine has no {} distance", metric.label())));
    }
    if !column.metrics.is_empty() && !column.metrics.contains(&metric) {
        let allowed = column.metrics.iter().map(|metric| metric.label()).collect::<Vec<_>>().join(" or ");
        return Err(AppError::new(format!(
            "{} is searched with {allowed} distance only (its similarity is fixed); pick {allowed} or auto",
            column.name
        )));
    }
    Ok(())
}

pub async fn search<S: VectorStore + ?Sized>(store: &mut S, request: &SearchArgs) -> AppResult<VectorSearchResult> {
    let profile = store.profile();
    let table = TableRef::parse(&request.database, &request.table, profile.schemas);
    let column = find_column(store, &table, &request.column).await?;
    check_metric(profile, &column, request.metric)?;
    let query = match &request.query {
        SearchQuery::Row { key } => store.fetch(&table, &column.name, key).await?,
        SearchQuery::Literal { text } => parse_vector(text)?,
        SearchQuery::Vector { values } => values.clone(),
    };
    ensure_dims(query.len(), column.dims)?;
    // A stored row or an embedding can hold NaN or an overflow; no engine ranks those meaningfully.
    ensure_finite(&query)?;
    let index = store.indexes(&table).await?.into_iter().find(|index| index.column == column.name);
    let filter = request.filter.as_deref().map(str::trim).filter(|filter| !filter.is_empty());
    let checked = SearchRequest {
        column: &column,
        query: &query,
        k: request.k.clamp(1, MAX_K),
        metric: request.metric,
        index: index.as_ref(),
        filter,
    };
    store.search(&table, &checked).await
}

/// `[0.1,0.2]`: the text form vector engines read and vector cells show.
pub fn vector_text(values: &[f32]) -> String {
    let joined = values.iter().map(f32::to_string).collect::<Vec<_>>().join(",");
    format!("[{joined}]")
}

/// `[0.1, 0.2]` or `0.1 0.2` as floats.
pub fn parse_vector(text: &str) -> AppResult<Vec<f32>> {
    let values = text
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f32>().map_err(|_| AppError::new(format!("'{part}' is not a number"))))
        .collect::<AppResult<Vec<_>>>()?;
    if values.is_empty() {
        return Err(AppError::new("The vector is empty"));
    }
    ensure_finite(&values)?;
    Ok(values)
}

/// A vector cell as the result grid shows it: dimensions, length and the first values.
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

/// Refuses a vector with NaN or an infinite value (`1e39` overflows f32 to infinity).
pub fn ensure_finite(values: &[f32]) -> AppResult<()> {
    match values.iter().position(|value| !value.is_finite()) {
        None => Ok(()),
        Some(position) => Err(AppError::new(format!(
            "The query vector has a value that is not a finite number ({}) at position {}",
            values[position],
            position + 1
        ))),
    }
}

pub fn norm(values: &[f32]) -> f32 {
    values.iter().map(|value| value * value).sum::<f32>().sqrt()
}

/// A search's WHERE clause (` WHERE …`, or nothing).
pub fn where_clause(filter: Option<&str>) -> String {
    filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .map(|filter| format!(" WHERE {filter}"))
        .unwrap_or_default()
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
        assert!(matches!(&args.query, SearchQuery::Row { key } if key[0].value == Value::Int(7)));
        assert_eq!(args.metric, Metric::Cosine);
        let vector: SearchQuery = serde_json::from_value(serde_json::json!({ "kind": "vector", "values": [0.5, 1.0] })).unwrap();
        assert!(matches!(vector, SearchQuery::Vector { values } if values == [0.5, 1.0]));
        let recall: RecallArgs = serde_json::from_value(serde_json::json!({
            "database": "d", "table": "t", "column": "c", "k": 5, "queries": 3
        }))
        .unwrap();
        assert_eq!((recall.seed, recall.self_check), (None, false));
        assert_eq!(serde_json::to_value(Metric::Dot).unwrap(), "dot");
    }

    #[test]
    fn helpers_format_and_check_vectors() {
        assert_eq!(vector_text(&[0.5, 1.0]), "[0.5,1]");
        assert_eq!(parse_vector(" [0.5, 1 ,2] ").unwrap(), [0.5, 1.0, 2.0]);
        assert_eq!(parse_vector("1 2").unwrap(), [1.0, 2.0]);
        assert!(parse_vector("[]").is_err());
        assert!(parse_vector("[a]").is_err());
        for text in ["[NaN, 1]", "[inf, 1]", "[1, -inf]", "[1e39, 1]"] {
            let error = parse_vector(text).unwrap_err().to_string();
            assert!(error.contains("not a finite number"), "{text}: {error}");
        }
        assert!(ensure_finite(&[f32::NAN]).is_err());
        assert!(ensure_finite(&[0.5, -1.0]).is_ok());
        assert_eq!(summarize(&[3.0, 4.0]), "[2] ‖5.000‖ 3.0000, 4.0000");
        assert!(summarize(&[1.0; 6]).ends_with(", …"));
        assert!(ensure_dims(3, 4).is_err());
        assert!(ensure_dims(4, 4).is_ok());
        assert_eq!(where_clause(Some(" stock > 0 ")), " WHERE stock > 0");
        assert_eq!(where_clause(Some("  ")), "");
    }

    #[test]
    fn tables_and_metrics_resolve() {
        let plain = TableRef::parse("shop", "public.products", false);
        assert_eq!((plain.schema, plain.table.as_str()), (None, "public.products"));
        let split = TableRef::parse("shop", "public.products", true);
        assert_eq!((split.schema.as_deref(), split.table.as_str()), (Some("public"), "products"));
        assert_eq!(TableRef::parse("shop", "products", true).schema, None);
        let index = VectorIndex {
            name: "i".into(),
            column: "c".into(),
            metric: "dot".into(),
            kind: "hnsw".into(),
            m: None,
            params: String::new(),
        };
        assert_eq!(Metric::Auto.resolve(Some(&index), Metric::Cosine), Metric::Dot);
        assert_eq!(Metric::Auto.resolve(None, Metric::Cosine), Metric::Cosine);
        assert_eq!(Metric::Euclidean.resolve(Some(&index), Metric::Cosine), Metric::Euclidean);
        assert_eq!(Metric::of_name("L2"), Some(Metric::Euclidean));
        assert_eq!(Metric::of_name("l1"), None);
    }

    #[test]
    fn a_column_with_fixed_metrics_refuses_the_others() {
        static PROFILE: VectorProfile = VectorProfile {
            metrics: &[Metric::Cosine, Metric::Euclidean, Metric::Dot],
            schemas: false,
            index_usage: "",
            add_index: "",
            euclidean_advice: "",
        };
        let column = |metrics: Vec<Metric>| VectorColumn {
            name: "embedding".into(),
            dims: 4,
            type_name: "dense_vector(4)".into(),
            metrics,
        };
        let open = column(Vec::new());
        for metric in [Metric::Auto, Metric::Cosine, Metric::Euclidean, Metric::Dot] {
            assert!(check_metric(&PROFILE, &open, metric).is_ok(), "{metric:?}");
        }
        let fixed = column(vec![Metric::Cosine]);
        assert!(check_metric(&PROFILE, &fixed, Metric::Auto).is_ok());
        assert!(check_metric(&PROFILE, &fixed, Metric::Cosine).is_ok());
        let refused = check_metric(&PROFILE, &fixed, Metric::Euclidean).unwrap_err().to_string();
        assert!(refused.contains("embedding is searched with cosine distance only"), "{refused}");
        let narrow = VectorProfile {
            metrics: &[Metric::Cosine],
            ..PROFILE
        };
        assert!(check_metric(&narrow, &open, Metric::Dot)
            .unwrap_err()
            .to_string()
            .contains("no inner product"));
        // The list travels as `metrics`; a column without one leaves it empty (the profile's metrics apply).
        let sent: VectorColumn = serde_json::from_value(serde_json::json!({ "name": "v", "dims": 2, "typeName": "vector(2)" })).unwrap();
        assert!(sent.metrics.is_empty());
        assert_eq!(serde_json::to_value(&fixed).unwrap()["metrics"], serde_json::json!(["cosine"]));
    }
}
