//! Typed document operations: get, index, update and delete by id, and update / delete by query. Each has its
//! classification so the gateway can confirm before running it.

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    classify::RequestClass,
    client::EsRequest,
    connection::{segment, EsEngine},
};
use crate::error::{AppError, AppResult};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub index: String,
    pub id: String,
    pub found: bool,
    pub version: Option<i64>,
    pub seq_no: Option<i64>,
    pub primary_term: Option<i64>,
    pub routing: Option<String>,
    pub source: Option<Value>,
}

/// Optimistic concurrency and routing for writes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WriteOptions {
    pub routing: Option<String>,
    /// Fail with a conflict unless the document still has this `_seq_no` / `_primary_term` (from the read).
    pub if_seq_no: Option<i64>,
    pub if_primary_term: Option<i64>,
    /// `true`, `false` or `wait_for` (make the change visible to search before returning).
    pub refresh: Option<String>,
    pub pipeline: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "op")]
pub enum DocumentOp {
    /// Create or replace (`PUT /<index>/_doc/<id>`, or `POST /<index>/_doc` with a generated id).
    #[serde(rename_all = "camelCase")]
    Index {
        index: String,
        id: Option<String>,
        source: Value,
        create_only: bool,
        options: WriteOptions,
    },
    /// Partial update (`POST /<index>/_update/<id>` with `doc`), optionally creating the document (`doc_as_upsert`).
    #[serde(rename_all = "camelCase")]
    Update {
        index: String,
        id: String,
        doc: Value,
        upsert: bool,
        options: WriteOptions,
    },
    /// Scripted update.
    #[serde(rename_all = "camelCase")]
    UpdateScript {
        index: String,
        id: String,
        script: Value,
        options: WriteOptions,
    },
    Delete {
        index: String,
        id: String,
        options: WriteOptions,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResult {
    pub index: String,
    pub id: String,
    /// created, updated, deleted, noop, not_found.
    pub result: String,
    pub version: Option<i64>,
    pub seq_no: Option<i64>,
    pub primary_term: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ByQueryKind {
    Update,
    Delete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByQueryOp {
    pub kind: ByQueryKind,
    pub index: String,
    pub query: Value,
    /// Painless script for updates (`{ "source": "ctx._source.n += 1" }`); none just reindexes in place.
    pub script: Option<Value>,
    /// Continue past version conflicts instead of aborting.
    pub proceed_on_conflicts: bool,
    /// Return a task id at once instead of waiting (use the tasks API to follow it).
    pub background: bool,
    pub refresh: bool,
    /// Maximum documents to process.
    pub max_docs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ByQueryResult {
    /// Set when run in the background.
    pub task: Option<String>,
    pub total: u64,
    pub updated: u64,
    pub deleted: u64,
    pub version_conflicts: u64,
    pub failures: Vec<String>,
    pub took_ms: u64,
}

impl DocumentOp {
    pub fn class(&self) -> RequestClass {
        RequestClass::Write
    }

    fn request(&self) -> AppResult<EsRequest> {
        fn query(options: &WriteOptions, extra: &[(&str, String)]) -> String {
            let mut params: Vec<String> = extra.iter().map(|(key, value)| format!("{key}={}", segment(value))).collect();
            let mut add = |key: &str, value: Option<String>| {
                if let Some(value) = value {
                    params.push(format!("{key}={}", segment(&value)));
                }
            };
            add("routing", options.routing.clone());
            add("if_seq_no", options.if_seq_no.map(|n| n.to_string()));
            add("if_primary_term", options.if_primary_term.map(|n| n.to_string()));
            add("refresh", options.refresh.clone());
            add("pipeline", options.pipeline.clone());
            if params.is_empty() {
                String::new()
            } else {
                format!("?{}", params.join("&"))
            }
        }
        let check_index = |index: &str| {
            if index.trim().is_empty() || index.contains('*') || index.contains(',') {
                Err(AppError::new(
                    "Document operations need one concrete index (no wildcards or lists).",
                ))
            } else {
                Ok(segment(index))
            }
        };
        Ok(match self {
            Self::Index {
                index,
                id,
                source,
                create_only,
                options,
            } => {
                if !source.is_object() {
                    return Err(AppError::new("A document must be a JSON object."));
                }
                let index = check_index(index)?;
                match id.as_deref().filter(|id| !id.is_empty()) {
                    Some(id) => {
                        let extra = if *create_only {
                            vec![("op_type", "create".to_string())]
                        } else {
                            Vec::new()
                        };
                        EsRequest::new(Method::PUT, format!("/{index}/_doc/{}{}", segment(id), query(options, &extra))).json(source.clone())
                    }
                    None => EsRequest::new(Method::POST, format!("/{index}/_doc{}", query(options, &[]))).json(source.clone()),
                }
            }
            Self::Update {
                index,
                id,
                doc,
                upsert,
                options,
            } => EsRequest::new(
                Method::POST,
                format!("/{}/_update/{}{}", check_index(index)?, segment(id), query(options, &[])),
            )
            .json(json!({ "doc": doc, "doc_as_upsert": upsert })),
            Self::UpdateScript {
                index,
                id,
                script,
                options,
            } => EsRequest::new(
                Method::POST,
                format!("/{}/_update/{}{}", check_index(index)?, segment(id), query(options, &[])),
            )
            .json(json!({ "script": script })),
            Self::Delete { index, id, options } => EsRequest::new(
                Method::DELETE,
                format!("/{}/_doc/{}{}", check_index(index)?, segment(id), query(options, &[])),
            ),
        })
    }
}

impl ByQueryOp {
    pub fn class(&self) -> RequestClass {
        match self.kind {
            ByQueryKind::Update => RequestClass::Destructive,
            ByQueryKind::Delete => RequestClass::Destructive,
        }
    }
}

impl EsEngine {
    /// `GET /<index>/_doc/<id>`. A missing document is `found: false`, not an error.
    pub async fn get_document(&self, index: &str, id: &str, routing: Option<&str>) -> AppResult<Document> {
        let mut path = format!("/{}/_doc/{}", segment(index), segment(id));
        if let Some(routing) = routing {
            path.push_str(&format!("?routing={}", segment(routing)));
        }
        let response = self.client.send(&EsRequest::new(Method::GET, path)).await?;
        if response.status >= 400 && !(response.status == 404 && response.body["found"] == false) {
            return Err(super::errors::from_response(response.status, &response.body));
        }
        let body = response.body;
        Ok(Document {
            index: body["_index"].as_str().unwrap_or(index).to_string(),
            id: body["_id"].as_str().unwrap_or(id).to_string(),
            found: body["found"].as_bool().unwrap_or(false),
            version: body["_version"].as_i64(),
            seq_no: body["_seq_no"].as_i64(),
            primary_term: body["_primary_term"].as_i64(),
            routing: body["_routing"].as_str().map(str::to_string),
            source: body.get("_source").cloned(),
        })
    }

    pub async fn apply_document_op(&self, op: &DocumentOp) -> AppResult<WriteResult> {
        let response = self.client.send(&op.request()?).await?;
        // Deleting a missing document answers 404 with `result: not_found`; report it rather than fail.
        if response.status >= 400 && response.body["result"] != "not_found" {
            return Err(super::errors::from_response(response.status, &response.body));
        }
        let body = response.body;
        Ok(WriteResult {
            index: body["_index"].as_str().unwrap_or_default().to_string(),
            id: body["_id"].as_str().unwrap_or_default().to_string(),
            result: body["result"].as_str().unwrap_or_default().to_string(),
            version: body["_version"].as_i64(),
            seq_no: body["_seq_no"].as_i64(),
            primary_term: body["_primary_term"].as_i64(),
        })
    }

    /// `_update_by_query` / `_delete_by_query`.
    pub async fn by_query(&self, op: &ByQueryOp) -> AppResult<ByQueryResult> {
        if op.index.trim().is_empty() {
            return Err(AppError::new("Choose the index to run this on."));
        }
        let api = match op.kind {
            ByQueryKind::Update => "_update_by_query",
            ByQueryKind::Delete => "_delete_by_query",
        };
        let mut params = vec![format!("wait_for_completion={}", !op.background), format!("refresh={}", op.refresh)];
        if op.proceed_on_conflicts {
            params.push("conflicts=proceed".into());
        }
        if let Some(max_docs) = op.max_docs {
            params.push(format!("max_docs={max_docs}"));
        }
        let mut body = json!({ "query": op.query });
        if let (ByQueryKind::Update, Some(script)) = (&op.kind, &op.script) {
            body["script"] = script.clone();
        }
        let path = format!("/{}/{api}?{}", segment(&op.index), params.join("&"));
        // Waiting for a big job can take long; give it ten minutes.
        let request = EsRequest::new(Method::POST, path)
            .json(body)
            .timeout(std::time::Duration::from_secs(600));
        let response = self.client.call(&request).await?;
        Ok(ByQueryResult {
            task: response["task"].as_str().map(str::to_string),
            total: response["total"].as_u64().unwrap_or(0),
            updated: response["updated"].as_u64().unwrap_or(0),
            deleted: response["deleted"].as_u64().unwrap_or(0),
            version_conflicts: response["version_conflicts"].as_u64().unwrap_or(0),
            failures: response["failures"]
                .as_array()
                .map(|failures| {
                    failures
                        .iter()
                        .map(|failure| failure["cause"]["reason"].as_str().unwrap_or("failure").to_string())
                        .collect()
                })
                .unwrap_or_default(),
            took_ms: response["took"].as_u64().unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_document_requests() {
        let options = WriteOptions {
            if_seq_no: Some(3),
            if_primary_term: Some(1),
            refresh: Some("wait_for".into()),
            ..Default::default()
        };
        let op = DocumentOp::Index {
            index: "products".into(),
            id: Some("a/b".into()),
            source: json!({ "x": 1 }),
            create_only: true,
            options,
        };
        let request = op.request().unwrap();
        assert_eq!(request.method, Method::PUT);
        assert_eq!(
            request.path,
            "/products/_doc/a%2Fb?op_type=create&if_seq_no=3&if_primary_term=1&refresh=wait_for"
        );
        let generated = DocumentOp::Index {
            index: "p".into(),
            id: None,
            source: json!({}),
            create_only: false,
            options: Default::default(),
        };
        assert_eq!(generated.request().unwrap().path, "/p/_doc");
        let wildcard = DocumentOp::Delete {
            index: "p*".into(),
            id: "1".into(),
            options: Default::default(),
        };
        assert!(wildcard.request().is_err());
        let not_object = DocumentOp::Index {
            index: "p".into(),
            id: None,
            source: json!([1]),
            create_only: false,
            options: Default::default(),
        };
        assert!(not_object.request().is_err());
        let delete = ByQueryOp {
            kind: ByQueryKind::Delete,
            index: "p".into(),
            query: json!({}),
            script: None,
            proceed_on_conflicts: false,
            background: false,
            refresh: false,
            max_docs: None,
        };
        assert_eq!(delete.class(), RequestClass::Destructive);
    }
}
