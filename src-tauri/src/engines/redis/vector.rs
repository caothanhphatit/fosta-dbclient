//! Redis vector tooling over the Query Engine (the `search` module, built into Redis 8): the [`VectorStore`] the shared
//! `vector.*` operations (`crate::contract::vector`) run on. A "table" is a search index (`FT._LIST`, `FT.INFO`), its
//! vector columns are the `VECTOR` fields (FLAT or HNSW, FLOAT32 / FLOAT64 / BFLOAT16) over HASH documents (binary
//! little-endian blobs) or JSON documents (arrays), and a row is a document, identified by its key (`__key`).
//!
//! A field's index serves one metric, its `DISTANCE_METRIC`: searches with it are KNN queries
//! (`*=>[KNN k @field $vec AS vec_distance]`); other metrics, and exact top-k on an HNSW field without a FLAT twin, scan
//! at most [`SCAN_LIMIT`] documents and compute distances here, on Redis's scale (cosine `1 - cos`, L2 squared,
//! inner product `1 - dot`). Search indexes exist in database 0 only. Every command only reads.

use std::{collections::HashMap, time::Instant};

use async_trait::async_trait;
use redis::Value as Reply;

use super::{
    connection::RedisEngine,
    types::{as_bytes, as_i64, as_items, as_pairs, as_string},
};
use crate::{
    contract::{
        vector::{
            KeyCell, Metric, NearestRequest, Neighbour, RowSet, SampleOrder, SearchRequest, SplitMix64, TableRef, VectorColumn,
            VectorIndex, VectorProfile, VectorSearchResult, VectorStore,
        },
        Badge, BuiltinAction, ColumnInfo, NodeAction, NodeKey, NodeKind, ObjectNode, ObjectRef, Tone, Value, ValueKind,
    },
    error::{AppError, AppResult},
};

pub use crate::contract::vector::OPERATIONS;

static PROFILE: VectorProfile = VectorProfile {
    metrics: &[Metric::Cosine, Metric::Euclidean, Metric::Dot],
    schemas: false,
    index_usage: "KNN queries (FT.SEARCH idx \"*=>[KNN k @field $vec]\") use the field's index with its DISTANCE_METRIC; other metrics scan the documents.",
    add_index: "Vector fields are always indexed: FT.CREATE idx ON HASH PREFIX 1 doc: SCHEMA embedding VECTOR HNSW 6 TYPE FLOAT32 DIM 4 DISTANCE_METRIC COSINE",
    euclidean_advice: "Normalize them or use DISTANCE_METRIC COSINE.",
};

/// The key column of every result: the document's Redis key.
pub const KEY_COLUMN: &str = "__key";
const DISTANCE: &str = "vec_distance";
/// The most documents an exact scan reads.
pub const SCAN_LIMIT: u32 = 50_000;
/// Documents read per `FT.SEARCH` page while scanning.
const PAGE: u32 = 1000;
/// The default HNSW `EF_RUNTIME`: a KNN query asks for at least `k`.
const DEFAULT_EF_RUNTIME: u32 = 10;

/// The module Redis 8 reports for the Query Engine (and RediSearch before it).
pub const SEARCH_MODULE: &str = "search";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Hash,
    Json,
}

/// One attribute of a search index.
#[derive(Debug, Clone, Default)]
struct Field {
    /// The hash field or JSON path.
    identifier: String,
    /// The name queries use (`@name`).
    attribute: String,
    kind: String,
    algorithm: String,
    data_type: String,
    dims: u32,
    distance_metric: String,
    m: Option<u32>,
    ef_construction: Option<u32>,
}

impl Field {
    fn is_vector(&self) -> bool {
        self.kind.eq_ignore_ascii_case("VECTOR")
    }

    fn metric(&self) -> Metric {
        Metric::of_name(&self.distance_metric).unwrap_or(Metric::Euclidean)
    }

    fn element(&self) -> Option<Element> {
        Element::of(&self.data_type)
    }
}

/// The element type of a vector field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Element {
    Float32,
    Float64,
    BFloat16,
}

impl Element {
    fn of(data_type: &str) -> Option<Self> {
        match data_type.to_ascii_uppercase().as_str() {
            "FLOAT32" => Some(Self::Float32),
            "FLOAT64" => Some(Self::Float64),
            "BFLOAT16" => Some(Self::BFloat16),
            _ => None,
        }
    }

    fn size(self) -> usize {
        match self {
            Self::Float32 => 4,
            Self::Float64 => 8,
            Self::BFloat16 => 2,
        }
    }

