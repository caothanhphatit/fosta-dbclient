//! The collection tools, offered on connections whose engine filters with MongoDB JSON (`FilterSupport::raw`):
//!
//! - `open_collection_workspace` (OpenView) opens a collection's workspace on Documents or Aggregations with a filter,
//!   sort and projection, or pipeline stages, pre-filled and marked as suggested by the assistant. Nothing runs: the
//!   user reviews the suggestion and runs it.
//! - `sample_documents` (ReadRows, only where the user lets the assistant read data, like `run_read_query`) reads at
//!   most [`MAX_SAMPLE`] documents of the chat's database through the gateway (`gateway::assistant::invoke_read`:
//!   `mongo.find`, or `mongo.aggregatePreview` for what leaves a pipeline stage on a bounded sample). Values are cut
//!   per field before the model (and the chat) sees them, and the read is audited.

use serde_json::{json, Map, Value};

use super::{
    tools::{ToolSpec, MAX_CELL_CHARS, MAX_RESULT_CHARS},
    ChatContext, ChatEvent, ChatSink, UiAction,
};
use crate::{contract::RawFilterLang, gateway, runtime::ConnectionManager};

pub(super) const OPEN_WORKSPACE: &str = "open_collection_workspace";
pub(super) const SAMPLE_DOCUMENTS: &str = "sample_documents";
/// Documents `sample_documents` returns at most.
pub(super) const MAX_SAMPLE: u64 = 20;
const DEFAULT_SAMPLE: u64 = 5;
/// The bound `sample_documents` passes to the server (the gateway also caps the call).
const MAX_TIME_MS: u64 = 10_000;
/// Input documents an aggregation sample reads (the preview's own sample).
const PIPELINE_SAMPLE: u64 = 1000;
/// Array elements kept per array, and nesting kept, in a sampled document.
const MAX_ARRAY_ITEMS: usize = 10;
const MAX_DEPTH: usize = 6;
/// Top-level fields shown as columns in the chat.
const MAX_COLUMNS: usize = 12;
const SECTIONS: [&str; 2] = ["documents", "aggregations"];
/// Operators that run JavaScript on the server; a suggestion holding one is refused (the query bar refuses it too).
const DENIED: [&str; 3] = ["$where", "$function", "$accumulator"];

/// Whether the chat's connection has collections (its engine filters with MongoDB JSON).
pub(super) fn has_collections(db: &ConnectionManager, context: &ChatContext) -> bool {
    context
        .connection_id
        .as_deref()
        .and_then(|id| db.handle(id).ok())
        .is_some_and(|handle| handle.engine.descriptor().filter.raw == Some(RawFilterLang::MongoJson))
}

pub(super) fn definitions(read_rows: bool) -> Vec<ToolSpec> {
    let names = json!({
        "connection": { "type": "string", "description": "Leave out: always the chat's connection." },
        "database": { "type": "string", "description": "Database name; defaults to the current one." },
        "collection": { "type": "string", "description": "Collection (or view) name; defaults to the one the user is looking at." }
    });
    let mut open = names.clone();
    let properties = open.as_object_mut().expect("an object");
    properties.insert(
        "section".into(),
        json!({ "type": "string", "enum": SECTIONS, "description": "documents (query bar) or aggregations (pipeline builder)." }),
    );
    for (key, what) in [
        (
            "filter",
            "Filter document in mongosh syntax, e.g. { status: 'paid', total: { $gt: 100 } }.",
        ),
        ("sort", "Sort document, e.g. { createdAt: -1 }."),
        ("project", "Projection document, e.g. { name: 1, total: 1 }."),
    ] {
        properties.insert(key.into(), json!({ "type": "string", "description": what }));
    }
    properties.insert(
        "pipeline".into(),
        json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "Aggregation stages for the aggregations section, one stage document per item in mongosh syntax, e.g. [\"{ $match: { status: 'paid' } }\", \"{ $group: { _id: '$customerId', total: { $sum: '$total' } } }\"]."
        }),
    );
    let mut tools = vec![ToolSpec {
        name: OPEN_WORKSPACE,
        description: "Opens a collection's workspace in the app on its Documents section (query bar) or its Aggregations section (pipeline builder), with your filter, sort and projection, or your pipeline stages, pre-filled and marked as suggested by you. Nothing runs: the user reviews it and runs it. The user sees the documents; you do not.",
        schema: json!({ "type": "object", "properties": open, "required": ["section"], "additionalProperties": false }),
    }];
    if read_rows {
        let mut sample = names;
        let properties = sample.as_object_mut().expect("an object");
        properties.insert(
            "filter".into(),
            json!({ "type": "object", "description": "Filter as Extended JSON, e.g. { \"status\": \"paid\" } or { \"_id\": { \"$oid\": \"…\" } }." }),
        );
        properties.insert(
            "pipeline".into(),
            json!({
                "type": "array",
                "items": { "type": "object" },
                "description": "Aggregation stages as Extended JSON: the documents returned are those leaving stage upTo, on a sample of the collection (the first 1000 documents)."
            }),
        );
        properties.insert(
            "upTo".into(),
            json!({ "type": "integer", "minimum": 0, "description": "0-based stage index of pipeline whose output is returned; defaults to the last stage. To see what reaches stage n, pass n - 1." }),
        );
        properties.insert(
            "limit".into(),
            json!({ "type": "integer", "minimum": 1, "maximum": MAX_SAMPLE, "description": "Documents to read, at most 20 (default 5)." }),
        );
        tools.push(ToolSpec {
            name: SAMPLE_DOCUMENTS,
            description: "Reads a few documents of a collection in the chat's current database (at most 20, long values cut), optionally matching a filter, or as they leave a stage of an aggregation pipeline. Use it to see real field values and shapes before writing a filter or a stage. The documents are sent to you, so read only what you need. It never writes.",
            schema: json!({ "type": "object", "properties": sample, "additionalProperties": false }),
        });
    }
    tools
}

