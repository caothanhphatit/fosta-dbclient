//! Vector tooling on Elasticsearch (`dense_vector`) and OpenSearch (`knn_vector`, k-NN plugin): the [`VectorStore`] the
//! shared `vector.*` operations (`crate::contract::vector`) run on. Every operation is a read (`_mapping`, `_settings`,
//! `_search`, `_count`) through the engine's client.
//!
//! - **Columns.** Vector fields of the mapping (top level and inside objects, as dotted paths; not inside `nested`).
//!   A field searchable by ANN has one fixed similarity (Elasticsearch `similarity`, OpenSearch `space_type`), so its
//!   column lists only that metric ([`VectorColumn::metrics`]) and is reported as a [`VectorIndex`] named after the
//!   field. A field without a search graph (`"index": false`, or an OpenSearch index without `index.knn: true`) is
//!   searched exactly with any metric.
//! - **Search.** ANN with the top-level `knn` section (Elasticsearch 8.4+; `num_candidates` = max(100, 2k), at most
//!   10,000) or the `knn` query clause (OpenSearch; the filter goes inside the clause for the lucene and faiss engines,
//!   in a `bool` filter otherwise). Exact search is `script_score` over the documents with a vector: the Painless
//!   vector functions on Elasticsearch, the plugin's `knn_score` script on OpenSearch.
//! - **Distances.** Scores differ by flavor, similarity and engine (and quantized graphs score approximately), so the
//!   hits return their vector in `_source` and the distance is computed here, as pgvector defines it: `1 - cosine`,
//!   L2, or the negative inner product. Rows are ordered by that distance.
//! - **Keys.** The grid's key: the declared key field (`_meta.fosta.key`), else `_id` (plus `_index` for aliases and
//!   patterns). [`SampleOrder::ByKey`] sorts by the declared key field; `_id` cannot be sorted on Elasticsearch, so a
//!   key on `_id` sorts by `_doc` (index order: stable while the index does not change, which is what seeded recall
//!   needs).
//! - **Limits.** Samples read at most 10,000 documents (`index.max_result_window`).

use std::time::Instant;

use async_trait::async_trait;
use reqwest::Method;
use serde_json::{json, Value as Json};

use super::{
    client::EsRequest,
    connection::{segment, EsEngine},
    dsl,
    grid::{self, Target, ID, INDEX},
    search::MAX_RESULT_WINDOW,
    values,
};
use crate::{
    contract::{
        vector::{
            self as shared, KeyCell, Metric, NearestRequest, Neighbour, RowSet, SampleOrder, SearchRequest, TableRef, VectorColumn,
            VectorIndex, VectorProfile, VectorSearchResult, VectorStore, KEY_SEPARATOR,
        },
        ColumnInfo, Value, ValueKind,
    },
    error::{AppError, AppResult},
};

static ELASTIC_PROFILE: VectorProfile = VectorProfile {
    metrics: &[Metric::Cosine, Metric::Euclidean, Metric::Dot],
    schemas: false,
    index_usage: "Searches with the knn section (k, num_candidates) use the field's graph; script_score reads every document.",
    add_index: "Map the field with \"index\": true and a similarity (changing the mapping needs a reindex).",
    euclidean_advice: "Normalize them or map the field with cosine similarity.",
};

static OPENSEARCH_PROFILE: VectorProfile = VectorProfile {
    metrics: &[Metric::Cosine, Metric::Euclidean, Metric::Dot],
    schemas: false,
    index_usage: "Searches with the knn query clause use the field's graph; the knn_score script reads every document.",
    add_index: "Create the index with \"index.knn\": true and a method on the field (changing it needs a reindex).",
    euclidean_advice: "Normalize them or use the cosinesimil space type.",
};

const DENSE_VECTOR: &str = "dense_vector";
const KNN_VECTOR: &str = "knn_vector";
const DISTANCE: &str = "vec_distance";
const QUERY_PLACEHOLDER: &str = "<query vector>";
/// The most candidates an Elasticsearch knn search may consider per shard.
const MAX_CANDIDATES: u32 = 10_000;

/// Whether the cluster has vector search: Elasticsearch 8+, OpenSearch with the k-NN plugin.
pub async fn cluster_supports(engine: &EsEngine) -> bool {
    if !engine.info().is_opensearch() {
        return engine.info().major >= 8;
    }
    match engine.client().get("/_cat/plugins?format=json&h=component").await {
        Ok(body) => has_knn_plugin(&body),
        Err(_) => false,
    }
}

fn has_knn_plugin(body: &Json) -> bool {
    body.as_array()
        .is_some_and(|rows| rows.iter().any(|row| row["component"] == "opensearch-knn"))
}

/// Runs one `vector.*` operation.
pub async fn invoke(engine: &EsEngine, name: &str, args: Json) -> AppResult<Json> {
    shared::invoke(&mut EsVectors::new(engine), name, args).await
}