    /// The little-endian blob of `values`, as HASH fields and query parameters hold it.
    fn encode(self, values: &[f32]) -> Vec<u8> {
        let mut blob = Vec::with_capacity(values.len() * self.size());
        for value in values {
            match self {
                Self::Float32 => blob.extend_from_slice(&value.to_le_bytes()),
                Self::Float64 => blob.extend_from_slice(&f64::from(*value).to_le_bytes()),
                Self::BFloat16 => blob.extend_from_slice(&((value.to_bits() >> 16) as u16).to_le_bytes()),
            }
        }
        blob
    }

    fn decode(self, blob: &[u8], dims: u32) -> Option<Vec<f32>> {
        if blob.len() != dims as usize * self.size() {
            return None;
        }
        Some(
            blob.chunks_exact(self.size())
                .map(|chunk| match self {
                    Self::Float32 => f32::from_le_bytes(chunk.try_into().unwrap_or_default()),
                    Self::Float64 => f64::from_le_bytes(chunk.try_into().unwrap_or_default()) as f32,
                    Self::BFloat16 => f32::from_bits(u32::from(u16::from_le_bytes(chunk.try_into().unwrap_or_default())) << 16),
                })
                .collect(),
        )
    }
}

/// `FT.INFO`, as this module needs it.
#[derive(Debug, Clone)]
struct IndexInfo {
    source: Source,
    prefixes: Vec<String>,
    fields: Vec<Field>,
    docs: u64,
}

impl IndexInfo {
    fn parse(reply: Reply) -> Self {
        let mut info = IndexInfo {
            source: Source::Hash,
            prefixes: Vec::new(),
            fields: Vec::new(),
            docs: 0,
        };
        for (key, value) in named(reply) {
            match key.as_str() {
                "index_definition" => {
                    for (key, value) in named(value) {
                        match key.as_str() {
                            "key_type" if as_string(value.clone()).is_some_and(|kind| kind.eq_ignore_ascii_case("JSON")) => {
                                info.source = Source::Json;
                            }
                            "prefixes" => info.prefixes = as_items(value).into_iter().filter_map(as_string).collect(),
                            _ => {}
                        }
                    }
                }
                "attributes" => info.fields = as_items(value).into_iter().map(field).collect(),
                "num_docs" => info.docs = number(&value).unwrap_or_default(),
                _ => {}
            }
        }
        info.prefixes.retain(|prefix| !prefix.is_empty());
        info
    }

    fn vector(&self, name: &str) -> AppResult<&Field> {
        self.fields
            .iter()
            .find(|field| field.is_vector() && field.attribute == name)
            .ok_or_else(|| AppError::new(format!("{name} is not a vector field of this index")))
    }

    /// The keys a key cell may stand for: itself when it carries an index prefix, otherwise each prefix + it.
    fn candidate_keys(&self, text: &str) -> Vec<String> {
        if self.prefixes.is_empty() || self.prefixes.iter().any(|prefix| text.starts_with(prefix.as_str())) {
            return vec![text.to_owned()];
        }
        self.prefixes.iter().map(|prefix| format!("{prefix}{text}")).collect()
    }
}

/// Pairs of a reply with their keys as text (lower-cased).
fn named(reply: Reply) -> Vec<(String, Reply)> {
    as_pairs(reply)
        .into_iter()
        .filter_map(|(key, value)| as_string(key).map(|key| (key.to_ascii_lowercase(), value)))
        .collect()
}

fn number(value: &Reply) -> Option<u64> {
    as_i64(value)
        .and_then(|number| u64::try_from(number).ok())
        .or_else(|| as_string(value.clone())?.trim().parse::<f64>().ok().map(|number| number as u64))
}

fn field(reply: Reply) -> Field {
    let mut field = Field::default();
    for (key, value) in named(reply) {
        let text = || as_string(value.clone()).unwrap_or_default();
        match key.as_str() {
            "identifier" => field.identifier = text(),
            "attribute" => field.attribute = text(),
            "type" => field.kind = text(),
            "algorithm" => field.algorithm = text(),
            "data_type" => field.data_type = text(),
            "dim" => field.dims = number(&value).and_then(|dims| u32::try_from(dims).ok()).unwrap_or_default(),
            "distance_metric" => field.distance_metric = text(),
            "m" => field.m = number(&value).and_then(|m| u32::try_from(m).ok()),
            "ef_construction" => field.ef_construction = number(&value).and_then(|ef| u32::try_from(ef).ok()),
            _ => {}
        }
    }
    if field.attribute.is_empty() {
        field.attribute = field.identifier.clone();
    }
    field
}