pub(super) fn describe(name: &str, input: &Value) -> String {
    let collection = text(input, "collection").unwrap_or_else(|| String::from("the collection"));
    match name {
        OPEN_WORKSPACE => {
            let section = if text(input, "section").as_deref() == Some("aggregations") {
                "Aggregations"
            } else {
                "Documents"
            };
            format!("Opened {section} of {collection}")
        }
        _ => format!("Sampling documents of {collection}"),
    }
}

fn text(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// A `connection` argument may only name the chat's own connection.
fn check_connection(input: &Value, context: &ChatContext) -> Result<(), String> {
    let Some(asked) = text(input, "connection") else {
        return Ok(());
    };
    let own = [context.connection_id.as_deref(), context.connection_name.as_deref()];
    if own.iter().flatten().any(|name| name.trim().eq_ignore_ascii_case(&asked)) {
        Ok(())
    } else {
        Err(format!(
            "Refused: the collection tools work on the chat's connection only, not `{asked}`. Leave `connection` out."
        ))
    }
}

fn collection(input: &Value, context: &ChatContext) -> Result<String, String> {
    text(input, "collection")
        .or_else(|| context.table.clone())
        .ok_or_else(|| String::from("`collection` is required"))
}

fn suggestion(input: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(value) = text(input, key) else {
        return Ok(None);
    };
    match DENIED.iter().find(|operator| value.contains(*operator)) {
        Some(operator) => Err(format!(
            "Refused: {operator} runs JavaScript on the server, which the app does not run."
        )),
        None => Ok(Some(value)),
    }
}

/// `open_collection_workspace`: the view request only, nothing is read or run.
pub(super) fn open_workspace(context: &ChatContext, input: &Value, events: &dyn ChatSink) -> Result<String, String> {
    check_connection(input, context)?;
    let database = text(input, "database")
        .or_else(|| context.database.clone())
        .ok_or("No database selected; pass `database`.")?;
    let collection = collection(input, context)?;
    let section = text(input, "section").unwrap_or_else(|| String::from("documents"));
    if !SECTIONS.contains(&section.as_str()) {
        return Err(format!("`section` is documents or aggregations, not `{section}`"));
    }
    let (filter, sort, project) = (
        suggestion(input, "filter")?,
        suggestion(input, "sort")?,
        suggestion(input, "project")?,
    );
    let pipeline = match input.get("pipeline") {
        None | Some(Value::Null) => None,
        Some(Value::Array(stages)) => {
            let mut texts = Vec::with_capacity(stages.len());
            for stage in stages {
                let text = match stage {
                    Value::String(text) => text.trim().to_owned(),
                    other => other.to_string(),
                };
                if let Some(operator) = DENIED.iter().find(|operator| text.contains(*operator)) {
                    return Err(format!(
                        "Refused: {operator} runs JavaScript on the server, which the app does not run."
                    ));
                }
                if !text.is_empty() {
                    texts.push(text);
                }
            }
            Some(texts)
        }
        Some(_) => return Err(String::from("`pipeline` is an array of stage documents")),
    };
    if section == "documents" && pipeline.is_some() {
        return Err(String::from(
            "A pipeline opens in the aggregations section; pass section \"aggregations\".",
        ));
    }
    if section == "aggregations" && (filter.is_some() || sort.is_some() || project.is_some()) {
        return Err(String::from(
            "filter, sort and project fill the documents section; in aggregations, pass them as $match, $sort and $project stages of `pipeline`.",
        ));
    }
    let suggested = filter.is_some() || sort.is_some() || project.is_some() || pipeline.as_ref().is_some_and(|stages| !stages.is_empty());
    let _ = events.send(ChatEvent::Action {
        action: UiAction::OpenCollectionWorkspace {
            database: database.clone(),
            collection: collection.clone(),
            section: section.clone(),
            suggested_filter: filter,
            suggested_sort: sort,
            suggested_project: project,
            suggested_pipeline: pipeline,
        },
    });
    Ok(if suggested {
        format!(
            "Opened the {section} of {database}.{collection} with your suggestion pre-filled. It has not run: the user \
             reviews it and runs it. You do not see the documents."
        )
    } else {
        format!("Opened the {section} of {database}.{collection}. The user sees the documents; you do not.")
    })
}

/// A JSON argument given as a value or as JSON text.
fn json_value(input: &Value, key: &str) -> Result<Option<Value>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => serde_json::from_str(text).map(Some).map_err(|error| {
            format!(
                "`{key}` must be Extended JSON ({error}); write ObjectId(\"…\") as {{\"$oid\": \"…\"}} and dates as {{\"$date\": \"…\"}}."
            )
        }),
        Some(value) => Ok(Some(value.clone())),
    }
}

