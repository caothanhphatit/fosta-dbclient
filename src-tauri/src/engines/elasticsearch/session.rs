//! Query-tab sessions in three languages:
//!
//! - `es-console`: Kibana Dev Tools requests (`METHOD /path` and a JSON or NDJSON body), run in order. Each request
//!   streams a hits table (`_index`, `_id`, `_score` and the `_source` fields) when the response has hits, then the
//!   response itself as one JSON cell with its HTTP status. An HTTP error stops the run with a structured error whose
//!   position points into the request's body.
//! - `esql` (Elasticsearch 8.11+) and `es-sql`: one query, its typed table streamed in batches up to the row limit
//!   (SQL cursors are followed, then closed).
//!
//! Stop is real: the canceller wakes the running request, which is dropped (the cluster cancels a search when its
//! connection closes), and then cancels any task the request left on the cluster, found by its `X-Opaque-Id`.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use reqwest::Method;
use serde_json::{json, Value as Json};
use tokio::sync::watch;

use super::{
    client::{EsRequest, EsResponse},
    connection::EsEngine,
    console::{self, ConsoleRequest},
    errors,
    query_lang::{parse_esql, parse_sql, TypedColumn},
    values,
};
use crate::{
    contract::{Batcher, Canceller, ColumnInfo, Flow, ResultSink, RunOptions, RunRequest, Session, StreamEvent, Value, ValueKind},
    error::{AppError, AppResult, ErrorPosition},
};

pub const CONSOLE: &str = "es-console";
pub const ESQL: &str = "esql";
pub const SQL: &str = "es-sql";

/// Console requests without a statement timeout may run this long (force merges, reindexing).
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(600);

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);

/// Shared by a session and its canceller.
#[derive(Default)]
struct CancelState {
    generation: Mutex<Option<watch::Sender<u64>>>,
    /// The `X-Opaque-Id` of the request in flight.
    in_flight: Mutex<Option<String>>,
}

impl CancelState {
    fn subscribe(&self) -> watch::Receiver<u64> {
        let mut guard = self.generation.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.get_or_insert_with(|| watch::channel(0).0).subscribe()
    }