/// A vector stored in a JSON document: `[1,2]`, or `[[1,2]]` from a `$` path.
fn json_vector(text: &str) -> Option<Vec<f32>> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let items = match value.as_array()?.as_slice() {
        [serde_json::Value::Array(inner)] => inner.clone(),
        items => items.to_vec(),
    };
    items.iter().map(|item| item.as_f64().map(|number| number as f32)).collect()
}

/// A distance on Redis's scale: cosine `1 - cos`, euclidean squared, inner product `1 - dot`.
fn distance(metric: Metric, left: &[f32], right: &[f32]) -> f64 {
    let dot: f64 = left.iter().zip(right).map(|(a, b)| f64::from(*a) * f64::from(*b)).sum();
    match metric {
        Metric::Euclidean => left.iter().zip(right).map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2)).sum(),
        Metric::Dot => 1.0 - dot,
        Metric::Cosine | Metric::Auto => {
            let norm = |values: &[f32]| values.iter().map(|value| f64::from(*value).powi(2)).sum::<f64>().sqrt();
            1.0 - dot / (norm(left) * norm(right))
        }
    }
}

/// `"\x00\x01…"`: a blob as a console argument.
fn escaped(blob: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(2 + blob.len() * 4);
    text.push('"');
    for byte in blob {
        let _ = write!(text, "\\x{byte:02x}");
    }
    text.push('"');
    text
}

/// A query argument for the console: quoted when it has spaces or quotes.
fn console_arg(text: &str) -> String {
    if !text.is_empty() && !text.contains(|character: char| character.is_whitespace() || character == '"' || character == '\'') {
        return text.to_owned();
    }
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `*` or `(filter)`: the documents a query looks at.
fn base_query(filter: Option<&str>) -> String {
    filter.map_or_else(|| "*".to_owned(), |filter| format!("({filter})"))
}

fn knn_query(filter: Option<&str>, field: &Field, k: u32) -> String {
    let ef = if field.algorithm.eq_ignore_ascii_case("HNSW") {
        format!(" EF_RUNTIME {}", k.max(DEFAULT_EF_RUNTIME))
    } else {
        String::new()
    };
    format!("{}=>[KNN {k} @{} $vec{ef} AS {DISTANCE}]", base_query(filter), field.attribute)
}

/// One document of an `FT.SEARCH` reply: its key and returned fields.
struct Document {
    key: String,
    fields: HashMap<String, Reply>,
}

/// `[total, key, [field, value, …], key, …]`, or `[total, key, key, …]` with NOCONTENT.
fn documents(reply: Reply, content: bool) -> (u64, Vec<Document>) {
    let mut items = as_items(reply).into_iter();
    let total = items.next().as_ref().and_then(number).unwrap_or_default();
    let mut documents = Vec::new();
    while let Some(key) = items.next() {
        let fields = if content {
            as_pairs(items.next().unwrap_or(Reply::Nil))
                .into_iter()
                .filter_map(|(name, value)| as_string(name).map(|name| (name, value)))
                .collect()
        } else {
            HashMap::new()
        };
        documents.push(Document {
            key: as_string(key).unwrap_or_default(),
            fields,
        });
    }
    (total, documents)
}

/// Builds `RETURN n identifier AS attribute …`.
fn return_clause(fields: &[&Field], distance: bool) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if distance {
        args.push(DISTANCE.to_owned());
    }
    for field in fields {
        args.extend([field.identifier.clone(), "AS".to_owned(), field.attribute.clone()]);
    }
    let mut clause = vec!["RETURN".to_owned(), args.len().to_string()];
    clause.extend(args);
    clause
}

pub struct RedisVectors {
    engine: RedisEngine,
    /// Whether the server has the Query Engine.
    search: bool,
    cache: Option<(String, IndexInfo)>,
}

impl RedisVectors {
    /// `engine` is a RESP2 connection to database 0, where search indexes live.
    pub fn new(engine: RedisEngine, search: bool) -> Self {
        Self {
            engine,
            search,
            cache: None,
        }
    }

    async fn info(&mut self, table: &TableRef) -> AppResult<IndexInfo> {
        if let Some((name, info)) = &self.cache {
            if *name == table.table {
                return Ok(info.clone());
            }
        }
        let reply = self
            .engine
            .query(redis::cmd("FT.INFO").arg(&table.table))
            .await
            .map_err(|error| AppError::new(format!("Search index {} not found: {error}", table.table)))?;
        let info = IndexInfo::parse(reply);
        self.cache = Some((table.table.clone(), info.clone()));
        Ok(info)
    }