/// A vector field of the mapping.
#[derive(Clone, Debug, PartialEq)]
struct VectorField {
    /// The field path (`embedding`, `meta.embedding`).
    name: String,
    dims: u32,
    field_type: &'static str,
    /// The similarity of the field's graph, as the engine names it (`cosine`, `l2_norm`, `cosinesimil`).
    similarity: Option<String>,
    metric: Option<Metric>,
    /// ANN search is possible on this field.
    indexed: bool,
    /// The graph type (`hnsw`, `int8_hnsw`, `flat`; the OpenSearch method name).
    kind: String,
    /// The OpenSearch method engine (`lucene`, `faiss`, `nmslib`), when the mapping names it.
    engine: Option<String>,
    m: Option<u32>,
    params: String,
}

impl VectorField {
    fn column(&self) -> VectorColumn {
        VectorColumn {
            name: self.name.clone(),
            dims: self.dims,
            type_name: format!("{}({})", self.field_type, self.dims),
            metrics: if self.indexed {
                self.metric.into_iter().collect()
            } else {
                Vec::new()
            },
        }
    }

    fn index(&self) -> Option<VectorIndex> {
        let metric = self.metric.filter(|_| self.indexed)?;
        Some(VectorIndex {
            name: self.name.clone(),
            column: self.name.clone(),
            metric: metric_name(metric).to_owned(),
            kind: self.kind.clone(),
            m: self.m,
            params: self.params.clone(),
        })
    }
}

fn metric_name(metric: Metric) -> &'static str {
    match metric {
        Metric::Auto | Metric::Cosine => "cosine",
        Metric::Euclidean => "euclidean",
        Metric::Dot => "dot",
    }
}

/// An Elasticsearch similarity or OpenSearch space type as a shared metric (`None`: l1, linf, hamming).
fn metric_of(similarity: &str) -> Option<Metric> {
    match similarity {
        "cosine" | "cosinesimil" => Some(Metric::Cosine),
        "l2_norm" | "l2" => Some(Metric::Euclidean),
        "dot_product" | "max_inner_product" | "innerproduct" => Some(Metric::Dot),
        _ => None,
    }
}

/// The OpenSearch space type of a metric.
fn space_type(metric: Metric) -> &'static str {
    match metric {
        Metric::Auto | Metric::Cosine => "cosinesimil",
        Metric::Euclidean => "l2",
        Metric::Dot => "innerproduct",
    }
}

fn options_text(options: &[(&str, String)]) -> String {
    options
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The vector fields of one index's mapping; `knn` is the OpenSearch `index.knn` setting.
fn vector_fields(mappings: &Json, knn: bool) -> Vec<VectorField> {
    let mut fields = Vec::new();
    collect_fields(&mappings["properties"], "", knn, &mut fields);
    fields
}

fn collect_fields(properties: &Json, prefix: &str, knn: bool, out: &mut Vec<VectorField>) {
    let Some(properties) = properties.as_object() else {
        return;
    };
    for (name, definition) in properties {
        let path = format!("{prefix}{name}");
        match definition["type"].as_str() {
            Some(DENSE_VECTOR) => out.extend(dense_vector(&path, definition)),
            Some(KNN_VECTOR) => out.extend(knn_vector(&path, definition, knn)),
            None | Some("object") => collect_fields(&definition["properties"], &format!("{path}."), knn, out),
            _ => {}
        }
    }
}

fn dims_of(value: &Json) -> Option<u32> {
    value.as_u64().and_then(|dims| u32::try_from(dims).ok()).filter(|dims| *dims > 0)
}

fn dense_vector(name: &str, definition: &Json) -> Option<VectorField> {
    // Bit vectors only have a hamming distance; dims are unknown until a first document on a field mapped without.
    if definition["element_type"] == "bit" {
        return None;
    }
    let dims = dims_of(&definition["dims"])?;
    let similarity = definition["similarity"].as_str().map(str::to_owned);
    let metric = similarity.as_deref().and_then(metric_of);
    let indexed = definition["index"].as_bool().unwrap_or(false) && metric.is_some();
    let options = &definition["index_options"];
    let kind = options["type"].as_str().unwrap_or(if indexed { "hnsw" } else { "none" }).to_owned();
    let m = options["m"].as_u64().and_then(|m| u32::try_from(m).ok());
    let mut params = Vec::new();
    if let Some(similarity) = &similarity {
        params.push(("similarity", similarity.clone()));
    }
    for option in ["m", "ef_construction", "confidence_interval"] {
        if let Some(value) = options.get(option).filter(|value| !value.is_null()) {
            params.push((option, value.to_string()));
        }
    }
    Some(VectorField {
        name: name.to_owned(),
        dims,
        field_type: DENSE_VECTOR,
        similarity,
        metric,
        indexed,
        kind,
        engine: None,
        m,
        params: options_text(&params),
    })
}

fn knn_vector(name: &str, definition: &Json, knn: bool) -> Option<VectorField> {
    if definition["data_type"] == "binary" {
        return None;
    }
    let dims = dims_of(&definition["dimension"])?;
    let method = &definition["method"];
    // The space type sits on the method or (2.17+) on the field; l2 is the default.
    let similarity = method["space_type"]
        .as_str()
        .or_else(|| definition["space_type"].as_str())
        .unwrap_or("l2")
        .to_owned();
    let metric = metric_of(&similarity);
    let engine = method["engine"].as_str().map(str::to_owned);
    let parameters = &method["parameters"];
    let m = parameters["m"].as_u64().and_then(|m| u32::try_from(m).ok());
    let mut params = vec![("space_type", similarity.clone())];
    params.push(("engine", engine.clone().unwrap_or_else(|| "default".to_owned())));
    if let Some(parameters) = parameters.as_object() {
        params.extend(parameters.iter().map(|(name, value)| (name.as_str(), value.to_string())));
    }
    for option in ["mode", "compression_level"] {
        if let Some(value) = definition[option].as_str() {
            params.push((option, value.to_owned()));
        }
    }
    Some(VectorField {
        name: name.to_owned(),
        dims,
        field_type: KNN_VECTOR,
        similarity: Some(similarity),
        metric,
        indexed: knn && metric.is_some(),
        kind: method["name"].as_str().unwrap_or("hnsw").to_owned(),
        engine,
        m,
        params: options_text(&params),
    })
}

/// Merges the fields of several indices (an alias or pattern): the first definition of a name, searchable by ANN only
/// when every index defines it the same way.
fn merge_fields(per_index: Vec<Vec<VectorField>>) -> Vec<VectorField> {
    let mut merged: Vec<VectorField> = Vec::new();
    for field in per_index.into_iter().flatten() {
        match merged.iter_mut().find(|known| known.name == field.name) {
            Some(known) => {
                if known.dims != field.dims || known.metric != field.metric || !field.indexed {
                    known.indexed = false;
                }
            }
            None => merged.push(field),
        }
    }
    merged
}

/// A value of `_source` at a dotted path (nested objects or a literal dotted key).
fn source_at<'j>(source: &'j Json, path: &str) -> Option<&'j Json> {
    if let Some(value) = source.get(path) {
        return Some(value);
    }
    path.match_indices('.').find_map(|(at, _)| {
        let inner = source.get(&path[..at])?;
        source_at(inner, &path[at + 1..])
    })
}

