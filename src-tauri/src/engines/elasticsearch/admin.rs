//! Admin and insight APIs: `_analyze`, ingest `_simulate`, `_cat` tables, cluster stats, tasks, and index operations
//! (create, delete, open, close, refresh, force merge, reindex) as typed, classified operations.

use std::collections::BTreeSet;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    classify::RequestClass,
    client::EsRequest,
    connection::{segment, EsEngine},
    query_lang::{TypedColumn, TypedTable},
};
use crate::error::{AppError, AppResult};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AnalyzeRequest {
    /// Analyze with an index's analyzers / field mapping.
    pub index: Option<String>,
    pub text: Vec<String>,
    pub analyzer: Option<String>,
    /// Use the analyzer of this field (needs `index`).
    pub field: Option<String>,
    pub tokenizer: Option<Value>,
    pub filter: Option<Vec<Value>>,
    pub char_filter: Option<Vec<Value>>,
    /// Return per-step details (`explain: true`).
    pub explain: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeToken {
    pub token: String,
    pub start_offset: u64,
    pub end_offset: u64,
    pub token_type: String,
    pub position: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    pub tokens: Vec<AnalyzeToken>,
    /// The `detail` section when `explain` was set.
    pub detail: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PipelineRef {
    /// A stored pipeline id.
    Id(String),
    /// An inline pipeline definition (`{ "processors": [...] }`), to test before saving.
    Inline(Value),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulatedDoc {
    /// The document after the pipeline (`_source`), when it succeeded.
    pub source: Option<Value>,
    pub error: Option<String>,
    /// Per-processor results with `verbose`.
    pub processor_results: Option<Vec<Value>>,
}

/// The `_cat` APIs exposed as tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CatApi {
    Nodes,
    Shards,
    Allocation,
    ThreadPool,
    Indices,
    Health,
    Segments,
    Recovery,
    PendingTasks,
    Plugins,
    Templates,
    Fielddata,
    MasterNode,
}

impl CatApi {
    fn path(self) -> &'static str {
        match self {
            Self::Nodes => "/_cat/nodes",
            Self::Shards => "/_cat/shards",
            Self::Allocation => "/_cat/allocation",
            Self::ThreadPool => "/_cat/thread_pool",
            Self::Indices => "/_cat/indices",
            Self::Health => "/_cat/health",
            Self::Segments => "/_cat/segments",
            Self::Recovery => "/_cat/recovery",
            Self::PendingTasks => "/_cat/pending_tasks",
            Self::Plugins => "/_cat/plugins",
            Self::Templates => "/_cat/templates",
            Self::Fielddata => "/_cat/fielddata",
            Self::MasterNode => "/_cat/master",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterStats {
    pub cluster_name: String,
    pub status: String,
    pub nodes: u64,
    pub data_nodes: u64,
    pub indices: u64,
    pub shards: u64,
    pub docs: u64,
    pub store_bytes: u64,
    pub heap_used_bytes: u64,
    pub heap_max_bytes: u64,
    pub versions: Vec<String>,
    pub raw: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInfo {
    /// `node:id`, as the cancel API expects.
    pub id: String,
    pub action: String,
    pub description: Option<String>,
    pub running_time_ms: u64,
    pub cancellable: bool,
    pub parent: Option<String>,
    pub status: Option<Value>,
}

/// Index operations with their class.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "op")]
pub enum IndexOp {
    /// `PUT /<index>` with optional `settings`, `mappings` and `aliases`.
    Create {
        index: String,
        body: Option<Value>,
    },
    Delete {
        index: String,
    },
    Open {
        index: String,
    },
    Close {
        index: String,
    },
    Refresh {
        index: String,
    },
    #[serde(rename_all = "camelCase")]
    ForceMerge {
        index: String,
        max_num_segments: Option<u32>,
        only_expunge_deletes: bool,
    },
    /// Merge into `PUT /<index>/_mapping`.
    PutMapping {
        index: String,
        mapping: Value,
    },
    PutSettings {
        index: String,
        settings: Value,
    },
    /// `POST /_reindex` (returns a task when `background`).
    Reindex {
        body: Value,
        background: bool,
    },
}

impl IndexOp {
    pub fn class(&self) -> RequestClass {
        match self {
            Self::Create { .. } | Self::Open { .. } | Self::PutMapping { .. } | Self::PutSettings { .. } => RequestClass::Ddl,
            Self::Delete { .. } | Self::Close { .. } => RequestClass::Destructive,
            Self::Refresh { .. } | Self::ForceMerge { .. } => RequestClass::Admin,
            Self::Reindex { .. } => RequestClass::Write,
        }
    }

    fn request(&self) -> AppResult<EsRequest> {
        let target = |index: &str| {
            let trimmed = index.trim();
            if trimmed.is_empty() || trimmed == "*" || trimmed == "_all" {
                Err(AppError::new("Name the index explicitly; `*` and `_all` are not accepted here."))
            } else {
                Ok(segment(trimmed))
            }
        };
        Ok(match self {
            Self::Create { index, body } => {
                let request = EsRequest::new(Method::PUT, format!("/{}", target(index)?));
                match body {
                    Some(body) => request.json(body.clone()),
                    None => request,
                }
            }
            Self::Delete { index } => EsRequest::new(Method::DELETE, format!("/{}", target(index)?)),
            Self::Open { index } => EsRequest::new(Method::POST, format!("/{}/_open", target(index)?)),
            Self::Close { index } => EsRequest::new(Method::POST, format!("/{}/_close", target(index)?)),
            Self::Refresh { index } => EsRequest::new(Method::POST, format!("/{}/_refresh", target(index)?)),
            Self::ForceMerge {
                index,
                max_num_segments,
                only_expunge_deletes,
            } => {
                let mut path = format!("/{}/_forcemerge?only_expunge_deletes={only_expunge_deletes}", target(index)?);
                if let Some(segments) = max_num_segments {
                    path.push_str(&format!("&max_num_segments={segments}"));
                }
                EsRequest::new(Method::POST, path).timeout(std::time::Duration::from_secs(600))
            }
            Self::PutMapping { index, mapping } => {
                EsRequest::new(Method::PUT, format!("/{}/_mapping", target(index)?)).json(mapping.clone())
            }
            Self::PutSettings { index, settings } => {
                EsRequest::new(Method::PUT, format!("/{}/_settings", target(index)?)).json(settings.clone())
            }
            Self::Reindex { body, background } => EsRequest::new(Method::POST, format!("/_reindex?wait_for_completion={}", !background))
                .json(body.clone())
                .timeout(std::time::Duration::from_secs(600)),
        })
    }
}

/// Rows of a `_cat` JSON response as a table; columns in first-seen order, values kept as strings.
pub fn cat_table(rows: &Value) -> TypedTable {
    let mut order: Vec<String> = Vec::new();
    let mut seen = BTreeSet::new();
    let items = rows.as_array().cloned().unwrap_or_default();
    for row in &items {
        for key in row.as_object().into_iter().flat_map(|row| row.keys()) {
            if seen.insert(key.clone()) {
                order.push(key.clone());
            }
        }
    }
    TypedTable {
        columns: order
            .iter()
            .map(|name| TypedColumn {
                name: name.clone(),
                column_type: "keyword".into(),
            })
            .collect(),
        rows: items
            .iter()
            .map(|row| order.iter().map(|name| row.get(name).cloned().unwrap_or(Value::Null)).collect())
            .collect(),
        cursor: None,
        took_ms: None,
        warnings: Vec::new(),
    }
}

impl EsEngine {
    pub async fn analyze(&self, request: &AnalyzeRequest) -> AppResult<AnalyzeResult> {
        let mut body = json!({ "text": request.text });
        for (key, value) in [
            ("analyzer", request.analyzer.clone().map(Value::String)),
            ("field", request.field.clone().map(Value::String)),
            ("tokenizer", request.tokenizer.clone()),
            ("filter", request.filter.clone().map(Value::Array)),
            ("char_filter", request.char_filter.clone().map(Value::Array)),
        ] {
            if let Some(value) = value {
                body[key] = value;
            }
        }
        if request.explain {
            body["explain"] = json!(true);
        }
        let path = match request.index.as_deref().filter(|index| !index.is_empty()) {
            Some(index) => format!("/{}/_analyze", segment(index)),
            None => "/_analyze".into(),
        };
        let response = self.client.post(&path, body).await?;
        let tokens = response["tokens"]
            .as_array()
            .map(|tokens| {
                tokens
                    .iter()
                    .map(|token| AnalyzeToken {
                        token: token["token"].as_str().unwrap_or_default().to_string(),
                        start_offset: token["start_offset"].as_u64().unwrap_or(0),
                        end_offset: token["end_offset"].as_u64().unwrap_or(0),
                        token_type: token["type"].as_str().unwrap_or_default().to_string(),
                        position: token["position"].as_u64().unwrap_or(0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(AnalyzeResult {
            tokens,
            detail: response.get("detail").cloned(),
        })
    }

    /// Runs sample documents through a stored or inline ingest pipeline. `docs` are `_source` objects.
    pub async fn simulate_pipeline(&self, pipeline: &PipelineRef, docs: &[Value], verbose: bool) -> AppResult<Vec<SimulatedDoc>> {
        let docs: Vec<Value> = docs.iter().map(|source| json!({ "_source": source })).collect();
        let (path, mut body) = match pipeline {
            PipelineRef::Id(id) => (format!("/_ingest/pipeline/{}/_simulate", segment(id)), json!({})),
            PipelineRef::Inline(definition) => ("/_ingest/pipeline/_simulate".to_string(), json!({ "pipeline": definition })),
        };
        body["docs"] = Value::Array(docs);
        let response = self.client.post(&format!("{path}?verbose={verbose}"), body).await?;
        Ok(response["docs"]
            .as_array()
            .map(|docs| {
                docs.iter()
                    .map(|doc| {
                        let results = doc["processor_results"].as_array().cloned();
                        let source = match &results {
                            Some(results) => results
                                .last()
                                .and_then(|last| last["doc"]["_source"].as_object())
                                .map(|s| Value::Object(s.clone())),
                            None => doc["doc"]["_source"].as_object().map(|s| Value::Object(s.clone())),
                        };
                        let error = doc["error"]["reason"].as_str().map(str::to_string).or_else(|| {
                            results
                                .as_ref()
                                .and_then(|results| results.iter().find_map(|r| r["error"]["reason"].as_str().map(str::to_string)))
                        });
                        SimulatedDoc {
                            source,
                            error,
                            processor_results: results,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// A `_cat` API as a table (`format=json`, byte sizes in bytes).
    pub async fn cat(&self, api: CatApi) -> AppResult<TypedTable> {
        let rows = self.client.get(&format!("{}?format=json&bytes=b", api.path())).await?;
        Ok(cat_table(&rows))
    }

    pub async fn cluster_stats(&self) -> AppResult<ClusterStats> {
        let raw = self.client.get("/_cluster/stats").await?;
        let number = |value: &Value| value.as_u64().unwrap_or(0);
        Ok(ClusterStats {
            cluster_name: raw["cluster_name"].as_str().unwrap_or_default().to_string(),
            status: raw["status"].as_str().unwrap_or_default().to_string(),
            nodes: number(&raw["nodes"]["count"]["total"]),
            data_nodes: number(&raw["nodes"]["count"]["data"]),
            indices: number(&raw["indices"]["count"]),
            shards: number(&raw["indices"]["shards"]["total"]),
            docs: number(&raw["indices"]["docs"]["count"]),
            store_bytes: number(&raw["indices"]["store"]["size_in_bytes"]),
            heap_used_bytes: number(&raw["nodes"]["jvm"]["mem"]["heap_used_in_bytes"]),
            heap_max_bytes: number(&raw["nodes"]["jvm"]["mem"]["heap_max_in_bytes"]),
            versions: raw["nodes"]["versions"]
                .as_array()
                .map(|versions| versions.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            raw,
        })
    }

    /// Running tasks, optionally filtered by action pattern (`*search*`, `*byquery*`, `*reindex*`).
    pub async fn list_tasks(&self, actions: Option<&str>) -> AppResult<Vec<TaskInfo>> {
        let mut path = "/_tasks?detailed=true&group_by=none".to_string();
        if let Some(actions) = actions.filter(|actions| !actions.is_empty()) {
            path.push_str(&format!("&actions={}", segment(actions)));
        }
        let body = self.client.get(&path).await?;
        let mut tasks: Vec<TaskInfo> = body["tasks"]
            .as_array()
            .map(|tasks| {
                tasks
                    .iter()
                    .map(|task| TaskInfo {
                        id: format!("{}:{}", task["node"].as_str().unwrap_or_default(), task["id"].as_u64().unwrap_or(0)),
                        action: task["action"].as_str().unwrap_or_default().to_string(),
                        description: task["description"].as_str().filter(|d| !d.is_empty()).map(str::to_string),
                        running_time_ms: task["running_time_in_nanos"].as_u64().unwrap_or(0) / 1_000_000,
                        cancellable: task["cancellable"].as_bool().unwrap_or(false),
                        parent: task["parent_task_id"].as_str().map(str::to_string),
                        status: task.get("status").cloned(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.running_time_ms));
        Ok(tasks)
    }

    /// The state of one task (`GET /_tasks/<id>`): `completed`, and `response` / `error` when done.
    pub async fn get_task(&self, id: &str) -> AppResult<Value> {
        self.client.get(&format!("/_tasks/{}", segment(id))).await
    }

    /// Cancels a task (class `Admin`).
    pub async fn cancel_task(&self, id: &str) -> AppResult<()> {
        self.client
            .call(&EsRequest::new(Method::POST, format!("/_tasks/{}/_cancel", segment(id))))
            .await
            .map(|_| ())
    }

    /// Runs an index operation and returns the response body (for reindex in the background, it holds `task`).
    pub async fn apply_index_op(&self, op: &IndexOp) -> AppResult<Value> {
        self.client.call(&op.request()?).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_index_ops_with_classes() {
        let close = IndexOp::Close { index: "logs".into() };
        assert_eq!(
            (close.class(), close.request().unwrap().path),
            (RequestClass::Destructive, "/logs/_close".to_string())
        );
        assert!(IndexOp::Delete { index: "*".into() }.request().is_err());
        assert!(IndexOp::Delete { index: "_all".into() }.request().is_err());
        let merge = IndexOp::ForceMerge {
            index: "logs".into(),
            max_num_segments: Some(1),
            only_expunge_deletes: false,
        };
        assert_eq!(
            merge.request().unwrap().path,
            "/logs/_forcemerge?only_expunge_deletes=false&max_num_segments=1"
        );
        assert_eq!(merge.class(), RequestClass::Admin);
        assert_eq!(
            IndexOp::Reindex {
                body: json!({}),
                background: true
            }
            .class(),
            RequestClass::Write
        );
        assert_eq!(
            IndexOp::Create {
                index: "x".into(),
                body: None
            }
            .class(),
            RequestClass::Ddl
        );
    }

    #[test]
    fn builds_cat_tables() {
        let table = cat_table(&json!([{ "index": "a", "docs.count": "3" }, { "index": "b", "health": "green" }]));
        let names: Vec<_> = table.columns.iter().map(|column| column.name.as_str()).collect();
        assert_eq!(names, vec!["index", "docs.count", "health"]);
        assert_eq!(table.rows[1], vec![json!("b"), Value::Null, json!("green")]);
    }
}