    /// `FT.SEARCH index query args… DIALECT 2`.
    async fn search_command(&self, index: &str, query: &str, args: &[String], blob: Option<&[u8]>) -> AppResult<Reply> {
        let mut cmd = redis::cmd("FT.SEARCH");
        cmd.arg(index).arg(query);
        if let Some(blob) = blob {
            cmd.arg("PARAMS").arg(2).arg("vec").arg(blob);
        }
        for arg in args {
            cmd.arg(arg);
        }
        cmd.arg("DIALECT").arg(2);
        self.engine.query(&cmd).await
    }

    /// Reads documents page by page (at most [`SCAN_LIMIT`]), handing each page to `each`; returns (read, total).
    async fn scan(&self, index: &str, query: &str, fields: &[&Field], mut each: impl FnMut(Vec<Document>)) -> AppResult<(u64, u64)> {
        let (mut read, mut total) = (0u64, 0u64);
        while read < u64::from(SCAN_LIMIT) {
            let size = PAGE.min(SCAN_LIMIT - read as u32);
            let mut args = if fields.is_empty() {
                vec!["NOCONTENT".to_owned()]
            } else {
                return_clause(fields, false)
            };
            args.extend(["LIMIT".to_owned(), read.to_string(), size.to_string()]);
            let (count, page) = documents(self.search_command(index, query, &args, None).await?, !fields.is_empty());
            total = count;
            let len = page.len() as u64;
            each(page);
            read += len;
            if len < u64::from(size) || read >= total {
                break;
            }
        }
        Ok((read, total))
    }

    /// The document keys (at most [`SCAN_LIMIT`]).
    async fn keys(&self, index: &str) -> AppResult<Vec<String>> {
        let mut keys = Vec::new();
        self.scan(index, "*", &[], |page| keys.extend(page.into_iter().map(|document| document.key)))
            .await?;
        Ok(keys)
    }

    /// One document's vector, `None` when the key does not exist; an error when the vector does not decode.
    async fn load(&self, info: &IndexInfo, field: &Field, key: &str) -> AppResult<Option<Option<Vec<f32>>>> {
        let mut cmd = redis::cmd(match info.source {
            Source::Hash => "HGET",
            Source::Json => "JSON.GET",
        });
        cmd.arg(key).arg(&field.identifier);
        // A key of another type (WRONGTYPE) is not a document of this index either.
        let reply = self.engine.query(&cmd).await.unwrap_or(Reply::Nil);
        if matches!(reply, Reply::Nil) {
            let present = as_i64(&self.engine.query(redis::cmd("EXISTS").arg(key)).await?).unwrap_or_default() > 0;
            return Ok(present.then_some(None));
        }
        Ok(Some(decode(info, field, reply)))
    }

    /// The vectors of `keys`, in order, through one pipeline.
    async fn load_many(&self, info: &IndexInfo, field: &Field, keys: &[String]) -> AppResult<Vec<Option<Vec<f32>>>> {
        let mut pipe = redis::pipe();
        for key in keys {
            match info.source {
                Source::Hash => pipe.cmd("HGET").arg(key).arg(&field.identifier),
                Source::Json => pipe.cmd("JSON.GET").arg(key).arg(&field.identifier),
            };
        }
        Ok(self
            .engine
            .pipeline(&mut pipe)
            .await?
            .into_iter()
            .map(|reply| decode(info, field, reply))
            .collect())
    }

    /// Top-k by an exact scan; `rows` keeps the documents' other fields for a result grid.
    async fn exact(
        &self,
        index: &str,
        info: &IndexInfo,
        field: &Field,
        metric: Metric,
        query: &[f32],
        filter: Option<&str>,
        k: u32,
        others: &[&Field],
    ) -> AppResult<(Vec<(f64, Document)>, u64, u64)> {
        let mut fields = vec![field];
        fields.extend_from_slice(others);
        let mut best: Vec<(f64, Document)> = Vec::new();
        let (read, total) = self
            .scan(index, &base_query(filter), &fields, |page| {
                for document in page {
                    let Some(vector) = document
                        .fields
                        .get(&field.attribute)
                        .cloned()
                        .and_then(|reply| decode(info, field, reply))
                    else {
                        continue;
                    };
                    let distance = distance(metric, &vector, query);
                    best.push((if distance.is_nan() { f64::INFINITY } else { distance }, document));
                }
                best.sort_by(|left, right| left.0.total_cmp(&right.0).then_with(|| left.1.key.cmp(&right.1.key)));
                best.truncate(k as usize);
            })
            .await?;
        Ok((best, read, total))
    }