/// `sample_documents`: through the gateway as the assistant, in the chat's database only.
pub(super) async fn sample(
    db: &ConnectionManager,
    context: &ChatContext,
    reads: Option<&gateway::Context>,
    input: &Value,
    events: &dyn ChatSink,
) -> Result<String, String> {
    let reads = reads.ok_or(
        "Reading documents is turned off for this workspace. Write the filter or pipeline in a ```mongo-filter or \
         ```mongo-pipeline block for the user to apply instead.",
    )?;
    check_connection(input, context)?;
    // Pinned to the chat's database, like run_read_query: data cannot point the model at another one.
    let database = context
        .database
        .clone()
        .ok_or("Refused: no database is selected in the chat. Ask the user to open the collection first.")?;
    if let Some(asked) = text(input, "database").filter(|asked| *asked != database) {
        return Err(format!(
            "Refused: sample_documents reads only in the chat's current database `{database}`, not `{asked}`. Leave \
             `database` out."
        ));
    }
    let collection = collection(input, context)?;
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SAMPLE)
        .clamp(1, MAX_SAMPLE);
    let filter = json_value(input, "filter")?.filter(|filter| filter.as_object().is_none_or(|map| !map.is_empty()));
    if filter.as_ref().is_some_and(|filter| !filter.is_object()) {
        return Err(String::from("`filter` is a document"));
    }
    let stages = match json_value(input, "pipeline")? {
        None => None,
        Some(Value::Array(stages)) if stages.is_empty() => None,
        Some(Value::Array(stages)) => Some(stages),
        Some(_) => return Err(String::from("`pipeline` is an array of stage documents")),
    };
    let (operation, args, statement) = match stages {
        None => {
            let shown = filter.clone().unwrap_or_else(|| json!({}));
            let statement = format!(
                "db.getCollection({}).find({shown}).limit({limit})",
                Value::String(collection.clone())
            );
            let mut args = json!({ "database": database, "collection": collection, "limit": limit, "maxTimeMS": MAX_TIME_MS });
            if let Some(filter) = filter {
                args["filter"] = filter;
            }
            ("mongo.find", args, statement)
        }
        Some(stages) => {
            let last = stages.len() as u64 - 1;
            let wanted = input.get("upTo").and_then(Value::as_u64).unwrap_or(last).min(last);
            let mut pipeline = Vec::with_capacity(stages.len() + 1);
            if let Some(filter) = filter {
                pipeline.push(json!({ "$match": filter }));
            }
            let shift = pipeline.len() as u64;
            pipeline.extend(stages.into_iter().take(wanted as usize + 1));
            let statement = format!(
                "db.getCollection({}).aggregate({}) on a sample, documents leaving stage {}",
                Value::String(collection.clone()),
                Value::Array(pipeline.clone()),
                wanted + 1
            );
            let args = json!({
                "database": database,
                "collection": collection,
                "pipeline": pipeline,
                "upTo": wanted + shift,
                "sampleSize": PIPELINE_SAMPLE,
                "limit": limit,
                "maxTimeMS": MAX_TIME_MS,
            });
            ("mongo.aggregatePreview", args, statement)
        }
    };
    let outcome = gateway::assistant::invoke_read(db, reads, operation, args)
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| documents_of(operation, &result).map(|(documents, more)| (documents, more, result)));
    let elapsed_ms = outcome
        .as_ref()
        .ok()
        .and_then(|(_, _, result)| result.get("elapsedMs").and_then(Value::as_u64))
        .unwrap_or_default();
    let (documents, more) = match &outcome {
        Ok((documents, more, _)) => (documents.iter().take(limit as usize).map(cut).collect::<Vec<_>>(), *more),
        Err(_) => (Vec::new(), false),
    };
    let (columns, rows) = table_of(&documents);
    let _ = events.send(ChatEvent::Query {
        statement,
        database: Some(database),
        columns,
        rows,
        truncated: more,
        elapsed_ms,
        error: outcome.as_ref().err().cloned(),
    });
    outcome.map(|_| model_text(&documents, more))
}