    fn cancel(&self) {
        let guard = self.generation.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sender) = guard.as_ref() {
            sender.send_modify(|generation| *generation += 1);
        }
    }

    fn set_in_flight(&self, opaque_id: Option<String>) {
        *self.in_flight.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = opaque_id;
    }

    fn in_flight(&self) -> Option<String> {
        self.in_flight.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

fn cancelled() -> AppError {
    AppError::coded("CANCELLED", "The request was cancelled.")
}

pub struct EsSession {
    engine: EsEngine,
    language: String,
    state: Arc<CancelState>,
}

pub struct EsCanceller {
    engine: EsEngine,
    state: Arc<CancelState>,
}

/// Opens a session and its canceller.
pub fn open(engine: EsEngine, language: &str) -> (Arc<EsSession>, Arc<EsCanceller>) {
    let state = Arc::new(CancelState::default());
    (
        Arc::new(EsSession {
            engine: engine.clone(),
            language: language.to_owned(),
            state: Arc::clone(&state),
        }),
        Arc::new(EsCanceller { engine, state }),
    )
}

impl Canceller for EsCanceller {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>> {
        self.state.cancel();
        let opaque_id = self.state.in_flight();
        let engine = self.engine.clone();
        Box::pin(async move {
            if let Some(opaque_id) = opaque_id {
                cancel_tasks(&engine, &opaque_id).await;
            }
            Ok(())
        })
    }
}

/// Cancels the cluster tasks a request started (by-query jobs, reindexing), found by their `X-Opaque-Id`. Best effort:
/// the dropped connection already cancels searches.
async fn cancel_tasks(engine: &EsEngine, opaque_id: &str) {
    let Ok(tasks) = engine.client().get("/_tasks?detailed=true&group_by=none").await else {
        return;
    };
    for task in tasks["tasks"].as_array().into_iter().flatten() {
        let ours = task["headers"]["X-Opaque-Id"].as_str() == Some(opaque_id);
        if ours && task["cancellable"] == true {
            if let (Some(node), Some(id)) = (task["node"].as_str(), task["id"].as_u64()) {
                let _ = engine
                    .client()
                    .call(&EsRequest::new(Method::POST, format!("/_tasks/{node}:{id}/_cancel")))
                    .await;
            }
        }
    }
}

impl EsSession {
    /// Sends a request; a cancel drops it at once.
    async fn send(&self, mut request: EsRequest, cancel: &mut watch::Receiver<u64>) -> AppResult<EsResponse> {
        let opaque_id = format!("fosta-{}-{}", std::process::id(), NEXT_RUN.fetch_add(1, Ordering::Relaxed));
        request.opaque_id = Some(opaque_id.clone());
        self.state.set_in_flight(Some(opaque_id));
        let outcome = tokio::select! {
            response = self.engine.client().send(&request) => response,
            _ = cancel.changed() => Err(cancelled()),
        };
        self.state.set_in_flight(None);
        outcome
    }

    async fn run_console(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        let requests = console::parse(&request.text).map_err(|error| {
            let line = error
                .message()
                .strip_prefix("Line ")
                .and_then(|rest| rest.split(':').next())
                .and_then(|line| line.parse::<u32>().ok());
            let mut out = AppError::coded(errors::ENGINE_ERROR, error.message().to_owned()).with_vendor_code("console_parse_error");
            if let Some(line) = line {
                out = out.with_position(ErrorPosition::line(line));
            }
            out
        })?;
        if requests.is_empty() {
            return Err(AppError::new("Write a request such as GET /_cluster/health."));
        }
        let mut cancel = self.state.subscribe();
        let timeout = options.timeout.unwrap_or(CONSOLE_TIMEOUT);
        for (index, console_request) in requests.iter().enumerate() {
            let statement = format!("{} {}", console_request.method, console_request.path);
            sink.event(StreamEvent::Statement {
                index,
                sql: statement.clone(),
            });
            let http = console_request.to_es_request(timeout, None)?;
            let started = Instant::now();
            let response = self
                .send(http, &mut cancel)
                .await
                .map_err(|error| error.with_statement(index, &statement))?;
            for warning in &response.warnings {
                sink.event(StreamEvent::Notice {
                    index,
                    text: warning.clone(),
                });
            }
            if response.status >= 400 {
                let error = errors::from_response(response.status, &response.body).with_statement(index, &statement);
                return Err(errors::shift_position(error, (console_request.line + 1) as u32, &request.text));
            }
            if emit_console(index, console_request, &response, started, options, sink) {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn run_esql(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        if !self.engine.features().esql {
            return Err(AppError::new("ES|QL needs Elasticsearch 8.11 or newer."));
        }
        let text = request.text.trim();
        sink.event(StreamEvent::Statement {
            index: 0,
            sql: text.to_owned(),
        });
        let mut cancel = self.state.subscribe();
        let started = Instant::now();
        let http = EsRequest::new(Method::POST, "/_query?format=json")
            .json(json!({ "query": text }))
            .timeout(options.timeout.unwrap_or(CONSOLE_TIMEOUT));
        let response = self.send(http, &mut cancel).await?;
        if response.status >= 400 {
            return Err(query_position(errors::from_response(response.status, &response.body), text));
        }
        for warning in &response.warnings {
            sink.event(StreamEvent::Notice {
                index: 0,
                text: warning.clone(),
            });
        }
        let table = parse_esql(&response.body);
        emit_table(&table.columns, table.rows, false, started, options, sink);
        Ok(())
    }

    async fn run_sql(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        let text = request.text.trim().trim_end_matches(';');
        sink.event(StreamEvent::Statement {
            index: 0,
            sql: text.to_owned(),
        });
        let opensearch = self.engine.info().is_opensearch();
        let path = if opensearch {
            "/_plugins/_sql?format=jdbc"
        } else {
            "/_sql?format=json"
        };
        let mut cancel = self.state.subscribe();
        let started = Instant::now();
        let timeout = options.timeout.unwrap_or(CONSOLE_TIMEOUT);
        let fetch = options.row_limit.clamp(1, 1000) as u64;
        let first = EsRequest::new(Method::POST, path)
            .json(json!({ "query": text, "fetch_size": fetch }))
            .timeout(timeout);
        let response = self.send(first, &mut cancel).await?;
        if response.status >= 400 {
            return Err(query_position(errors::from_response(response.status, &response.body), text));
        }
        let mut table = parse_sql(&response.body);
        let columns = table.columns.clone();
        let mut rows = std::mem::take(&mut table.rows);
        let mut cursor = table.cursor.take();
        while let Some(current) = cursor.clone() {
            if rows.len() > options.row_limit {
                break;
            }
            let next = EsRequest::new(Method::POST, path)
                .json(json!({ "cursor": current }))
                .timeout(timeout);
            let response = self.send(next, &mut cancel).await?;
            if response.status >= 400 {
                return Err(errors::from_response(response.status, &response.body));
            }
            let mut page = parse_sql(&response.body);
            rows.append(&mut page.rows);
            cursor = page.cursor;
        }
        let truncated = rows.len() > options.row_limit || cursor.is_some();
        if let Some(cursor) = cursor {
            let close = if opensearch { "/_plugins/_sql/close" } else { "/_sql/close" };
            let _ = self.engine.client().post(close, json!({ "cursor": cursor })).await;
        }
        emit_table(&columns, rows, truncated, started, options, sink);
        Ok(())
    }
}

/// `line 1:15: …` in ES|QL and SQL errors as a position in the query.
fn query_position(error: AppError, text: &str) -> AppError {
    let message = error.message().to_owned();
    let Some(rest) = message.find("line ").map(|at| &message[at + 5..]) else {
        return error;
    };
    let mut parts = rest.splitn(3, ':');
    let (Some(Ok(line)), Some(Ok(column))) = (parts.next().map(str::parse::<u32>), parts.next().map(str::parse::<u32>)) else {
        return error;
    };
    let offset: usize = text
        .split('\n')
        .take(line.saturating_sub(1) as usize)
        .map(|l| l.chars().count() + 1)
        .sum::<usize>()
        + column.saturating_sub(1) as usize;
    error.with_position(ErrorPosition {
        line: Some(line),
        column: Some(column),
        offset: Some(offset as u32),
    })
}

fn column_infos(columns: &[TypedColumn]) -> Vec<ColumnInfo> {
    columns
        .iter()
        .map(|column| ColumnInfo::result(&column.name, &column.column_type, values::kind_of(&column.column_type)))
        .collect()
}

/// Streams a typed table as one result set.
fn emit_table(
    columns: &[TypedColumn],
    rows: Vec<Vec<Json>>,
    truncated: bool,
    started: Instant,
    options: &RunOptions,
    sink: &mut dyn ResultSink,
) {
    let infos = column_infos(columns);
    sink.event(StreamEvent::Columns {
        index: 0,
        columns: infos.clone(),
    });
    let mut batcher = Batcher::new(0, options);
    for row in rows {
        let typed = row
            .iter()
            .zip(&infos)
            .map(|(value, column)| values::to_value(value, &column.kind))
            .collect();
        if !batcher.push(typed, sink) {
            break;
        }
    }
    batcher.flush(sink);
    sink.event(StreamEvent::Done {
        index: 0,
        affected: None,
        rows: batcher.kept,
        truncated: truncated || batcher.stopped,
        elapsed_ms: started.elapsed().as_millis() as u64,
    });
}

/// How many documents a write response says it touched.
fn affected(body: &Json) -> Option<u64> {
    if let Some(items) = body["items"].as_array() {
        return Some(items.len() as u64);
    }
    for key in ["deleted", "updated", "created"] {
        if let Some(count) = body[key].as_u64() {
            let others: u64 = ["deleted", "updated", "created"]
                .iter()
                .filter(|other| **other != key)
                .filter_map(|other| body[*other].as_u64())
                .sum();
            return Some(count + others);
        }
    }
    match body["result"].as_str() {
        Some("created" | "updated" | "deleted") => Some(1),
        Some("noop" | "not_found") => Some(0),
        _ => None,
    }
}

/// Emits a console response: the hits table (when there are hits), then the response as one JSON row. Returns true
/// when the consumer asked to stop.
fn emit_console(
    index: usize,
    request: &ConsoleRequest,
    response: &EsResponse,
    started: Instant,
    options: &RunOptions,
    sink: &mut dyn ResultSink,
) -> bool {
    let elapsed = || started.elapsed().as_millis() as u64;
    if let Some(hits) = response.body["hits"]["hits"].as_array() {
        let mut fields: Vec<String> = Vec::new();
        for hit in hits {
            for key in hit["_source"].as_object().into_iter().flatten().map(|(key, _)| key) {
                if !fields.contains(key) {
                    fields.push(key.clone());
                }
            }
        }
        let mut columns = vec![
            ColumnInfo::result("_index", "_index", ValueKind::Text),
            ColumnInfo::result("_id", "_id", ValueKind::Text),
            ColumnInfo::result("_score", "float", ValueKind::Float),
        ];
        columns.extend(fields.iter().map(|field| ColumnInfo::result(field, "", ValueKind::Json)));
        sink.event(StreamEvent::Columns { index, columns });
        let mut batcher = Batcher::new(index, options);
        for hit in hits {
            let mut row = vec![
                values::generic(&hit["_index"]),
                values::generic(&hit["_id"]),
                values::generic(&hit["_score"]),
            ];
            row.extend(fields.iter().map(|field| values::generic(&hit["_source"][field])));
            if !batcher.push(row, sink) {
                break;
            }
        }
        batcher.flush(sink);
        // Stopped before the row limit and before the last hit: the consumer went away.
        let consumer_stopped = batcher.stopped && batcher.kept < options.row_limit && batcher.kept < hits.len();
        let done = sink.event(StreamEvent::Done {
            index,
            affected: None,
            rows: batcher.kept,
            truncated: hits.len() > batcher.kept,
            elapsed_ms: elapsed(),
        });
        if consumer_stopped || done == Flow::Stop {
            return true;
        }
        // The response is a result set of its own.
        sink.event(StreamEvent::Statement {
            index,
            sql: format!("{} {} (response)", request.method, request.path),
        });
    }
    let response_value = if response.is_json {
        Value::Json(response.body.to_string())
    } else {
        Value::Text(response.body.as_str().unwrap_or_default().to_owned())
    };
    sink.event(StreamEvent::Columns {
        index,
        columns: vec![
            ColumnInfo::result("status", "http status", ValueKind::Int),
            ColumnInfo::result(
                "response",
                if response.is_json { "json" } else { "text" },
                if response.is_json { ValueKind::Json } else { ValueKind::Text },
            ),
        ],
    });
    sink.event(StreamEvent::Rows {
        index,
        rows: vec![vec![Value::Int(i64::from(response.status)), response_value]],
    });
    let write = !matches!(request.method.as_str(), "GET" | "HEAD");
    sink.event(StreamEvent::Done {
        index,
        affected: if write { affected(&response.body) } else { None },
        rows: 1,
        truncated: false,
        elapsed_ms: elapsed(),
    }) == Flow::Stop
}

#[async_trait]
impl Session for EsSession {
    fn language(&self) -> &str {
        &self.language
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        match self.language.as_str() {
            CONSOLE => self.run_console(request, options, sink).await,
            ESQL => self.run_esql(request, options, sink).await,
            SQL => self.run_sql(request, options, sink).await,
            other => Err(AppError::new(format!("Unknown language '{other}'."))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::CollectSink;

    fn response(status: u16, body: Json) -> EsResponse {
        EsResponse {
            status,
            body,
            is_json: true,
            took_ms: 1,
            warnings: Vec::new(),
        }
    }

    fn names(columns: &[ColumnInfo]) -> Vec<&str> {
        columns.iter().map(|column| column.name.as_str()).collect()
    }

    #[test]
    fn search_responses_stream_a_hits_table_and_the_json() {
        let request = &console::parse("GET /products/_search").unwrap()[0];
        let body = json!({ "hits": { "hits": [
            { "_index": "products", "_id": "1", "_score": 1.0, "_source": { "name": "a", "n": 1 } },
            { "_index": "products", "_id": "2", "_score": 0.5, "_source": { "name": "b", "tags": ["x"] } }
        ] } });
        let mut sink = CollectSink::default();
        sink.event(StreamEvent::Statement {
            index: 0,
            sql: "GET".into(),
        });
        let options = RunOptions {
            row_limit: 1,
            ..RunOptions::default()
        };
        assert!(!emit_console(0, request, &response(200, body), Instant::now(), &options, &mut sink));
        assert_eq!(sink.sets.len(), 2);
        assert_eq!(names(&sink.sets[0].columns), vec!["_index", "_id", "_score", "name", "n", "tags"]);
        assert_eq!(sink.sets[0].rows.len(), 1);
        assert!(sink.sets[0].truncated, "two hits, row limit one");
        assert_eq!(names(&sink.sets[1].columns), vec!["status", "response"]);
        assert_eq!(sink.sets[1].rows[0][0], Value::Int(200));
        // Other responses are the JSON row only.
        let count = &console::parse("GET /products/_count").unwrap()[0];
        let mut sink = CollectSink::default();
        sink.event(StreamEvent::Statement {
            index: 0,
            sql: "GET".into(),
        });
        emit_console(
            0,
            count,
            &response(200, json!({ "count": 3 })),
            Instant::now(),
            &RunOptions::default(),
            &mut sink,
        );
        assert_eq!(sink.sets.len(), 1);
        assert_eq!(sink.sets[0].rows[0][1], Value::Json("{\"count\":3}".into()));
    }

    #[test]
    fn write_responses_report_affected_documents() {
        assert_eq!(affected(&json!({ "result": "created" })), Some(1));
        assert_eq!(affected(&json!({ "result": "noop" })), Some(0));
        assert_eq!(affected(&json!({ "items": [{}, {}, {}], "errors": false })), Some(3));
        assert_eq!(affected(&json!({ "deleted": 4, "total": 4 })), Some(4));
        assert_eq!(affected(&json!({ "acknowledged": true })), None);
    }

    #[test]
    fn query_errors_get_a_position() {
        let error = query_position(AppError::new("line 1:15: mismatched input 'x'"), "FROM logs | WHERE x");
        let position = error.field("position").unwrap();
        assert_eq!(
            (position["line"].as_u64(), position["column"].as_u64(), position["offset"].as_u64()),
            (Some(1), Some(15), Some(14))
        );
        let two = query_position(AppError::new("line 2:3: bad"), "FROM a\n| LIMIT x");
        assert_eq!(two.field("position").unwrap()["offset"], 9);
        assert!(query_position(AppError::new("no position"), "x").field("position").is_none());
    }

    #[tokio::test]
    async fn cancel_wakes_a_subscriber() {
        let state = CancelState::default();
        let mut receiver = state.subscribe();
        state.cancel();
        tokio::time::timeout(Duration::from_secs(1), receiver.changed())
            .await
            .unwrap()
            .unwrap();
        state.set_in_flight(Some("x".into()));
        assert_eq!(state.in_flight().as_deref(), Some("x"));
    }
}