    /// A KNN query's documents with their distances.
    async fn knn(
        &self,
        index: &str,
        field: &Field,
        query: &[f32],
        filter: Option<&str>,
        k: u32,
        others: &[&Field],
    ) -> AppResult<Vec<(f64, Document)>> {
        let blob = encode(field, query)?;
        let args = self.knn_args(others, k);
        let reply = self.search_command(index, &knn_query(filter, field, k), &args, Some(&blob)).await?;
        Ok(documents(reply, true)
            .1
            .into_iter()
            .map(|document| {
                let distance = document
                    .fields
                    .get(DISTANCE)
                    .and_then(|reply| as_string(reply.clone()))
                    .and_then(|text| text.trim().parse().ok())
                    .unwrap_or(f64::NAN);
                (distance, document)
            })
            .collect())
    }

    fn knn_args(&self, others: &[&Field], k: u32) -> Vec<String> {
        let mut args = vec!["SORTBY".to_owned(), DISTANCE.to_owned(), "ASC".to_owned()];
        args.extend(return_clause(others, true));
        args.extend(["LIMIT".to_owned(), "0".to_owned(), k.to_string()]);
        args
    }

    /// `FT.EXPLAINCLI` of a query, one line per row.
    async fn explain(&self, index: &str, query: &str, blob: Option<&[u8]>) -> Vec<String> {
        let mut cmd = redis::cmd("FT.EXPLAINCLI");
        cmd.arg(index).arg(query);
        if let Some(blob) = blob {
            cmd.arg("PARAMS").arg(2).arg("vec").arg(blob);
        }
        cmd.arg("DIALECT").arg(2);
        match self.engine.query(&cmd).await {
            Ok(reply) => as_items(reply)
                .into_iter()
                .filter_map(as_string)
                .filter(|line| !line.trim().is_empty())
                .collect(),
            Err(error) => vec![format!("FT.EXPLAINCLI failed: {error}")],
        }
    }
}

fn encode(field: &Field, query: &[f32]) -> AppResult<Vec<u8>> {
    field
        .element()
        .map(|element| element.encode(query))
        .ok_or_else(|| AppError::new(format!("Vector fields of type {} are not supported", field.data_type)))
}

fn decode(info: &IndexInfo, field: &Field, reply: Reply) -> Option<Vec<f32>> {
    let bytes = as_bytes(reply)?;
    match info.source {
        Source::Hash => field.element()?.decode(&bytes, field.dims),
        Source::Json => json_vector(&String::from_utf8_lossy(&bytes)),
    }
}

/// The database a table reference names (`0`, `db0`, empty for 0).
fn database_of(table: &TableRef) -> Option<i64> {
    let text = table.database.trim().trim_start_matches("db");
    if text.is_empty() {
        Some(0)
    } else {
        text.parse().ok()
    }
}

/// A result grid row: the distance, the key, then the other fields (numbers as floats).
fn result_row(distance: f64, document: &Document, others: &[&Field]) -> Vec<Value> {
    let mut row = vec![Value::Float(distance), Value::Text(document.key.clone())];
    row.extend(
        others
            .iter()
            .map(|field| match document.fields.get(&field.attribute).cloned().and_then(as_string) {
                None => Value::Null,
                Some(text) if field.kind.eq_ignore_ascii_case("NUMERIC") => text.parse().map(Value::Float).unwrap_or(Value::Text(text)),
                Some(text) => Value::Text(text),
            }),
    );
    row
}

fn result_columns(others: &[&Field]) -> Vec<ColumnInfo> {
    let mut columns = vec![
        ColumnInfo::result(DISTANCE, "DOUBLE", ValueKind::Float),
        ColumnInfo::result(KEY_COLUMN, "KEY", ValueKind::Text),
    ];
    columns.extend(others.iter().map(|field| {
        let kind = if field.kind.eq_ignore_ascii_case("NUMERIC") {
            ValueKind::Float
        } else {
            ValueKind::Text
        };
        ColumnInfo::result(&field.attribute, &field.kind, kind)
    }));
    columns
}

fn lines(lines: Vec<String>) -> RowSet {
    RowSet {
        columns: vec![ColumnInfo::result("plan", "TEXT", ValueKind::Text)],
        rows: lines.into_iter().map(|line| vec![Value::Text(line)]).collect(),
    }
}