/// The documents of a `mongo.find` result, or those leaving the last stage of a preview (its error when it failed).
fn documents_of(operation: &str, result: &Value) -> Result<(Vec<Value>, bool), String> {
    let list = |value: Option<&Value>| value.and_then(Value::as_array).cloned().unwrap_or_default();
    if operation == "mongo.find" {
        let more = result.get("hasMore").and_then(Value::as_bool).unwrap_or(false);
        return Ok((list(result.get("documents")), more));
    }
    let stages = list(result.get("stages"));
    let Some(last) = stages.last() else {
        return Ok((Vec::new(), false));
    };
    if let Some(error) = last.get("error").and_then(Value::as_str) {
        return Err(format!(
            "Stage {} failed on the sample: {error}",
            last["index"].as_u64().unwrap_or(0) + 1
        ));
    }
    let documents = list(last.get("documents"));
    let count = last.get("count").and_then(Value::as_u64).unwrap_or(0);
    Ok((documents.clone(), count > documents.len() as u64))
}

/// A document with long strings, long arrays and deep nesting cut, so one field cannot fill the answer.
fn cut(value: &Value) -> Value {
    cut_at(value, 0)
}

fn cut_at(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(text) => Value::String(cut_text(text)),
        Value::Array(items) if depth >= MAX_DEPTH => Value::String(format!("[… {} item(s)]", items.len())),
        Value::Object(map) if depth >= MAX_DEPTH => Value::String(format!("{{… {} field(s)}}", map.len())),
        Value::Array(items) => {
            let mut kept: Vec<Value> = items.iter().take(MAX_ARRAY_ITEMS).map(|item| cut_at(item, depth + 1)).collect();
            if items.len() > MAX_ARRAY_ITEMS {
                kept.push(Value::String(format!("… {} more item(s)", items.len() - MAX_ARRAY_ITEMS)));
            }
            Value::Array(kept)
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, inner)| (cut_text(key), cut_at(inner, depth + 1)))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

/// One line, at most [`MAX_CELL_CHARS`] characters (like a `run_read_query` cell).
fn cut_text(text: &str) -> String {
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if flat.chars().count() > MAX_CELL_CHARS {
        format!("{}…", flat.chars().take(MAX_CELL_CHARS).collect::<String>())
    } else {
        flat
    }
}