fn vector_of(value: &Json) -> Option<Vec<f32>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_f64().map(|number| number as f32))
        .collect()
}

/// The distance as pgvector defines it: `1 - cosine`, L2, or the negative inner product.
fn distance(metric: Metric, left: &[f32], right: &[f32]) -> f64 {
    if left.len() != right.len() {
        return f64::NAN;
    }
    let pairs = || left.iter().zip(right).map(|(a, b)| (f64::from(*a), f64::from(*b)));
    let dot: f64 = pairs().map(|(a, b)| a * b).sum();
    match metric {
        Metric::Euclidean => pairs().map(|(a, b)| (a - b) * (a - b)).sum::<f64>().sqrt(),
        Metric::Dot => -dot,
        Metric::Cosine | Metric::Auto => {
            let norms = pairs().map(|(a, _)| a * a).sum::<f64>().sqrt() * pairs().map(|(_, b)| b * b).sum::<f64>().sqrt();
            if norms == 0.0 {
                1.0
            } else {
                1.0 - dot / norms
            }
        }
    }
}

/// A Painless string literal.
fn painless_string(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The Painless score of an exact Elasticsearch search (higher is nearer; scores must not be negative).
fn painless_score(metric: Metric, field: &str) -> String {
    let field = painless_string(field);
    match metric {
        Metric::Euclidean => format!("1 / (1 + l2norm(params.query_vector, {field}))"),
        Metric::Dot => format!("double d = dotProduct(params.query_vector, {field}); return d < 0 ? 1 / (1 - d) : d + 1;"),
        Metric::Cosine | Metric::Auto => format!("cosineSimilarity(params.query_vector, {field}) + 1.0"),
    }
}

fn num_candidates(k: u32) -> u32 {
    k.saturating_mul(2).clamp(100, MAX_CANDIDATES).max(k)
}

/// What a search body asks for besides the query: hit count, `_source` and the filter.
struct BodyParts {
    k: u32,
    filter: Option<Json>,
    source: Json,
}

/// An ANN search body; `query` is the vector (or a placeholder).
fn knn_body(opensearch: bool, field: &VectorField, query: Json, parts: &BodyParts) -> Json {
    let k = parts.k;
    if !opensearch {
        let mut knn = json!({ "field": field.name, "query_vector": query, "k": k, "num_candidates": num_candidates(k) });
        if let Some(filter) = &parts.filter {
            knn["filter"] = filter.clone();
        }
        return json!({ "size": k, "knn": knn, "_source": parts.source, "track_total_hits": false });
    }
    let mut clause = json!({ "vector": query, "k": k });
    // lucene and faiss filter while searching the graph; other engines filter the k hits afterwards.
    let efficient = matches!(field.engine.as_deref(), Some("lucene" | "faiss"));
    let filter = parts.filter.clone();
    if efficient {
        if let Some(filter) = &filter {
            clause["filter"] = filter.clone();
        }
    }
    let knn = json!({ "knn": { field.name.clone(): clause } });
    let query = match filter {
        Some(filter) if !efficient => json!({ "bool": { "must": [knn], "filter": [filter] } }),
        _ => knn,
    };
    json!({ "size": k, "query": query, "_source": parts.source, "track_total_hits": false })
}

/// An exact search body: `script_score` over the documents with a vector, ordered by score then `tiebreak`.
fn exact_body(opensearch: bool, field: &VectorField, metric: Metric, query: Json, parts: &BodyParts, tiebreak: Vec<Json>) -> Json {
    let mut filters = vec![json!({ "exists": { "field": field.name } })];
    filters.extend(parts.filter.clone());
    let script = if opensearch {
        json!({
            "source": "knn_score",
            "lang": "knn",
            "params": { "field": field.name, "query_value": query, "space_type": space_type(metric) }
        })
    } else {
        json!({ "source": painless_score(metric, &field.name), "params": { "query_vector": query } })
    };
    json!({
        "size": parts.k,
        "query": { "script_score": { "query": { "bool": { "filter": filters } }, "script": script } },
        "sort": std::iter::once(json!({ "_score": { "order": "desc" } })).chain(tiebreak).collect::<Vec<_>>(),
        "_source": parts.source,
        "track_total_hits": false
    })
}

/// The sort for a key: the declared key field, `_index`, and `_doc` for `_id` (which Elasticsearch cannot sort).
fn key_sort(target: &Target, key: &[String]) -> Vec<Json> {
    let mut sort: Vec<Json> = key
        .iter()
        .map(|name| match name.as_str() {
            INDEX => json!({ INDEX: "asc" }),
            ID => json!("_doc"),
            name => match target.column(name).and_then(|column| column.exact.clone()) {
                Some(field) => json!({ field: "asc" }),
                None => json!("_doc"),
            },
        })
        .collect();
    sort.dedup();
    if sort.is_empty() {
        sort.push(json!("_doc"));
    }
    sort
}

fn sample_body(field: &str, limit: u32, order: Option<Vec<Json>>) -> Json {
    let exists = json!({ "exists": { "field": field } });
    let size = limit.min(MAX_RESULT_WINDOW as u32);
    match order {
        Some(sort) => json!({ "size": size, "query": exists, "_source": [field], "sort": sort, "track_total_hits": false }),
        None => json!({
            "size": size,
            "query": { "function_score": { "query": exists, "random_score": {}, "boost_mode": "replace" } },
            "_source": [field],
            "track_total_hits": false
        }),
    }
}

/// A console request (`POST /index/_search` and the body).
fn console_text(index: &str, body: &Json) -> String {
    format!(
        "POST /{}/_search\n{}",
        segment(index),
        serde_json::to_string_pretty(body).unwrap_or_default()
    )
}

/// A hit's key cell as text.
fn key_text(hit: &Json, column: &str) -> String {
    let value = match column {
        ID | INDEX => &hit[column],
        name => source_at(&hit["_source"], name).unwrap_or(&Json::Null),
    };
    match value {
        Json::String(text) => text.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

/// The mapping of a target with its vector fields, read once per operation.
struct Resolved {
    target: Target,
    fields: Vec<VectorField>,
}

pub struct EsVectors<'a> {
    engine: &'a EsEngine,
    opensearch: bool,
    /// The cluster runs ANN searches (Elasticsearch 8.4+ has the knn search section; OpenSearch with the plugin).
    ann: bool,
    resolved: Option<Resolved>,
}

impl<'a> EsVectors<'a> {
    pub fn new(engine: &'a EsEngine) -> Self {
        let info = engine.info();
        let opensearch = info.is_opensearch();
        Self {
            engine,
            opensearch,
            ann: opensearch || (info.major, info.minor) >= (8, 4),
            resolved: None,
        }
    }

    async fn resolve(&mut self, table: &TableRef) -> AppResult<&Resolved> {
        if self.resolved.as_ref().is_some_and(|resolved| resolved.target.name == table.table) {
            return Ok(self.resolved.as_ref().expect("resolved"));
        }
        let target = grid::resolve(self.engine, &table.table).await?;
        let name = segment(&target.name);
        let mappings = self.engine.client().get(&format!("/{name}/_mapping")).await?;
        let settings = if self.opensearch {
            self.engine.client().get(&format!("/{name}/_settings/index.knn")).await?
        } else {
            Json::Null
        };
        let per_index = mappings
            .as_object()
            .map(|indices| {
                indices
                    .iter()
                    .map(|(index, entry)| {
                        let knn =
                            settings[index]["settings"]["index"]["knn"] == "true" || settings[index]["settings"]["index"]["knn"] == true;
                        vector_fields(&entry["mappings"], knn)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut fields = merge_fields(per_index);
        if !self.ann {
            fields.iter_mut().for_each(|field| field.indexed = false);
        }
        Ok(self.resolved.insert(Resolved { target, fields }))
    }

    async fn field(&mut self, table: &TableRef, name: &str) -> AppResult<(Target, VectorField)> {
        let resolved = self.resolve(table).await?;
        let field = resolved
            .fields
            .iter()
            .find(|field| field.name == name)
            .cloned()
            .ok_or_else(|| AppError::new(format!("{name} is not a vector field")))?;
        Ok((resolved.target.clone(), field))
    }

    async fn search_hits(&self, index: &str, body: &Json) -> AppResult<(Json, Vec<Json>)> {
        let path = format!("/{}/_search", segment(index));
        let response = self
            .engine
            .client()
            .call(&EsRequest::new(Method::POST, path).json(body.clone()))
            .await?;
        let hits = response["hits"]["hits"].as_array().cloned().unwrap_or_default();
        Ok((response, hits))
    }

    /// `_source` for a search: everything except the other vector fields.
    fn source_without_other_vectors(fields: &[VectorField], keep: &str) -> Json {
        let excludes: Vec<&str> = fields
            .iter()
            .map(|field| field.name.as_str())
            .filter(|name| *name != keep)
            .collect();
        if excludes.is_empty() {
            json!(true)
        } else {
            json!({ "excludes": excludes })
        }
    }
}

/// Hits with their distance to `query`, nearest first (ties keep the engine's order).
fn by_distance(hits: Vec<Json>, field: &str, metric: Metric, query: &[f32]) -> Vec<(f64, Json)> {
    let mut ranked: Vec<(f64, Json)> = hits
        .into_iter()
        .map(|hit| {
            let distance = source_at(&hit["_source"], field)
                .and_then(vector_of)
                .map_or(f64::NAN, |vector| distance(metric, query, &vector));
            (distance, hit)
        })
        .collect();
    ranked.sort_by(|left, right| left.0.total_cmp(&right.0));
    ranked
}

#[async_trait]
impl VectorStore for EsVectors<'_> {
    fn profile(&self) -> &'static VectorProfile {
        if self.opensearch {
            &OPENSEARCH_PROFILE
        } else {
            &ELASTIC_PROFILE
        }
    }

    async fn supported(&mut self, _table: &TableRef) -> AppResult<bool> {
        // The connection advertises the capability only when the cluster has vector search.
        Ok(true)
    }

    async fn columns(&mut self, table: &TableRef) -> AppResult<Vec<VectorColumn>> {
        Ok(self.resolve(table).await?.fields.iter().map(VectorField::column).collect())
    }

    async fn indexes(&mut self, table: &TableRef) -> AppResult<Vec<VectorIndex>> {
        Ok(self.resolve(table).await?.fields.iter().filter_map(VectorField::index).collect())
    }

    async fn primary_key(&mut self, table: &TableRef) -> AppResult<Vec<String>> {
        Ok(self.resolve(table).await?.target.key_columns())
    }

    async fn fetch(&mut self, table: &TableRef, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
        let (target, field) = self.field(table, column).await?;
        let body = json!({ "size": 1, "query": key_query(&target, key)?, "_source": [field.name], "track_total_hits": false });
        let (_, hits) = self.search_hits(&target.name, &body).await?;
        let hit = hits.first().ok_or_else(|| AppError::new("Document not found"))?;
        source_at(&hit["_source"], &field.name)
            .and_then(vector_of)
            .ok_or_else(|| AppError::new("This document has no vector in this field"))
    }

    async fn count(&mut self, table: &TableRef, column: &VectorColumn) -> AppResult<(u64, u64)> {
        let index = self.resolve(table).await?.target.name.clone();
        let total = self.engine.count(&index, None).await?;
        let missing = json!({ "bool": { "must_not": [{ "exists": { "field": column.name } }] } });
        Ok((total, self.engine.count(&index, Some(&missing)).await?))
    }

    async fn sample(
        &mut self,
        table: &TableRef,
        column: &VectorColumn,
        limit: u32,
        order: SampleOrder<'_>,
    ) -> AppResult<Vec<Option<Vec<f32>>>> {
        let target = self.resolve(table).await?.target.clone();
        let sort = match order {
            SampleOrder::Any => Some(vec![json!("_doc")]),
            SampleOrder::Random => None,
            SampleOrder::ByKey(key) => Some(key_sort(&target, key)),
        };
        let (_, hits) = self.search_hits(&target.name, &sample_body(&column.name, limit, sort)).await?;
        Ok(hits
            .iter()
            .map(|hit| source_at(&hit["_source"], &column.name).and_then(vector_of))
            .collect())
    }

    async fn search(&mut self, table: &TableRef, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult> {
        let (target, field) = self.field(table, &request.column.name).await?;
        let fields = self.resolve(table).await?.fields.clone();
        let metric = request.metric.resolve(request.index, field.metric.unwrap_or(Metric::Cosine));
        let parts = BodyParts {
            k: request.k,
            filter: request.filter.map(dsl::parse_filter).transpose()?,
            source: Self::source_without_other_vectors(&fields, &field.name),
        };
        let ann = field.indexed && field.metric == Some(metric);
        let tiebreak = key_sort(&target, &target.key_columns());
        let render = |query: Json| {
            if ann {
                knn_body(self.opensearch, &field, query, &parts)
            } else {
                exact_body(self.opensearch, &field, metric, query, &parts, tiebreak.clone())
            }
        };
        let body = render(json!(request.query));
        let started = Instant::now();
        let (response, hits) = self.search_hits(&target.name, &body).await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let ranked = by_distance(hits, &field.name, metric, request.query);

        let infos = target.column_infos();
        let keep: Vec<usize> = infos
            .iter()
            .enumerate()
            .filter(|(_, info)| !fields.iter().any(|vector| vector.name == info.name))
            .map(|(position, _)| position)
            .collect();
        let mut columns = vec![ColumnInfo::result(DISTANCE, "double", ValueKind::Float)];
        columns.extend(keep.iter().map(|position| infos[*position].clone()));
        let rows = ranked
            .iter()
            .map(|(distance, hit)| {
                let mut cells = vec![Value::Text(hit[ID].as_str().unwrap_or_default().to_owned())];
                if !target.concrete {
                    cells.push(Value::Text(hit[INDEX].as_str().unwrap_or_default().to_owned()));
                }
                for column in &target.columns {
                    cells.push(values::to_value(&hit["_source"][&column.name], &column.kind));
                }
                let mut row = vec![Value::Float(*distance)];
                row.extend(keep.iter().map(|position| cells[*position].clone()));
                row
            })
            .collect();

        let mut plan = if ann {
            let engine = field.engine.as_deref().map(|engine| format!(", {engine}")).unwrap_or_default();
            let mut line = format!(
                "knn on {} ({}{engine}, {}): k = {}",
                field.name,
                field.kind,
                field.similarity.as_deref().unwrap_or(metric_name(metric)),
                request.k
            );
            if !self.opensearch {
                line.push_str(&format!(", num_candidates = {}", num_candidates(request.k)));
            }
            vec![line]
        } else {
            let why = if field.indexed { "" } else { " (the field has no search graph)" };
            vec![format!(
                "Exact: script_score over every document with {}, {} distance{why}",
                field.name,
                shared_label(metric)
            )]
        };
        if parts.filter.is_some() {
            plan.push("Filter: the Query DSL filter, applied by the search".to_owned());
        }
        plan.push(format!(
            "took {} ms on {} shards",
            response["took"].as_u64().unwrap_or_default(),
            response["_shards"]["total"].as_u64().unwrap_or_default()
        ));
        plan.push("Distances are computed from the returned vectors (1 - cosine, L2, negative inner product)".to_owned());

        Ok(VectorSearchResult {
            result: RowSet { columns, rows },
            plan: RowSet {
                columns: vec![ColumnInfo::result("PLAN", "text", ValueKind::Text)],
                rows: plan.into_iter().map(|line| vec![Value::Text(line)]).collect(),
            },
            index_used: ann.then(|| field.name.clone()),
            sql: console_text(&target.name, &render(json!(QUERY_PLACEHOLDER))),
            runnable_sql: console_text(&target.name, &body),
            elapsed_ms,
            query_dims: request.query.len(),
            primary_key: target.key_columns(),
        })
    }

    async fn nearest(&mut self, table: &TableRef, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>> {
        let (target, field) = self.field(table, &request.column.name).await?;
        let metric = Metric::of_index(request.index);
        let mut source = vec![field.name.clone()];
        source.extend(request.key.iter().filter(|name| !name.starts_with('_')).cloned());
        let parts = BodyParts {
            k: request.k,
            filter: None,
            source: json!(source),
        };
        let body = if request.exact {
            let tiebreak = key_sort(&target, request.key);
            exact_body(self.opensearch, &field, metric, json!(request.query), &parts, tiebreak)
        } else if field.indexed {
            knn_body(self.opensearch, &field, json!(request.query), &parts)
        } else {
            return Err(AppError::new(format!("{} has no search graph to measure", field.name)));
        };
        let (_, hits) = self.search_hits(&target.name, &body).await?;
        let mut found: Vec<Neighbour> = by_distance(hits, &field.name, metric, request.query)
            .into_iter()
            .map(|(distance, hit)| Neighbour {
                key: request
                    .key
                    .iter()
                    .map(|column| key_text(&hit, column))
                    .collect::<Vec<_>>()
                    .join(KEY_SEPARATOR),
                distance,
            })
            .collect();
        found.sort_by(|left, right| left.distance.total_cmp(&right.distance).then_with(|| left.key.cmp(&right.key)));
        Ok(found)
    }
}

fn shared_label(metric: Metric) -> &'static str {
    match metric {
        Metric::Auto | Metric::Cosine => "cosine",
        Metric::Euclidean => "euclidean",
        Metric::Dot => "inner product",
    }
}

/// The query for one document by its key cells (as the grid sends them).
fn key_query(target: &Target, key: &[KeyCell]) -> AppResult<Json> {
    if key.is_empty() {
        return Err(AppError::new("Cannot identify the document"));
    }
    let clauses = key
        .iter()
        .map(|cell| match cell.column.as_str() {
            ID => Ok(json!({ "ids": { "values": [target.id_of(&cell.value)?] } })),
            INDEX => Ok(json!({ "term": { INDEX: target.id_of(&cell.value)? } })),
            name => {
                let column = target
                    .column(name)
                    .ok_or_else(|| AppError::new(format!("Unknown key column {name}")))?;
                let field = column
                    .exact
                    .clone()
                    .ok_or_else(|| AppError::new(format!("{name} cannot identify a document")))?;
                Ok(json!({ "term": { field: values::to_json(&cell.value, &column.kind)? } }))
            }
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(json!({ "bool": { "filter": clauses } }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::elasticsearch::values::FieldColumn;

    fn target(key: Option<&str>) -> Target {
        let id = FieldColumn {
            name: "id".into(),
            field_type: "long".into(),
            kind: ValueKind::Int,
            exact: Some("id".into()),
            vector_dims: None,
        };
        Target {
            name: "items".into(),
            concrete: true,
            indices: vec!["items".into()],
            columns: vec![id.clone()],
            key: key.map(|_| id),
            routing_required: false,
        }
    }

    fn mapping() -> Json {
        json!({ "properties": {
            "name": { "type": "keyword" },
            "embedding": { "type": "dense_vector", "dims": 4, "index": true, "similarity": "cosine",
                           "index_options": { "type": "int8_hnsw", "m": 16, "ef_construction": 100 } },
            "raw": { "type": "dense_vector", "dims": 3, "index": false },
            "bits": { "type": "dense_vector", "dims": 8, "element_type": "bit" },
            "meta": { "properties": { "mip": { "type": "dense_vector", "dims": 2, "index": true, "similarity": "max_inner_product" } } },
            "os": { "type": "knn_vector", "dimension": 4,
                    "method": { "name": "hnsw", "engine": "lucene", "space_type": "cosinesimil", "parameters": { "m": 8 } } },
            "plain": { "type": "knn_vector", "dimension": 4 }
        } })
    }

    #[test]
    fn mappings_give_vector_fields_with_their_fixed_metric() {
        let fields = vector_fields(&mapping(), true);
        let names: Vec<&str> = fields.iter().map(|field| field.name.as_str()).collect();
        assert_eq!(names, ["embedding", "raw", "meta.mip", "os", "plain"]);
        let embedding = &fields[0];
        assert_eq!(embedding.column().metrics, [Metric::Cosine]);
        assert_eq!(embedding.column().type_name, "dense_vector(4)");
        let index = embedding.index().unwrap();
        assert_eq!(
            (index.metric.as_str(), index.kind.as_str(), index.m),
            ("cosine", "int8_hnsw", Some(16))
        );
        assert_eq!(index.params, "similarity=cosine, m=16, ef_construction=100");
        // Without a graph: any metric, exact only.
        assert!(fields[1].column().metrics.is_empty() && fields[1].index().is_none());
        assert_eq!(fields[2].index().unwrap().metric, "dot");
        assert_eq!(fields[3].engine.as_deref(), Some("lucene"));
        assert_eq!(fields[3].index().unwrap().params, "space_type=cosinesimil, engine=lucene, m=8");
        assert_eq!(fields[4].column().metrics, [Metric::Euclidean]);
        // OpenSearch without index.knn: no graph.
        assert!(vector_fields(&mapping(), false)[3].index().is_none());
        assert_eq!(metric_of("l2_norm"), Some(Metric::Euclidean));
        assert_eq!(metric_of("l1"), None);
        let other = vector_fields(
            &json!({ "properties": { "embedding": { "type": "dense_vector", "dims": 4, "index": true, "similarity": "l2_norm" } } }),
            false,
        );
        let merged = merge_fields(vec![fields.clone(), other]);
        assert!(!merged[0].indexed);
        assert!(has_knn_plugin(
            &json!([{ "component": "opensearch-sql" }, { "component": "opensearch-knn" }])
        ));
        assert!(!has_knn_plugin(&json!([{ "component": "opensearch-sql" }])));
        // The grid marks vector fields, so it offers Find similar on them.
        let grid = values::columns_from_mapping(&mapping());
        let hint = |name: &str| {
            grid.iter()
                .find(|column| column.name == name)
                .unwrap()
                .info(false)
                .hints
                .vector_dims
        };
        assert_eq!((hint("embedding"), hint("os"), hint("name")), (Some(4), Some(4), None));
    }

    #[test]
    fn search_bodies_follow_the_flavor() {
        let fields = vector_fields(&mapping(), true);
        let parts = BodyParts {
            k: 5,
            filter: Some(json!({ "term": { "name": "a" } })),
            source: json!(true),
        };
        let es = knn_body(false, &fields[0], json!([1.0, 0.0]), &parts);
        assert_eq!(
            es,
            json!({ "size": 5, "knn": { "field": "embedding", "query_vector": [1.0, 0.0], "k": 5, "num_candidates": 100,
                    "filter": { "term": { "name": "a" } } }, "_source": true, "track_total_hits": false })
        );
        assert_eq!(num_candidates(80), 160);
        assert_eq!(num_candidates(1000), 2000);
        assert_eq!(num_candidates(9000), 10_000);
        let os = knn_body(true, &fields[3], json!([1.0]), &parts);
        assert_eq!(os["query"]["knn"]["os"]["filter"]["term"]["name"], "a");
        assert_eq!(os["query"]["knn"]["os"]["k"], 5);
        assert_eq!(os["size"], 5);
        // An engine without efficient filtering filters the hits.
        let post = knn_body(true, &fields[4], json!([1.0]), &parts);
        assert_eq!(post["query"]["bool"]["filter"][0]["term"]["name"], "a");
        assert!(post["query"]["bool"]["must"][0]["knn"]["plain"]["filter"].is_null());

        let exact = exact_body(
            false,
            &fields[0],
            Metric::Euclidean,
            json!([1.0]),
            &parts,
            vec![json!({ "id": "asc" })],
        );
        let script = &exact["query"]["script_score"]["script"];
        assert_eq!(script["source"], "1 / (1 + l2norm(params.query_vector, 'embedding'))");
        assert_eq!(script["params"]["query_vector"], json!([1.0]));
        assert_eq!(
            exact["query"]["script_score"]["query"]["bool"]["filter"][0]["exists"]["field"],
            "embedding"
        );
        assert_eq!(exact["sort"], json!([{ "_score": { "order": "desc" } }, { "id": "asc" }]));
        let os_exact = exact_body(true, &fields[3], Metric::Dot, json!([1.0]), &parts, vec![json!("_doc")]);
        let script = &os_exact["query"]["script_score"]["script"];
        assert_eq!(
            (script["source"].as_str(), script["lang"].as_str()),
            (Some("knn_score"), Some("knn"))
        );
        assert_eq!(script["params"]["space_type"], "innerproduct");
        assert!(painless_score(Metric::Dot, "a'b").contains("'a\\'b'"));
        assert!(painless_score(Metric::Cosine, "e").starts_with("cosineSimilarity("));
    }

    #[test]
    fn keys_samples_and_distances() {
        assert_eq!(key_sort(&target(Some("id")), &["id".into()]), [json!({ "id": "asc" })]);
        assert_eq!(key_sort(&target(None), &["_id".into()]), [json!("_doc")]);
        assert_eq!(
            key_sort(&target(None), &["_index".into(), "_id".into()]),
            [json!({ "_index": "asc" }), json!("_doc")]
        );
        let sample = sample_body("embedding", 50_000, Some(vec![json!("_doc")]));
        assert_eq!(sample["size"], 10_000);
        assert_eq!(sample["sort"], json!(["_doc"]));
        assert!(sample_body("e", 5, None)["query"]["function_score"]["random_score"].is_object());

        let key = |column: &str, value: Value| KeyCell {
            column: column.into(),
            value,
        };
        assert_eq!(
            key_query(&target(Some("id")), &[key("id", Value::Int(7))]).unwrap(),
            json!({ "bool": { "filter": [{ "term": { "id": 7 } }] } })
        );
        assert_eq!(
            key_query(&target(None), &[key("_id", Value::Text("a1".into()))]).unwrap()["bool"]["filter"][0],
            json!({ "ids": { "values": ["a1"] } })
        );
        assert!(key_query(&target(None), &[]).is_err());
        assert!(key_query(&target(None), &[key("nope", Value::Int(1))]).is_err());

        let source = json!({ "meta": { "e": [1.0, 2.0] }, "a.b": [3.0] });
        assert_eq!(source_at(&source, "meta.e").and_then(vector_of), Some(vec![1.0, 2.0]));
        assert_eq!(source_at(&source, "a.b").and_then(vector_of), Some(vec![3.0]));
        assert!(source_at(&source, "meta.x").is_none());
        assert!(distance(Metric::Cosine, &[1.0, 0.0], &[2.0, 0.0]).abs() < 1e-12);
        assert!((distance(Metric::Cosine, &[1.0, 0.0], &[0.0, 1.0]) - 1.0).abs() < 1e-12);
        assert_eq!(distance(Metric::Euclidean, &[0.0, 0.0], &[3.0, 4.0]), 5.0);
        assert_eq!(distance(Metric::Dot, &[1.0, 2.0], &[3.0, 4.0]), -11.0);
        assert!(distance(Metric::Dot, &[1.0], &[1.0, 2.0]).is_nan());
        let hit = json!({ "_id": "9", "_index": "items", "_source": { "id": 9 } });
        assert_eq!((key_text(&hit, "_id"), key_text(&hit, "id")), ("9".to_owned(), "9".to_owned()));
        assert!(console_text("items", &json!({})).starts_with("POST /items/_search\n{"));
    }
}
