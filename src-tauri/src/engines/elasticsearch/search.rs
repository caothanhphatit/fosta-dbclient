//! Document search with Query DSL: from/size for shallow pages, `search_after` + point in time for deep paging,
//! sort, `_source` filtering, highlight, aggregations parsed into a tree, profile, count and query validation.

use std::collections::BTreeMap;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::{
    client::EsRequest,
    connection::{segment, EsEngine},
    mapping::FieldCategory,
};
use crate::error::{AppError, AppResult};

/// `from + size` above this needs search_after (Elasticsearch's default `index.max_result_window`).
pub const MAX_RESULT_WINDOW: u64 = 10_000;
const DEFAULT_SIZE: u32 = 50;
const DEFAULT_KEEP_ALIVE: &str = "2m";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchRequest {
    /// Index, alias, data stream or pattern (`logs-*`). Empty means all.
    pub index: String,
    /// A full Query DSL body as typed by the user (`query`, `aggs`, `sort`, `runtime_mappings`...). The fields below
    /// override the matching keys.
    pub body: Option<Value>,
    /// Shortcut for `body.query`.
    pub query: Option<Value>,
    pub size: Option<u32>,
    pub sort: Option<Value>,
    /// `_source` filtering: `false`, a list of patterns, or `{ "includes": [...], "excludes": [...] }`.
    pub source: Option<Value>,
    pub highlight: Option<Value>,
    pub aggs: Option<Value>,
    /// `track_total_hits`: true for exact counts (default: Elasticsearch counts up to 10,000).
    pub track_total_hits: Option<Value>,
    pub profile: bool,
    pub paging: Paging,
    /// Also return field categories from `_field_caps` (ask on the first page only).
    pub with_field_types: bool,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum Paging {
    /// `from` / `size`; limited to the first 10,000 hits.
    Offset { from: u64 },
    /// `search_after` the given sort values without a point in time (results may shift if the index changes).
    After { search_after: Option<Vec<Value>> },
    /// Point in time + `search_after`: consistent deep paging. Start with `cursor: None`; pass back the returned
    /// cursor for the next page and call [`EsEngine::close_cursor`] when done.
    #[serde(rename_all = "camelCase")]
    PointInTime {
        cursor: Option<SearchCursor>,
        keep_alive: Option<String>,
    },
}

impl Default for Paging {
    fn default() -> Self {
        Self::Offset { from: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchCursor {
    pub pit_id: Option<String>,
    pub search_after: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    pub took_ms: u64,
    pub timed_out: bool,
    pub total: Option<TotalHits>,
    pub max_score: Option<f64>,
    pub hits: Vec<Hit>,
    /// The next page, when there may be one (for `After` and `PointInTime` paging; for `Offset` use `from + size`).
    pub next: Option<SearchCursor>,
    /// The point in time in use, so the caller can close it even after the last page.
    pub pit_id: Option<String>,
    pub aggregations: Vec<AggNode>,
    pub profile: Option<Value>,
    pub shards: ShardSummary,
    pub field_types: Option<BTreeMap<String, FieldCategory>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TotalHits {
    pub value: u64,
    /// `eq` or `gte` (a lower bound).
    pub relation: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardSummary {
    pub total: u64,
    pub successful: u64,
    pub skipped: u64,
    pub failed: u64,
    /// Reasons of failed shards (partial results).
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub index: String,
    pub id: String,
    pub score: Option<f64>,
    pub source: Option<Value>,
    pub fields: Option<Value>,
    pub highlight: Option<BTreeMap<String, Vec<String>>>,
    pub sort: Option<Vec<Value>>,
    pub version: Option<i64>,
    pub seq_no: Option<i64>,
    pub primary_term: Option<i64>,
    pub routing: Option<String>,
    pub inner_hits: Option<Value>,
}

/// One aggregation result. Requests are sent with `typed_keys`, so the type is known for every node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggNode {
    pub name: String,
    /// Aggregation type as Elasticsearch reports it (`sterms`, `date_histogram`, `avg`, `percentiles`...).
    pub agg_type: String,
    pub result: AggResult,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AggResult {
    /// terms, histograms, ranges, filters, composite...
    #[serde(rename_all = "camelCase")]
    Buckets {
        buckets: Vec<Bucket>,
        after_key: Option<Value>,
        sum_other_doc_count: Option<u64>,
    },
    /// filter, nested, global, missing, sampler...: one bucket with sub-aggregations.
    #[serde(rename_all = "camelCase")]
    SingleBucket { doc_count: u64, children: Vec<AggNode> },
    /// avg, sum, min, max, cardinality, value_count...
    #[serde(rename_all = "camelCase")]
    Value {
        value: Option<f64>,
        value_as_string: Option<String>,
    },
    /// stats, extended_stats, percentiles, geo_bounds, top_hits... as name → value.
    Values { values: Map<String, Value> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub key: Value,
    pub key_as_string: Option<String>,
    pub doc_count: u64,
    /// Range buckets.
    pub from: Option<f64>,
    pub to: Option<f64>,
    pub children: Vec<AggNode>,
}

/// Parses `aggregations` of a response sent with `typed_keys=true` (`sterms#by_brand`).
pub fn parse_aggregations(aggregations: &Value) -> Vec<AggNode> {
    let Some(entries) = aggregations.as_object() else {
        return Vec::new();
    };
    let mut nodes: Vec<AggNode> = entries
        .iter()
        .map(|(key, body)| {
            let (agg_type, name) = key.split_once('#').unwrap_or(("", key));
            AggNode {
                name: name.to_string(),
                agg_type: agg_type.to_string(),
                result: parse_agg_body(body),
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    nodes
}

/// Sub-aggregations are the typed keys (`type#name`) inside a bucket or single-bucket aggregation.
fn sub_aggregations(body: &Value) -> Vec<AggNode> {
    let typed: Map<String, Value> = body
        .as_object()
        .map(|map| {
            map.iter()
                .filter(|(key, _)| key.contains('#'))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    parse_aggregations(&Value::Object(typed))
}

fn parse_bucket(key: Value, bucket: &Value) -> Bucket {
    Bucket {
        key,
        key_as_string: bucket["key_as_string"].as_str().map(str::to_string),
        doc_count: bucket["doc_count"].as_u64().unwrap_or(0),
        from: bucket["from"].as_f64(),
        to: bucket["to"].as_f64(),
        children: sub_aggregations(bucket),
    }
}

fn parse_agg_body(body: &Value) -> AggResult {
    match &body["buckets"] {
        Value::Array(buckets) => {
            return AggResult::Buckets {
                buckets: buckets.iter().map(|bucket| parse_bucket(bucket["key"].clone(), bucket)).collect(),
                after_key: body.get("after_key").cloned(),
                sum_other_doc_count: body["sum_other_doc_count"].as_u64(),
            }
        }
        // Keyed buckets (`filters`, `keyed: true` ranges).
        Value::Object(buckets) => {
            return AggResult::Buckets {
                buckets: buckets
                    .iter()
                    .map(|(key, bucket)| parse_bucket(Value::String(key.clone()), bucket))
                    .collect(),
                after_key: None,
                sum_other_doc_count: None,
            }
        }
        _ => {}
    }
    if body.get("doc_count").is_some() && body.get("value").is_none() {
        return AggResult::SingleBucket {
            doc_count: body["doc_count"].as_u64().unwrap_or(0),
            children: sub_aggregations(body),
        };
    }
    if let Some(value) = body.get("value") {
        if value.is_number() || value.is_null() {
            return AggResult::Value {
                value: value.as_f64(),
                value_as_string: body["value_as_string"].as_str().map(str::to_string),
            };
        }
    }
    AggResult::Values {
        values: body.as_object().cloned().unwrap_or_default(),
    }
}

fn parse_hit(hit: &Value) -> Hit {
    Hit {
        index: hit["_index"].as_str().unwrap_or_default().to_string(),
        id: hit["_id"].as_str().unwrap_or_default().to_string(),
        score: hit["_score"].as_f64(),
        source: hit.get("_source").cloned(),
        fields: hit.get("fields").cloned(),
        highlight: hit["highlight"].as_object().map(|fields| {
            fields
                .iter()
                .map(|(field, fragments)| {
                    (
                        field.clone(),
                        fragments
                            .as_array()
                            .map(|items| items.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                    )
                })
                .collect()
        }),
        sort: hit["sort"].as_array().cloned(),
        version: hit["_version"].as_i64(),
        seq_no: hit["_seq_no"].as_i64(),
        primary_term: hit["_primary_term"].as_i64(),
        routing: hit["_routing"].as_str().map(str::to_string),
        inner_hits: hit.get("inner_hits").cloned(),
    }
}

/// Parses a `_search` response body.
pub fn parse_search_response(body: &Value) -> SearchPage {
    let total = match &body["hits"]["total"] {
        Value::Object(total) => Some(TotalHits {
            value: total.get("value").and_then(Value::as_u64).unwrap_or(0),
            relation: total.get("relation").and_then(Value::as_str).unwrap_or("eq").to_string(),
        }),
        Value::Number(number) => Some(TotalHits {
            value: number.as_u64().unwrap_or(0),
            relation: "eq".into(),
        }),
        _ => None,
    };
    let shards = &body["_shards"];
    SearchPage {
        took_ms: body["took"].as_u64().unwrap_or(0),
        timed_out: body["timed_out"].as_bool().unwrap_or(false),
        total,
        max_score: body["hits"]["max_score"].as_f64(),
        hits: body["hits"]["hits"]
            .as_array()
            .map(|hits| hits.iter().map(parse_hit).collect())
            .unwrap_or_default(),
        next: None,
        pit_id: body["pit_id"].as_str().map(str::to_string),
        aggregations: parse_aggregations(&body["aggregations"]),
        profile: body.get("profile").cloned(),
        shards: ShardSummary {
            total: shards["total"].as_u64().unwrap_or(0),
            successful: shards["successful"].as_u64().unwrap_or(0),
            skipped: shards["skipped"].as_u64().unwrap_or(0),
            failed: shards["failed"].as_u64().unwrap_or(0),
            failures: shards["failures"]
                .as_array()
                .map(|failures| {
                    failures
                        .iter()
                        .map(|failure| failure["reason"]["reason"].as_str().unwrap_or("shard failure").to_string())
                        .collect()
                })
                .unwrap_or_default(),
        },
        field_types: None,
    }
}

impl SearchRequest {
    /// The request body, with the shortcut fields applied over `body`.
    pub fn build_body(&self) -> AppResult<Map<String, Value>> {
        let mut body = match &self.body {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(map)) => map.clone(),
            Some(_) => return Err(AppError::new("The search body must be a JSON object.")),
        };
        let mut set = |key: &str, value: &Option<Value>| {
            if let Some(value) = value {
                body.insert(key.to_string(), value.clone());
            }
        };
        set("query", &self.query);
        set("sort", &self.sort);
        set("_source", &self.source);
        set("highlight", &self.highlight);
        set("aggs", &self.aggs);
        set("track_total_hits", &self.track_total_hits);
        if self.profile {
            body.insert("profile".into(), Value::Bool(true));
        }
        if let Some(size) = self.size {
            body.insert("size".into(), json!(size));
        } else if !body.contains_key("size") {
            body.insert("size".into(), json!(DEFAULT_SIZE));
        }
        Ok(body)
    }
}

impl EsEngine {
    pub async fn search(&self, request: &SearchRequest) -> AppResult<SearchPage> {
        let mut body = self.build_paged_body(request).await?;
        let size = body.get("size").and_then(Value::as_u64).unwrap_or(DEFAULT_SIZE as u64);
        // With a point in time the index goes in the PIT, not in the path.
        let pit = body.contains_key("pit");
        let path = if pit || request.index.is_empty() {
            "/_search?typed_keys=true".to_string()
        } else {
            format!("/{}/_search?typed_keys=true", segment(&request.index))
        };
        if pit && !body.contains_key("sort") && self.info.is_opensearch() {
            // OpenSearch needs an explicit tiebreaker for search_after with a PIT.
            body.insert("sort".into(), json!([{ "_score": "desc" }, { "_shard_doc": "asc" }]));
        }
        let mut search = EsRequest::new(Method::POST, path).json(Value::Object(body.clone()));
        if let Some(timeout) = request.timeout_ms {
            search = search.timeout(std::time::Duration::from_millis(timeout));
        }
        let response = self.client.call(&search).await?;
        let mut page = parse_search_response(&response);
        let pit_id = page
            .pit_id
            .clone()
            .or_else(|| body.get("pit").and_then(|pit| pit["id"].as_str()).map(str::to_string));
        page.pit_id = pit_id.clone();
        let full_page = page.hits.len() as u64 == size && size > 0;
        page.next = match &request.paging {
            Paging::Offset { .. } => None,
            Paging::After { .. } | Paging::PointInTime { .. } if full_page => {
                page.hits.last().and_then(|hit| hit.sort.clone()).map(|search_after| SearchCursor {
                    pit_id: if pit { pit_id } else { None },
                    search_after,
                })
            }
            _ => None,
        };
        if request.with_field_types {
            page.field_types = self
                .field_types(if request.index.is_empty() { "*" } else { &request.index })
                .await
                .ok();
        }
        Ok(page)
    }

    async fn build_paged_body(&self, request: &SearchRequest) -> AppResult<Map<String, Value>> {
        let mut body = request.build_body()?;
        let size = body.get("size").and_then(Value::as_u64).unwrap_or(DEFAULT_SIZE as u64);
        match &request.paging {
            Paging::Offset { from } => {
                if from + size > MAX_RESULT_WINDOW {
                    return Err(AppError::new(format!(
                        "Pages beyond the first {MAX_RESULT_WINDOW} hits need search_after; switch to point-in-time paging."
                    )));
                }
                body.insert("from".into(), json!(from));
            }
            Paging::After { search_after } => {
                if !body.contains_key("sort") {
                    return Err(AppError::new(
                        "search_after paging needs a sort (for example on @timestamp and a unique field).",
                    ));
                }
                if let Some(after) = search_after {
                    body.insert("search_after".into(), Value::Array(after.clone()));
                }
            }
            Paging::PointInTime { cursor, keep_alive } => {
                let keep_alive = keep_alive.clone().unwrap_or_else(|| DEFAULT_KEEP_ALIVE.to_string());
                let pit_id = match cursor.as_ref().and_then(|cursor| cursor.pit_id.clone()) {
                    Some(id) => id,
                    None => self.open_point_in_time(&request.index, &keep_alive).await?,
                };
                body.insert("pit".into(), json!({ "id": pit_id, "keep_alive": keep_alive }));
                if let Some(cursor) = cursor {
                    body.insert("search_after".into(), Value::Array(cursor.search_after.clone()));
                }
                body.remove("from");
            }
        }
        Ok(body)
    }

    /// Opens a point in time on `index` (Elasticsearch `_pit`, OpenSearch `_search/point_in_time`).
    pub async fn open_point_in_time(&self, index: &str, keep_alive: &str) -> AppResult<String> {
        if !self.features().point_in_time {
            return Err(AppError::new(
                "This cluster version has no point-in-time API; use offset or search_after paging.",
            ));
        }
        let index = segment(if index.is_empty() { "*" } else { index });
        let keep_alive = segment(keep_alive);
        let (path, field) = if self.info.is_opensearch() {
            (format!("/{index}/_search/point_in_time?keep_alive={keep_alive}"), "pit_id")
        } else {
            (format!("/{index}/_pit?keep_alive={keep_alive}"), "id")
        };
        let body = self.client.call(&EsRequest::new(Method::POST, path)).await?;
        body[field]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| AppError::new("The cluster did not return a point-in-time id."))
    }

    pub async fn close_point_in_time(&self, pit_id: &str) -> AppResult<()> {
        let request = if self.info.is_opensearch() {
            EsRequest::new(Method::DELETE, "/_search/point_in_time").json(json!({ "pit_id": [pit_id] }))
        } else {
            EsRequest::new(Method::DELETE, "/_pit").json(json!({ "id": pit_id }))
        };
        self.client.call(&request).await.map(|_| ())
    }

    /// Releases the point in time of a cursor (no-op without one).
    pub async fn close_cursor(&self, cursor: &SearchCursor) -> AppResult<()> {
        match &cursor.pit_id {
            Some(id) => self.close_point_in_time(id).await,
            None => Ok(()),
        }
    }

    /// `_count` with an optional query.
    pub async fn count(&self, index: &str, query: Option<&Value>) -> AppResult<u64> {
        let path = if index.is_empty() {
            "/_count".to_string()
        } else {
            format!("/{}/_count", segment(index))
        };
        let body = match query {
            Some(query) => json!({ "query": query }),
            None => json!({}),
        };
        let response = self.client.post(&path, body).await?;
        Ok(response["count"].as_u64().unwrap_or(0))
    }

    /// `_validate/query?explain=true`: whether a query is valid and how it is rewritten per index.
    pub async fn validate_query(&self, index: &str, query: &Value) -> AppResult<QueryValidation> {
        let path = if index.is_empty() {
            "/_validate/query?explain=true".to_string()
        } else {
            format!("/{}/_validate/query?explain=true", segment(index))
        };
        let response = self.client.post(&path, json!({ "query": query })).await?;
        Ok(QueryValidation {
            valid: response["valid"].as_bool().unwrap_or(false),
            error: response["error"].as_str().map(str::to_string),
            explanations: response["explanations"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|item| QueryExplanation {
                            index: item["index"].as_str().unwrap_or_default().to_string(),
                            valid: item["valid"].as_bool().unwrap_or(false),
                            explanation: item["explanation"].as_str().map(str::to_string),
                            error: item["error"].as_str().map(str::to_string),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryValidation {
    pub valid: bool,
    pub error: Option<String>,
    pub explanations: Vec<QueryExplanation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryExplanation {
    pub index: String,
    pub valid: bool,
    pub explanation: Option<String>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_aggregations() {
        let aggregations = json!({
            "sterms#by_brand": {
                "doc_count_error_upper_bound": 0, "sum_other_doc_count": 3,
                "buckets": [
                    { "key": "acme", "doc_count": 5, "avg#avg_price": { "value": 12.5 } },
                    { "key": "globex", "doc_count": 2, "avg#avg_price": { "value": null } }
                ]
            },
            "date_histogram#per_day": { "buckets": [{ "key": 1700000000000u64, "key_as_string": "2023-11-14", "doc_count": 4 }] },
            "filter#cheap": { "doc_count": 7, "max#top": { "value": 9.0, "value_as_string": "9.0" } },
            "stats#price_stats": { "count": 3, "min": 1.0, "max": 3.0, "avg": 2.0, "sum": 6.0 },
            "percentiles#p": { "values": { "50.0": 2.0 } },
            "filters#kinds": { "buckets": { "errors": { "doc_count": 1 }, "warnings": { "doc_count": 2 } } }
        });
        let nodes = parse_aggregations(&aggregations);
        let find = |name: &str| nodes.iter().find(|node| node.name == name).unwrap();
        let AggResult::Buckets {
            buckets,
            sum_other_doc_count,
            ..
        } = &find("by_brand").result
        else {
            panic!()
        };
        assert_eq!(find("by_brand").agg_type, "sterms");
        assert_eq!((buckets.len(), *sum_other_doc_count), (2, Some(3)));
        assert_eq!(
            buckets[0].children[0].result,
            AggResult::Value {
                value: Some(12.5),
                value_as_string: None
            }
        );
        let AggResult::Buckets { buckets, .. } = &find("per_day").result else {
            panic!()
        };
        assert_eq!(buckets[0].key_as_string.as_deref(), Some("2023-11-14"));
        let AggResult::SingleBucket { doc_count, children } = &find("cheap").result else {
            panic!()
        };
        assert_eq!((*doc_count, children[0].name.as_str()), (7, "top"));
        assert!(matches!(&find("price_stats").result, AggResult::Values { values } if values["avg"] == 2.0));
        assert!(matches!(&find("p").result, AggResult::Values { .. }));
        let AggResult::Buckets { buckets, .. } = &find("kinds").result else {
            panic!()
        };
        assert_eq!(buckets[1].key, "warnings");
    }

    #[test]
    fn parses_search_responses() {
        let body = json!({
            "took": 3, "timed_out": false, "pit_id": "abc",
            "_shards": { "total": 1, "successful": 1, "skipped": 0, "failed": 0 },
            "hits": { "total": { "value": 10000, "relation": "gte" }, "max_score": null, "hits": [
                { "_index": "p", "_id": "1", "_score": null, "_source": { "a": 1 }, "sort": [1, 2], "highlight": { "name": ["<em>x</em>"] } }
            ]}
        });
        let page = parse_search_response(&body);
        assert_eq!(page.total.as_ref().unwrap().relation, "gte");
        assert_eq!(page.hits[0].sort.as_ref().unwrap().len(), 2);
        assert_eq!(page.hits[0].highlight.as_ref().unwrap()["name"][0], "<em>x</em>");
        assert_eq!(page.pit_id.as_deref(), Some("abc"));
    }

    #[test]
    fn builds_bodies() {
        let request = SearchRequest {
            body: Some(json!({ "query": { "match_all": {} }, "size": 5 })),
            query: Some(json!({ "term": { "a": 1 } })),
            source: Some(json!(["a"])),
            ..Default::default()
        };
        let body = request.build_body().unwrap();
        assert_eq!(body["query"], json!({ "term": { "a": 1 } }));
        assert_eq!(body["size"], 5);
        assert_eq!(body["_source"], json!(["a"]));
        assert!(SearchRequest {
            body: Some(json!([1])),
            ..Default::default()
        }
        .build_body()
        .is_err());
        assert_eq!(SearchRequest::default().build_body().unwrap()["size"], DEFAULT_SIZE);
    }
}