/// The documents as the chat shows them: top-level fields as columns, each cell the value's JSON cut to one cell.
fn table_of(documents: &[Value]) -> (Vec<String>, Vec<Vec<String>>) {
    let mut columns: Vec<String> = Vec::new();
    for document in documents {
        for key in document.as_object().into_iter().flat_map(Map::keys) {
            if columns.len() < MAX_COLUMNS && !columns.contains(key) {
                columns.push(key.clone());
            }
        }
    }
    let rows = documents
        .iter()
        .map(|document| {
            columns
                .iter()
                .map(|column| match document.get(column) {
                    None => String::new(),
                    Some(Value::String(text)) => text.clone(),
                    Some(value) => cut_text(&value.to_string()),
                })
                .collect()
        })
        .collect();
    (columns, rows)
}

/// The documents for the model: one relaxed Extended JSON line each, within [`MAX_RESULT_CHARS`].
fn model_text(documents: &[Value], more: bool) -> String {
    if documents.is_empty() {
        return String::from("No documents matched.");
    }
    let mut text = format!("{} document(s)", documents.len());
    if more {
        text.push_str(" (more match; only these were read)");
    }
    text.push_str(", long values cut:");
    let mut shown = 0;
    for document in documents {
        let line = document.to_string();
        if text.len() + line.len() + 1 > MAX_RESULT_CHARS {
            break;
        }
        text.push('\n');
        text.push_str(&line);
        shown += 1;
    }
    if shown < documents.len() {
        text.push_str(&format!(
            "\n… ({} more document(s) left out to keep the answer short)",
            documents.len() - shown
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, OnceLock};

    use async_trait::async_trait;

    use super::*;
    use crate::{
        contract::{
            testing::fake, CallContext, Category, Classification, ClassifyInput, ConnectRequest, Connection, Engine, EngineDescriptor,
            FilterSupport, ObjectDescription, ObjectRef, OpenedSession, ServerInfo, SessionOptions, StatementInfo, TreeChildren,
            TreeRequest,
        },
        error::{AppError, AppResult},
    };

    /// A document engine: `mongo.find` and `mongo.aggregatePreview` read, everything else writes; it records each call.
    struct Docs {
        calls: Mutex<Vec<(String, Value)>>,
    }

    fn docs() -> &'static Docs {
        Box::leak(Box::new(Docs {
            calls: Mutex::new(Vec::new()),
        }))
    }

    #[async_trait]
    impl Engine for Docs {
        fn descriptor(&self) -> &EngineDescriptor {
            static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
            DESCRIPTOR.get_or_init(|| EngineDescriptor {
                id: "docs".into(),
                filter: FilterSupport {
                    structured: false,
                    raw: Some(RawFilterLang::MongoJson),
                    placeholder: String::new(),
                },
                ..fake::descriptor()
            })
        }

        fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
            match input {
                ClassifyInput::Capability { name, .. } if matches!(*name, "mongo.find" | "mongo.aggregatePreview") => {
                    Classification::single(StatementInfo::new(Category::Read, name, name))
                }
                ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::new(Category::Write, name, name)),
                _ => Classification::default(),
            }
        }

        async fn connect(&self, _request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
            Err(AppError::new("not used"))
        }
    }

    struct DocsConnection {
        engine: &'static Docs,
        info: ServerInfo,
    }

    #[async_trait]
    impl Connection for DocsConnection {
        fn engine_id(&self) -> &str {
            "docs"
        }
        fn info(&self) -> &ServerInfo {
            &self.info
        }
        fn capabilities(&self) -> crate::contract::CapabilitySet {
            self.engine.descriptor().capabilities
        }
        async fn children(&self, _request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
            Err(AppError::new("not used"))
        }
        async fn describe(&self, _object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
            Err(AppError::new("not used"))
        }
        async fn open_session(&self, _options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
            Err(AppError::new("not used"))
        }
        async fn invoke(&self, name: &str, args: Value, ctx: &CallContext) -> AppResult<Value> {
            assert!(ctx.limits.read_only, "the assistant reads read-only");
            self.engine.calls.lock().unwrap().push((name.to_owned(), args.clone()));
            let document = |n: u64| json!({ "_id": n, "note": "x".repeat(5000), "tags": (0..30).collect::<Vec<_>>() });
            let limit = args["limit"].as_u64().unwrap_or(5);
            let documents: Vec<Value> = (0..limit).map(document).collect();
            Ok(match name {
                "mongo.find" => json!({ "documents": documents, "hasMore": true, "elapsedMs": 3 }),
                _ => {
                    json!({ "stages": [{ "index": args["upTo"], "operator": "$match", "count": 40, "documents": documents, "error": null }], "elapsedMs": 4 })
                }
            })
        }
        async fn close(&self) {}
    }

    fn setup() -> (ConnectionManager, &'static Docs, ChatContext) {
        let db = ConnectionManager::default();
        let engine = docs();
        db.insert(
            "m",
            engine,
            Arc::new(DocsConnection {
                engine,
                info: ServerInfo::default(),
            }),
        );
        let context: ChatContext =
            serde_json::from_value(json!({ "connectionId": "m", "connectionName": "Local Mongo", "database": "shop", "table": "orders" }))
                .unwrap();
        (db, engine, context)
    }

    fn reads() -> gateway::Context {
        gateway::Context {
            workspace_id: "w".into(),
            workspace_name: "Team".into(),
            connection_id: "m".into(),
            connection_name: "Local Mongo".into(),
            safety: gateway::rules::SafetySettings::preset(gateway::rules::Preset::Guarded),
        }
    }

    #[test]
    fn sampling_is_offered_only_with_reads() {
        let names = |reads: bool| -> Vec<&'static str> { definitions(reads).iter().map(|tool| tool.name).collect() };
        assert_eq!(names(false), vec![OPEN_WORKSPACE]);
        assert_eq!(names(true), vec![OPEN_WORKSPACE, SAMPLE_DOCUMENTS]);
        let (db, _, context) = setup();
        assert!(has_collections(&db, &context));
        let sql: ChatContext = serde_json::from_value(json!({ "connectionId": "none" })).unwrap();
        assert!(!has_collections(&db, &sql));
    }

    #[test]
    fn opening_a_workspace_only_asks_the_ui() {
        let (db, engine, context) = setup();
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = seen.clone();
        let events = move |event: ChatEvent| sink.lock().unwrap().push(serde_json::to_value(&event).unwrap());
        let input = json!({ "section": "documents", "filter": "{ status: 'paid' }", "sort": "{ total: -1 }" });
        let text = open_workspace(&context, &input, &events).unwrap();
        assert!(text.contains("has not run"), "{text}");
        let pipeline = json!({ "section": "aggregations", "pipeline": ["{ $match: {} }", { "$limit": 5 }] });
        open_workspace(&context, &pipeline, &events).unwrap();
        let events = seen.lock().unwrap().clone();
        assert_eq!(events[0]["action"]["kind"], "openCollectionWorkspace");
        assert_eq!(events[0]["action"]["collection"], "orders");
        assert_eq!(events[0]["action"]["suggestedFilter"], "{ status: 'paid' }");
        assert_eq!(
            events[1]["action"]["suggestedPipeline"],
            json!(["{ $match: {} }", "{\"$limit\":5}"])
        );
        // Server-side JavaScript, another connection and mismatched sections are refused before the UI sees them.
        let refused = [
            json!({ "section": "documents", "filter": "{ $where: 'sleep(1000)' }" }),
            json!({ "section": "aggregations", "pipeline": ["{ $group: { _id: 1, x: { $accumulator: {} } } }"] }),
            json!({ "section": "documents", "connection": "prod" }),
            json!({ "section": "documents", "pipeline": ["{ $match: {} }"] }),
            json!({ "section": "schema" }),
        ];
        let quiet = |_: ChatEvent| {};
        for input in refused {
            assert!(open_workspace(&context, &input, &quiet).is_err(), "{input}");
        }
        assert!(open_workspace(&context, &json!({ "section": "documents", "connection": "local mongo" }), &quiet).is_ok());
        // Nothing reached the engine.
        assert!(engine.calls.lock().unwrap().is_empty());
        drop(db);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sampling_reads_through_the_gateway_with_cut_values() {
        let (db, engine, context) = setup();
        let quiet = |_: ChatEvent| {};
        // Off where the workspace does not let the assistant read data, even when the model calls it unasked.
        let off = sample(&db, &context, None, &json!({}), &quiet).await.unwrap_err();
        assert!(off.contains("turned off"), "{off}");
        let allowed = reads();
        let other = sample(&db, &context, Some(&allowed), &json!({ "database": "admin" }), &quiet).await;
        assert!(other.unwrap_err().contains("current database `shop`"));
        assert!(engine.calls.lock().unwrap().is_empty(), "refused before the engine");

        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let sink = seen.clone();
        let events = move |event: ChatEvent| sink.lock().unwrap().push(serde_json::to_value(&event).unwrap());
        let input = json!({ "filter": { "status": "paid" }, "limit": 500 });
        let text = sample(&db, &context, Some(&allowed), &input, &events).await.unwrap();
        assert!(text.starts_with("20 document(s) (more match"), "{text}");
        assert!(text.len() <= MAX_RESULT_CHARS + 200, "{}", text.len());
        assert!(!text.contains(&"x".repeat(MAX_CELL_CHARS + 1)), "values are cut");
        assert!(text.contains("more item(s)"), "{text}");
        let calls = engine.calls.lock().unwrap().clone();
        assert_eq!(calls[0].0, "mongo.find");
        assert_eq!(calls[0].1["limit"], 20);
        assert_eq!(calls[0].1["filter"], json!({ "status": "paid" }));
        let shown = seen.lock().unwrap().clone();
        assert_eq!(shown[0]["type"], "query");
        assert_eq!(shown[0]["columns"], json!(["_id", "note", "tags"]));
        assert_eq!(shown[0]["rows"].as_array().unwrap().len(), 20);

        // What leaves a stage: a preview of the stages up to it, the filter as a leading $match.
        let input =
            json!({ "filter": { "a": 1 }, "pipeline": [{ "$match": {} }, { "$unwind": "$tags" }, { "$limit": 1 }], "upTo": 1, "limit": 2 });
        sample(&db, &context, Some(&allowed), &input, &quiet).await.unwrap();
        let (name, args) = engine.calls.lock().unwrap()[1].clone();
        assert_eq!(name, "mongo.aggregatePreview");
        assert_eq!(args["pipeline"].as_array().unwrap().len(), 3);
        assert_eq!(args["pipeline"][0], json!({ "$match": { "a": 1 } }));
        assert_eq!(args["upTo"], 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sampling_is_a_read_for_the_gateway() {
        let (db, engine, _) = setup();
        let allowed = reads();
        // An operation the engine classifies as a write is refused for the assistant whatever the rules.
        let refused = gateway::assistant::invoke_read(&db, &allowed, "mongo.updateMany", json!({})).await;
        assert!(refused.is_err());
        // A Read-only workspace still lets reads through; reads that the rules confirm are refused (no one to confirm).
        let read_only = gateway::Context {
            safety: gateway::rules::SafetySettings::preset(gateway::rules::Preset::ReadOnly),
            ..reads()
        };
        gateway::assistant::invoke_read(&db, &read_only, "mongo.find", json!({ "limit": 1 }))
            .await
            .unwrap();
        let calls: Vec<String> = engine.calls.lock().unwrap().iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(calls, vec!["mongo.find"]);
    }

    #[tokio::test]
    async fn the_prompt_names_the_fences_and_tools_for_collections() {
        let (db, _, context) = setup();
        let prompt = super::super::prompt::system_prompt(&db, &context, false).await;
        assert!(
            prompt.contains("```mongo-filter") && prompt.contains("```mongo-pipeline"),
            "{prompt}"
        );
        assert!(prompt.contains("mongosh syntax") && prompt.contains(OPEN_WORKSPACE), "{prompt}");
        assert!(!prompt.contains(SAMPLE_DOCUMENTS), "no sampling without reads: {prompt}");
        let reads = super::super::prompt::system_prompt(&db, &context, true).await;
        assert!(reads.contains(SAMPLE_DOCUMENTS), "{reads}");
        // Other engines do not get them.
        let plain = super::super::prompt::system_prompt(&db, &ChatContext::default(), true).await;
        assert!(!plain.contains("mongo-filter"), "{plain}");
    }

    #[test]
    fn documents_are_cut_per_field() {
        let deep = json!({ "a": { "b": { "c": { "d": { "e": { "f": { "g": 1 } } } } } } });
        assert_eq!(cut(&deep)["a"]["b"]["c"]["d"]["e"]["f"], "{… 1 field(s)}");
        assert_eq!(cut(&json!("a\nb")), "a b");
        let (columns, rows) = table_of(&[json!({ "_id": 1, "nested": { "x": 1 } })]);
        assert_eq!(columns, vec!["_id", "nested"]);
        assert_eq!(rows, vec![vec!["1".to_owned(), "{\"x\":1}".to_owned()]]);
        assert_eq!(model_text(&[], false), "No documents matched.");
    }
}