#[async_trait]
impl VectorStore for RedisVectors {
    fn profile(&self) -> &'static VectorProfile {
        &PROFILE
    }

    async fn supported(&mut self, table: &TableRef) -> AppResult<bool> {
        Ok(self.search && database_of(table) == Some(0))
    }

    async fn columns(&mut self, table: &TableRef) -> AppResult<Vec<VectorColumn>> {
        Ok(self
            .info(table)
            .await?
            .fields
            .into_iter()
            .filter(|field| field.is_vector() && field.element().is_some() && field.dims > 0)
            .map(|field| VectorColumn {
                type_name: format!(
                    "VECTOR {} {} DIM {} {}",
                    field.algorithm, field.data_type, field.dims, field.distance_metric
                ),
                name: field.attribute,
                dims: field.dims,
            })
            .collect())
    }

    async fn indexes(&mut self, table: &TableRef) -> AppResult<Vec<VectorIndex>> {
        Ok(self
            .info(table)
            .await?
            .fields
            .into_iter()
            .filter(Field::is_vector)
            .map(|field| {
                let mut params = vec![format!("{} {}", field.data_type, field.distance_metric)];
                params.extend(field.m.map(|m| format!("M={m}")));
                params.extend(field.ef_construction.map(|ef| format!("EF_CONSTRUCTION={ef}")));
                VectorIndex {
                    name: table.table.clone(),
                    metric: match field.metric() {
                        Metric::Cosine => "cosine",
                        Metric::Dot => "dot",
                        _ => "euclidean",
                    }
                    .to_owned(),
                    kind: field.algorithm.to_ascii_lowercase(),
                    m: field.m,
                    params: params.join(", "),
                    column: field.attribute,
                }
            })
            .collect())
    }

    async fn primary_key(&mut self, _table: &TableRef) -> AppResult<Vec<String>> {
        Ok(vec![KEY_COLUMN.to_owned()])
    }

    async fn fetch(&mut self, table: &TableRef, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
        let text = match key {
            [cell] if cell.column == KEY_COLUMN => cell.value.display().ok_or_else(|| AppError::new("Cannot identify the document"))?,
            _ => return Err(AppError::new(format!("Documents are identified by their key ({KEY_COLUMN})"))),
        };
        let info = self.info(table).await?;
        let field = info.vector(column)?.clone();
        for candidate in info.candidate_keys(&text) {
            match self.load(&info, &field, &candidate).await? {
                Some(Some(vector)) => return Ok(vector),
                Some(None) => return Err(AppError::new(format!("{candidate} has no {column} vector that decodes"))),
                None => {}
            }
        }
        Err(AppError::new("Document not found"))
    }

    /// Documents in the index; documents without the field are not told apart (the index does not count them).
    async fn count(&mut self, table: &TableRef, _column: &VectorColumn) -> AppResult<(u64, u64)> {
        Ok((self.info(table).await?.docs, 0))
    }

    async fn sample(
        &mut self,
        table: &TableRef,
        column: &VectorColumn,
        limit: u32,
        order: SampleOrder<'_>,
    ) -> AppResult<Vec<Option<Vec<f32>>>> {
        let info = self.info(table).await?;
        let field = info.vector(&column.name)?.clone();
        let mut keys = match order {
            SampleOrder::Any => {
                let args = ["LIMIT".to_owned(), "0".to_owned(), limit.to_string(), "NOCONTENT".to_owned()];
                let reply = self.search_command(&table.table, "*", &args, None).await?;
                documents(reply, false).1.into_iter().map(|document| document.key).collect()
            }
            SampleOrder::ByKey(_) => {
                let mut keys = self.keys(&table.table).await?;
                keys.sort();
                keys
            }
            SampleOrder::Random => {
                let mut keys = self.keys(&table.table).await?;
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| elapsed.as_nanos() as u64);
                let mut rng = SplitMix64::new(seed);
                for i in (1..keys.len()).rev() {
                    keys.swap(i, rng.below(i + 1));
                }
                keys
            }
        };
        keys.truncate(limit as usize);
        let vectors = self.load_many(&info, &field, &keys).await?;
        Ok(vectors)
    }

    async fn search(&mut self, table: &TableRef, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult> {
        let info = self.info(table).await?;
        let field = info.vector(&request.column.name)?.clone();
        let others: Vec<&Field> = info.fields.iter().filter(|other| !other.is_vector()).collect();
        let metric = request.metric.resolve(request.index, field.metric());
        let index = &table.table;
        let started = Instant::now();
        let (found, plan, index_used, sql, runnable_sql) = if metric == field.metric() {
            let blob = encode(&field, request.query)?;
            let query = knn_query(request.filter, &field, request.k);
            let found = self.knn(index, &field, request.query, request.filter, request.k, &others).await?;
            let args = self
                .knn_args(&others, request.k)
                .iter()
                .map(|arg| console_arg(arg))
                .collect::<Vec<_>>()
                .join(" ");
            let render = |vec: &str| {
                format!(
                    "FT.SEARCH {} {} PARAMS 2 vec {vec} {args} DIALECT 2",
                    console_arg(index),
                    console_arg(&query)
                )
            };
            let plan = self.explain(index, &query, Some(&blob)).await;
            (found, plan, Some(index.clone()), render("$vec"), render(&escaped(&blob)))
        } else {
            let (found, read, total) = self
                .exact(index, &info, &field, metric, request.query, request.filter, request.k, &others)
                .await?;
            let plan = vec![
                format!(
                    "Exact scan: the index of {} serves {} distance only",
                    field.attribute, field.distance_metric
                ),
                format!("Read {read} of {total} documents (at most {SCAN_LIMIT}); distances computed by Fosta Studio on Redis's scale"),
            ];
            let sql = format!(
                "FT.SEARCH {} {} RETURN … LIMIT 0 {SCAN_LIMIT} DIALECT 2",
                console_arg(index),
                console_arg(&base_query(request.filter))
            );
            (found, plan, None, sql.clone(), sql)
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(VectorSearchResult {
            result: RowSet {
                columns: result_columns(&others),
                rows: found
                    .iter()
                    .map(|(distance, document)| result_row(*distance, document, &others))
                    .collect(),
            },
            plan: lines(plan),
            index_used,
            sql,
            runnable_sql,
            elapsed_ms,
            query_dims: request.query.len(),
            primary_key: vec![KEY_COLUMN.to_owned()],
        })
    }

    async fn nearest(&mut self, table: &TableRef, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>> {
        let info = self.info(table).await?;
        let field = info.vector(&request.column.name)?.clone();
        let flat = |candidate: &&Field| {
            candidate.is_vector()
                && candidate.algorithm.eq_ignore_ascii_case("FLAT")
                && candidate.identifier == field.identifier
                && candidate.distance_metric.eq_ignore_ascii_case(&field.distance_metric)
        };
        let found = match (request.exact, info.fields.iter().find(flat)) {
            (false, _) => self.knn(&table.table, &field, request.query, None, request.k, &[]).await?,
            (true, Some(flat)) => self.knn(&table.table, flat, request.query, None, request.k, &[]).await?,
            (true, None) => {
                self.exact(&table.table, &info, &field, field.metric(), request.query, None, request.k, &[])
                    .await?
                    .0
            }
        };
        Ok(found
            .into_iter()
            .map(|(distance, document)| Neighbour {
                key: document.key,
                distance,
            })
            .collect())
    }
}

/// Tree nodes for the search indexes with vector fields (database 0), opening the vector views.
pub async fn index_nodes(engine: &RedisEngine) -> Vec<ObjectNode> {
    let Ok(reply) = engine.query(&redis::cmd("FT._LIST")).await else {
        return Vec::new();
    };
    let mut nodes = Vec::new();
    for name in as_items(reply).into_iter().filter_map(as_string) {
        let Ok(info) = engine.query(redis::cmd("FT.INFO").arg(&name)).await.map(IndexInfo::parse) else {
            continue;
        };
        let vectors = info.fields.iter().filter(|field| field.is_vector()).count();
        if vectors == 0 {
            continue;
        }
        let object = ObjectRef::new(Some("0"), None, &name, NodeKind::SEARCH_INDEX);
        nodes.push(ObjectNode {
            key: NodeKey {
                id: format!("db:0/index:{name}"),
                kind: NodeKind::SEARCH_INDEX,
                object: Some(object.clone()),
            },
            label: name,
            kind: NodeKind::SEARCH_INDEX,
            icon: "vector".into(),
            object: Some(object),
            detail: Some(format!("{} documents", info.docs)),
            badges: vec![Badge {
                text: "search index".into(),
                tone: Tone::Neutral,
            }],
            expandable: false,
            default_view: Some("vectorSearch".into()),
            actions: vec![
                NodeAction::view("vectorSearch", "Vector search", "vectorSearch"),
                NodeAction::view("vectorHealth", "Vector health", "vectorHealth"),
                NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
            ],
        });
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(text: &str) -> Reply {
        Reply::BulkString(text.as_bytes().to_vec())
    }

    #[test]
    fn index_info_parses_vector_attributes() {
        let attribute = Reply::Array(
            [
                "identifier",
                "embedding",
                "attribute",
                "embedding",
                "type",
                "VECTOR",
                "algorithm",
                "HNSW",
                "data_type",
                "FLOAT32",
            ]
            .into_iter()
            .map(bulk)
            .chain([
                bulk("dim"),
                Reply::Int(4),
                bulk("distance_metric"),
                bulk("COSINE"),
                bulk("M"),
                Reply::Int(16),
            ])
            .collect(),
        );
        let name = Reply::Array(vec![
            bulk("identifier"),
            bulk("name"),
            bulk("attribute"),
            bulk("name"),
            bulk("type"),
            bulk("TEXT"),
        ]);
        let reply = Reply::Array(vec![
            bulk("index_name"),
            bulk("docs"),
            bulk("index_definition"),
            Reply::Array(vec![
                bulk("key_type"),
                bulk("HASH"),
                bulk("prefixes"),
                Reply::Array(vec![bulk("doc:")]),
            ]),
            bulk("attributes"),
            Reply::Array(vec![name, attribute]),
            bulk("num_docs"),
            bulk("12"),
        ]);
        let info = IndexInfo::parse(reply);
        assert_eq!((info.source, info.docs), (Source::Hash, 12));
        let field = info.vector("embedding").unwrap();
        assert_eq!((field.dims, field.m, field.metric()), (4, Some(16), Metric::Cosine));
        assert!(info.vector("name").is_err());
        assert_eq!(info.candidate_keys("7"), ["doc:7"]);
        assert_eq!(info.candidate_keys("doc:7"), ["doc:7"]);
    }

    #[test]
    fn blobs_and_json_decode() {
        let blob = Element::Float32.encode(&[1.0, -2.5]);
        assert_eq!(blob.len(), 8);
        assert_eq!(Element::Float32.decode(&blob, 2), Some(vec![1.0, -2.5]));
        assert_eq!(Element::Float32.decode(&blob, 3), None);
        assert_eq!(Element::Float64.decode(&Element::Float64.encode(&[0.5]), 1), Some(vec![0.5]));
        assert_eq!(
            Element::BFloat16.decode(&Element::BFloat16.encode(&[1.5, -2.0]), 2),
            Some(vec![1.5, -2.0])
        );
        assert_eq!(json_vector("[[1,2]]"), Some(vec![1.0, 2.0]));
        assert_eq!(json_vector("[3, 4.5]"), Some(vec![3.0, 4.5]));
        assert_eq!(json_vector("[\"a\"]"), None);
        assert_eq!(escaped(&[0, 255]), "\"\\x00\\xff\"");
    }

    #[test]
    fn distances_follow_redis() {
        // FT.SEARCH on Redis 8.2 with query [1, 1]: COSINE 0.2929 for [1, 0], L2 13 and IP -6 for [3, 4].
        assert!((distance(Metric::Cosine, &[1.0, 0.0], &[1.0, 1.0]) - 0.292_893).abs() < 1e-6);
        assert_eq!(distance(Metric::Euclidean, &[3.0, 4.0], &[1.0, 1.0]), 13.0);
        assert_eq!(distance(Metric::Dot, &[3.0, 4.0], &[1.0, 1.0]), -6.0);
    }

    #[test]
    fn queries_render() {
        let field = Field {
            attribute: "embedding".into(),
            algorithm: "HNSW".into(),
            ..Field::default()
        };
        assert_eq!(
            knn_query(None, &field, 50),
            "*=>[KNN 50 @embedding $vec EF_RUNTIME 50 AS vec_distance]"
        );
        assert_eq!(
            knn_query(Some("@city:{Lima}"), &field, 5),
            "(@city:{Lima})=>[KNN 5 @embedding $vec EF_RUNTIME 10 AS vec_distance]"
        );
        let name = Field {
            identifier: "$.name".into(),
            attribute: "name".into(),
            ..Field::default()
        };
        assert_eq!(
            return_clause(&[&name], true),
            ["RETURN", "4", "vec_distance", "$.name", "AS", "name"]
        );
        assert_eq!(console_arg("*=>[KNN 5 @v $vec AS d]"), "\"*=>[KNN 5 @v $vec AS d]\"");
        assert_eq!(console_arg("idx"), "idx");
        let (total, docs) = documents(Reply::Array(vec![Reply::Int(2), bulk("a"), bulk("b")]), false);
        assert_eq!((total, docs.len(), docs[1].key.as_str()), (2, 2, "b"));
        assert_eq!(database_of(&TableRef::parse("db0", "idx", false)), Some(0));
        assert_eq!(database_of(&TableRef::parse("3", "idx", false)), Some(3));
    }
}
