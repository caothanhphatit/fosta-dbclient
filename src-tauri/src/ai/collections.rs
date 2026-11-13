//! The collection tools, offered on connections whose engine filters with MongoDB JSON (`FilterSupport::raw`):
//!
//! - `open_collection_workspace` (OpenView) opens a collection's workspace on Documents or Aggregations with a filter,
//!   sort and projection, or pipeline stages, pre-filled and marked as suggested by the assistant. Nothing runs, not
//!   even Documents' first page: the user reviews the suggestion and runs it.
//!
//! Both are pinned to the chat's connection and database, refuse system collections (`system.*`) and the `admin`,
//! `local` and `config` databases, and refuse suggestions or pipelines that write (`$out`, `$merge`), run JavaScript on
//! the server, or name another database (`{ db, coll }` in `$lookup`, `$unionWith`, `$merge.into`).
//! - `sample_documents` (ReadRows, only where the user lets the assistant read data, like `run_read_query`) reads at
//!   most [`MAX_SAMPLE`] documents of the chat's database through the gateway (`gateway::assistant::invoke_read`:
//!   `mongo.find`, or `mongo.aggregatePreview` for what leaves a pipeline stage on a bounded sample). Values are
//!   redacted like learned profiles (`profile::redact::redact_json`: fields named like credentials keep their key and
//!   lose their value, secret-looking text is replaced, at any depth and in arrays) and cut per field before the model
//!   and the chat see them, and the read is audited.

use serde_json::{json, Map, Value};

use super::{
    profile::redact,
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
/// Stages that write into a collection (`$out` drops and replaces its target); the tools never suggest or run them.
const WRITE_STAGES: [&str; 2] = ["$out", "$merge"];
/// Databases the collection tools never open or read: users and credentials (`admin`), the oplog and the startup log
/// (`local`), the cluster's metadata and sessions (`config`).
const RESTRICTED_DATABASES: [&str; 3] = ["admin", "local", "config"];

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
        "database": { "type": "string", "description": "Leave out: always the chat's current database (another one is refused)." },
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
        description: "Opens a collection's workspace of the chat's current database in the app on its Documents section (query bar) or its Aggregations section (pipeline builder), with your filter, sort and projection, or your pipeline stages, pre-filled and marked as suggested by you. Nothing runs: the user reviews it and runs it. The user sees the documents; you do not. System collections, the admin, local and config databases, stages that write ($out, $merge) and stages naming another database are refused.",
        schema: json!({ "type": "object", "properties": open, "required": ["section"], "additionalProperties": false }),
    }];
    if read_rows {
        let mut sample = names;
        let properties = sample.as_object_mut().expect("an object");
        // Documents travel as JSON text: an `object` schema without properties reaches strict-schema providers as `{}`,
        // which drops every operator key ($group, $match) the model wrote.
        properties.insert(
            "filter".into(),
            json!({ "type": "string", "description": "Filter as Extended JSON text (keys in double quotes), e.g. {\"status\": \"paid\"} or {\"_id\": {\"$oid\": \"…\"}}." }),
        );
        properties.insert(
            "pipeline".into(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Aggregation stages, one stage per item as Extended JSON text (keys in double quotes), e.g. [\"{\\\"$match\\\": {\\\"status\\\": \\\"paid\\\"}}\", \"{\\\"$group\\\": {\\\"_id\\\": \\\"$status\\\", \\\"n\\\": {\\\"$sum\\\": 1}}}\"]. The documents returned are those leaving stage upTo, on a sample of the collection (the first 1000 documents)."
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
            description: "Reads a few documents of a collection in the chat's current database (at most 20, credentials redacted, long values cut), optionally matching a filter, or as they leave a stage of an aggregation pipeline. Use it to see real field values and shapes before writing a filter or a stage. The documents are sent to you, so read only what you need. It never writes.",
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

/// The chat's own database: the collection tools are pinned to it, so data cannot point the model at another one.
fn pinned_database(input: &Value, context: &ChatContext, tool: &str) -> Result<String, String> {
    let database = context
        .database
        .clone()
        .ok_or("Refused: no database is selected in the chat. Ask the user to open the collection first.")?;
    if let Some(asked) = text(input, "database").filter(|asked| *asked != database) {
        return Err(format!(
            "Refused: {tool} works only in the chat's current database `{database}`, not `{asked}`. Leave `database` \
             out; the user opens other databases themselves."
        ));
    }
    Ok(database)
}

/// Whether the collection tools refuse this namespace: system collections (`system.*`: users, roles, the profiler) and
/// the `admin`, `local` and `config` databases. Also used for the sampled documents of the prompt.
pub(super) fn restricted_namespace(database: &str, collection: &str) -> Option<String> {
    let database = database.trim();
    let collection = collection.trim();
    if RESTRICTED_DATABASES
        .iter()
        .any(|restricted| restricted.eq_ignore_ascii_case(database))
    {
        return Some(format!(
            "Refused: the `{database}` database holds server internals (users, the oplog, sessions); the assistant does \
             not open or read it."
        ));
    }
    if collection.starts_with("system.") {
        return Some(format!(
            "Refused: `{collection}` is a system collection; the assistant does not open or read system collections."
        ));
    }
    None
}

/// Whether `text` holds `operator` as a whole word (`$out`, not `$outcome`).
fn mentions(text: &str, operator: &str) -> bool {
    text.match_indices(operator).any(|(at, _)| {
        !text[at + operator.len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_alphanumeric() || next == '_')
    })
}

/// Whether mongosh `text` has `key` as an object key (`{ db: …`, `, "db": …`), not as part of a longer name or a value.
fn has_key(text: &str, key: &str) -> bool {
    text.match_indices(key).any(|(at, _)| {
        let before = text[..at].trim_end_matches(['"', '\'']).trim_end();
        let after = text[at + key.len()..].trim_start_matches(['"', '\'']).trim_start();
        (before.ends_with('{') || before.ends_with(',')) && after.starts_with(':')
    })
}

/// The refusal for a suggestion (mongosh text): server-side JavaScript, a stage that writes, or a `{ db, coll }` that
/// names a database (the database cannot be compared reliably in mongosh text, so any is refused).
fn refuse_text(text: &str) -> Result<(), String> {
    if let Some(operator) = DENIED.iter().find(|operator| text.contains(*operator)) {
        return Err(format!(
            "Refused: {operator} runs JavaScript on the server, which the app does not run."
        ));
    }
    if let Some(stage) = WRITE_STAGES.iter().find(|stage| mentions(text, stage)) {
        return Err(format!(
            "Refused: {stage} writes into a collection ({}); suggestions only read. If the user asked for it, write the \
             pipeline in a ```mongo-pipeline block and say what it overwrites.",
            if *stage == "$out" {
                "it drops and replaces the target"
            } else {
                "it changes the target"
            }
        ));
    }
    if has_key(text, "db") && has_key(text, "coll") {
        return Err(String::from(
            "Refused: a stage that names a database ({ db, coll }) reaches outside the chat's database. Name the \
             collection alone (it is read in the chat's database).",
        ));
    }
    Ok(())
}

/// The refusal for a pipeline given as JSON: a stage that writes, or a `{ db, coll }` naming another database than the
/// chat's (`$lookup` / `$graphLookup` `from`, `$unionWith`, `$merge.into`, `$out`), at any depth.
fn refuse_json(value: &Value, database: &str) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            if let Some(stage) = WRITE_STAGES.iter().find(|stage| map.contains_key(**stage)) {
                return Err(format!("Refused: {stage} writes into a collection; sample_documents only reads."));
            }
            if let Some(other) = map.get("db").and_then(Value::as_str).filter(|other| *other != database) {
                if map.contains_key("coll") {
                    return Err(format!(
                        "Refused: the pipeline reads `{other}`, another database than the chat's `{database}`. Name \
                         collections of `{database}` alone."
                    ));
                }
            }
            map.values().try_for_each(|inner| refuse_json(inner, database))
        }
        Value::Array(items) => items.iter().try_for_each(|inner| refuse_json(inner, database)),
        _ => Ok(()),
    }
}

fn suggestion(input: &Value, key: &str) -> Result<Option<String>, String> {
    let Some(value) = text(input, key) else {
        return Ok(None);
    };
    refuse_text(&value)?;
    Ok(Some(value))
}

/// `open_collection_workspace`: the view request only, nothing is read or run. Pinned to the chat's connection and
/// database; system collections and the admin, local and config databases are refused. The UI opens the section
/// without running it (Documents waits for Find), so what the assistant opens never runs on its own.
pub(super) fn open_workspace(context: &ChatContext, input: &Value, events: &dyn ChatSink) -> Result<String, String> {
    check_connection(input, context)?;
    let database = pinned_database(input, context, OPEN_WORKSPACE)?;
    let collection = collection(input, context)?;
    if let Some(refused) = restricted_namespace(&database, &collection) {
        return Err(refused);
    }
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
                    other => {
                        refuse_json(other, &database)?;
                        other.to_string()
                    }
                };
                refuse_text(&text)?;
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
        format!(
            "Opened the {section} of {database}.{collection}. Nothing has run: the user runs it. You do not see the \
             documents."
        )
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

/// What the model is told when a stage cannot be used, so it does not retry the same call.
const STAGE_HINT: &str = "Pass each stage as one Extended JSON string with double-quoted keys, e.g. \
     \"{\\\"$group\\\": {\\\"_id\\\": \\\"$status\\\", \\\"n\\\": {\\\"$sum\\\": 1}}}\". If it still fails, write the \
     pipeline in a ```mongo-pipeline block for the user instead of calling the tool again.";

/// The `pipeline` argument of `sample_documents` as stage documents; `None` when absent or empty. Stages arrive as JSON
/// text (the schema), as documents (providers that keep them), or the whole pipeline as one JSON text. Each must be a
/// document with exactly one stage operator: an empty `{}` is what a provider sends after dropping the operator keys.
fn pipeline_stages(input: &Value) -> Result<Option<Vec<Value>>, String> {
    let items = match json_value(input, "pipeline")? {
        None => return Ok(None),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(format!("`pipeline` is an array of stages. {STAGE_HINT}")),
    };
    let mut stages = Vec::with_capacity(items.len());
    for (index, item) in items.into_iter().enumerate() {
        let number = index + 1;
        let stage = match item {
            Value::String(text) if text.trim().is_empty() => continue,
            Value::String(text) => serde_json::from_str::<Value>(text.trim())
                .map_err(|error| format!("Stage {number} is not Extended JSON ({error}). {STAGE_HINT}"))?,
            other => other,
        };
        match &stage {
            Value::Object(map) if map.is_empty() => {
                return Err(format!("Stage {number} arrived empty ({{}}): its operator was lost. {STAGE_HINT}"));
            }
            Value::Object(map) if map.len() == 1 && map.keys().all(|key| key.starts_with('$')) => stages.push(stage),
            _ => {
                return Err(format!(
                    "Stage {number} must be a document with one stage operator, such as {{\"$match\": {{…}}}}. {STAGE_HINT}"
                ))
            }
        }
    }
    Ok((!stages.is_empty()).then_some(stages))
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
    let database = pinned_database(input, context, SAMPLE_DOCUMENTS)?;
    let collection = collection(input, context)?;
    if let Some(refused) = restricted_namespace(&database, &collection) {
        return Err(refused);
    }
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_SAMPLE)
        .clamp(1, MAX_SAMPLE);
    let filter = json_value(input, "filter")?.filter(|filter| filter.as_object().is_none_or(|map| !map.is_empty()));
    if filter.as_ref().is_some_and(|filter| !filter.is_object()) {
        return Err(String::from("`filter` is a document"));
    }
    let stages = pipeline_stages(input)?;
    if let Some(stages) = &stages {
        stages.iter().try_for_each(|stage| refuse_json(stage, &database))?;
    }
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
        // Server errors can quote values (a duplicate key, a failed conversion): scrubbed like row text.
        .map_err(|error| redact::scrub_value(&error.to_string()))
        .and_then(|result| documents_of(operation, &result).map(|(documents, more)| (documents, more, result)));
    let elapsed_ms = outcome
        .as_ref()
        .ok()
        .and_then(|(_, _, result)| result.get("elapsedMs").and_then(Value::as_u64))
        .unwrap_or_default();
    let (documents, more) = match &outcome {
        // Redacted before they are cut (a cut could hide a secret from the checks), and the chat shows what the model gets.
        Ok((documents, more, _)) => (
            documents
                .iter()
                .take(limit as usize)
                .map(|document| cut(&redact::redact_json(document)))
                .collect::<Vec<_>>(),
            *more,
        ),
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
    text.push_str(", credentials redacted, long values cut:");
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
                // A collection the engine rates as administration (as MongoDB does credentials or the oplog).
                ClassifyInput::Capability { args, .. } if args["collection"] == "audited" => {
                    Classification::single(StatementInfo::new(Category::Admin, "x", "x"))
                }
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
            let document = |n: u64| {
                json!({
                    "_id": n,
                    "note": "x".repeat(5000),
                    "tags": (0..30).collect::<Vec<_>>(),
                    "contact": { "email": format!("user{n}@example.com"), "password": format!("secret-{n}") },
                    "logins": [{ "token": format!("tok-{n}"), "via": "key sk-abcdefghijklmnopqrstuvwx1234" }],
                })
            };
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

    #[test]
    fn opening_a_workspace_is_pinned_and_refuses_internals_and_writes() {
        let (_, _, context) = setup();
        let quiet = |_: ChatEvent| {};
        let refused = |context: &ChatContext, input: Value| open_workspace(context, &input, &quiet).unwrap_err();
        // Another database (the chat's is shop), system collections and server databases.
        assert!(refused(
            &context,
            json!({ "section": "documents", "database": "admin", "collection": "system.users" })
        )
        .contains("current database `shop`"));
        assert!(refused(&context, json!({ "section": "documents", "database": "billing" })).contains("current database `shop`"));
        assert!(refused(&context, json!({ "section": "documents", "collection": "system.profile" })).contains("system collection"));
        for database in ["admin", "local", "config", "Admin"] {
            let inside: ChatContext =
                serde_json::from_value(json!({ "connectionId": "m", "database": database, "table": "anything" })).unwrap();
            assert!(
                refused(&inside, json!({ "section": "documents" })).contains("server internals"),
                "{database}"
            );
        }
        let nowhere: ChatContext = serde_json::from_value(json!({ "connectionId": "m", "table": "orders" })).unwrap();
        assert!(refused(&nowhere, json!({ "section": "documents", "database": "shop" })).contains("no database is selected"));
        // Stages that write, as text or JSON, and stages naming a database.
        for pipeline in [
            json!(["{ $match: {} }", "{ $out: 'orders' }"]),
            json!(["{ \"$merge\": { \"into\": \"totals\" } }"]),
            json!([{ "$out": "orders" }]),
            json!(["{ $lookup: { from: { db: 'billing', coll: 'cards' }, localField: 'c', foreignField: '_id', as: 'c' } }"]),
            json!(["{ $unionWith: { \"db\": \"shop\", \"coll\": \"x\" } }"]),
            json!([{ "$unionWith": { "db": "billing", "coll": "cards" } }]),
        ] {
            let why = refused(&context, json!({ "section": "aggregations", "pipeline": pipeline }));
            assert!(why.starts_with("Refused:"), "{pipeline}: {why}");
        }
        // Field names that only look like them are fine.
        let fine = json!({ "section": "aggregations", "pipeline": ["{ $project: { outcome: '$outcome', db_name: 1, mergeKey: '$merged' } }", "{ $group: { _id: '$db' } }"] });
        assert!(open_workspace(&context, &fine, &quiet).is_ok());
        assert!(open_workspace(&context, &json!({ "section": "documents", "filter": "{ db: 'x' }" }), &quiet).is_ok());
    }

    #[test]
    fn suggestion_text_checks() {
        assert!(mentions("{ $out: 'x' }", "$out") && !mentions("'$outcome'", "$out"));
        assert!(has_key("{ db: 'a', coll: 'b' }", "db") && has_key("{\"x\": 1, \"db\" : 2}", "db"));
        assert!(!has_key("{ admin_db: 1 }", "db") && !has_key("{ a: 'db' }", "db"));
        assert!(refuse_json(&json!({ "$lookup": { "from": { "db": "shop", "coll": "x" } } }), "shop").is_ok());
        assert!(refuse_json(
            &json!({ "$facet": { "a": [{ "$lookup": { "from": { "db": "other", "coll": "x" } } }] } }),
            "shop"
        )
        .is_err());
        assert!(restricted_namespace("shop", "orders").is_none());
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
        for input in [
            json!({ "collection": "system.users" }),
            json!({ "pipeline": [{ "$unionWith": { "db": "admin", "coll": "system.users" } }] }),
            json!({ "pipeline": [{ "$lookup": { "from": { "db": "billing", "coll": "cards" }, "localField": "a", "foreignField": "b", "as": "c" } }] }),
            json!({ "pipeline": [{ "$match": {} }, { "$out": "orders" }] }),
            json!({ "pipeline": "[{ \"$merge\": { \"into\": \"orders\" } }]" }),
        ] {
            let refused = sample(&db, &context, Some(&allowed), &input, &quiet).await.unwrap_err();
            assert!(refused.starts_with("Refused:"), "{input}: {refused}");
        }
        let inside: ChatContext = serde_json::from_value(json!({ "connectionId": "m", "database": "local", "table": "oplog.rs" })).unwrap();
        assert!(sample(&db, &inside, Some(&allowed), &json!({}), &quiet)
            .await
            .unwrap_err()
            .contains("server internals"));
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
        let columns: Vec<&str> = shown[0]["columns"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        for column in ["_id", "note", "tags", "contact", "logins"] {
            assert!(columns.contains(&column), "{columns:?}");
        }
        assert_eq!(shown[0]["rows"].as_array().unwrap().len(), 20);
        // Credentials never reach the model or the chat, at any depth; contact data does, and keys stay.
        let rows = shown[0]["rows"].to_string();
        for leaked in ["secret-", "tok-", "sk-abc"] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
            assert!(!rows.contains(leaked), "{leaked} in {rows}");
        }
        assert!(
            text.contains(r#""password":"[redacted]""#) && text.contains(r#""token":"[redacted]""#),
            "{text}"
        );
        assert!(text.contains("user0@example.com") && rows.contains("[redacted]"), "{rows}");

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

    /// Every `object` node of a schema declares its properties: a property-less object reaches strict-schema providers
    /// (OpenAI-compatible gateways) as `{}`, which is how pipeline stages lost their operators.
    fn assert_no_open_objects(schema: &Value, path: &str) {
        match schema {
            Value::Object(map) => {
                if map.get("type") == Some(&json!("object")) {
                    assert!(
                        map.get("properties").is_some_and(Value::is_object),
                        "{path}: object without properties"
                    );
                    assert_eq!(map.get("additionalProperties"), Some(&json!(false)), "{path}");
                }
                for (key, inner) in map {
                    assert_no_open_objects(inner, &format!("{path}.{key}"));
                }
            }
            Value::Array(items) => items.iter().for_each(|inner| assert_no_open_objects(inner, path)),
            _ => {}
        }
    }

    #[test]
    fn tool_schemas_keep_documents_as_text_for_every_provider() {
        let tools = super::super::tools::tool_definitions(true, true, true);
        for tool in &tools {
            assert_no_open_objects(&tool.schema, tool.name);
        }
        let sample = tools.iter().find(|tool| tool.name == SAMPLE_DOCUMENTS).expect("offered with reads");
        assert_eq!(sample.schema["properties"]["pipeline"]["items"], json!({ "type": "string" }));
        assert_eq!(sample.schema["properties"]["filter"]["type"], "string");
        let open = tools.iter().find(|tool| tool.name == OPEN_WORKSPACE).unwrap();
        assert_eq!(open.schema["properties"]["pipeline"]["items"], json!({ "type": "string" }));
        // What the adapters send is the schema as declared (the Fosta and OpenAI-compatible providers use the OpenAI one).
        let openai = super::super::openai::tools_body(&tools);
        let sent = openai
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["function"]["name"] == SAMPLE_DOCUMENTS)
            .unwrap();
        assert_eq!(sent["function"]["parameters"], sample.schema);
        assert!(sent["function"].get("strict").is_none());
        let anthropic = super::super::anthropic::tools_body(&tools);
        let sent = anthropic
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == SAMPLE_DOCUMENTS)
            .unwrap();
        assert_eq!(sent["input_schema"], sample.schema);
    }

    #[test]
    fn pipeline_stages_parse_from_text_and_refuse_lost_operators() {
        let stages = |pipeline: Value| pipeline_stages(&json!({ "pipeline": pipeline }));
        // As the schema asks: one Extended JSON string per stage.
        let parsed = stages(json!([
            "{\"$match\": {\"status\": \"paid\"}}",
            " {\"$group\": {\"_id\": \"$status\", \"n\": {\"$sum\": 1}}} ",
            ""
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(parsed[1], json!({ "$group": { "_id": "$status", "n": { "$sum": 1 } } }));
        assert_eq!(parsed.len(), 2);
        // Documents (providers that keep them) and the whole pipeline as one text still work.
        assert_eq!(stages(json!([{ "$limit": 5 }])).unwrap().unwrap(), vec![json!({ "$limit": 5 })]);
        assert_eq!(
            stages(json!("[{\"$count\": \"n\"}]")).unwrap().unwrap(),
            vec![json!({ "$count": "n" })]
        );
        assert_eq!(stages(json!([])).unwrap(), None);
        assert_eq!(pipeline_stages(&json!({})).unwrap(), None);
        // What a strict-schema provider made of object stages: refused with a hint, before the gateway.
        let empty = stages(json!([{}, {}])).unwrap_err();
        assert!(
            empty.contains("Stage 1 arrived empty") && empty.contains("mongo-pipeline"),
            "{empty}"
        );
        let shell = stages(json!(["{ $group: { _id: '$status' } }"])).unwrap_err();
        assert!(
            shell.contains("Stage 1 is not Extended JSON") && shell.contains("double-quoted"),
            "{shell}"
        );
        for bad in [
            json!(["{\"$match\": {}, \"$limit\": 1}"]),
            json!(["{\"status\": 1}"]),
            json!(["[1]"]),
            json!(7),
        ] {
            assert!(stages(bad.clone()).is_err(), "{bad}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pipeline_given_as_text_reaches_the_engine_with_its_operators() {
        let (db, engine, context) = setup();
        let quiet = |_: ChatEvent| {};
        let input = json!({
            "pipeline": ["{\"$match\": {\"status\": \"paid\"}}", "{\"$group\": {\"_id\": \"$status\", \"n\": {\"$sum\": 1}}}"],
            "filter": "{\"total\": {\"$gt\": 5}}"
        });
        sample(&db, &context, Some(&reads()), &input, &quiet).await.unwrap();
        let (name, args) = engine.calls.lock().unwrap()[0].clone();
        assert_eq!(name, "mongo.aggregatePreview");
        assert_eq!(
            args["pipeline"],
            json!([
                { "$match": { "total": { "$gt": 5 } } },
                { "$match": { "status": "paid" } },
                { "$group": { "_id": "$status", "n": { "$sum": 1 } } }
            ])
        );
        // Stage text that writes is refused like a document.
        let writes = json!({ "pipeline": ["{\"$out\": \"orders\"}"] });
        let refused = sample(&db, &context, Some(&reads()), &writes, &quiet).await.unwrap_err();
        assert!(refused.starts_with("Refused:"), "{refused}");
        assert_eq!(engine.calls.lock().unwrap().len(), 1);
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

    #[tokio::test]
    async fn sampled_documents_reach_the_prompt_only_from_a_readable_collection() {
        let (db, _, context) = setup();
        let with = |database: &str, table: &str| ChatContext {
            database: Some(database.into()),
            table: Some(table.into()),
            sampled_documents: Some("{\"email\": \"ann@example.com\"}".into()),
            ..context.clone()
        };
        let prompt = |context: ChatContext, reads: bool| {
            let db = &db;
            async move { super::super::prompt::system_prompt(db, &context, reads).await }
        };
        let allowed = prompt(with("shop", "orders"), true).await;
        assert!(allowed.contains("Sampled documents (values cut):\n{\"email\""), "{allowed}");
        // Not without the read setting, not from system collections or server databases, not from what the engine
        // rates as administration.
        assert!(!prompt(with("shop", "orders"), false).await.contains("ann@example.com"));
        for (database, table) in [
            ("admin", "system.users"),
            ("shop", "system.profile"),
            ("local", "oplog.rs"),
            ("config", "x"),
            ("shop", "audited"),
        ] {
            assert!(
                !prompt(with(database, table), true).await.contains("ann@example.com"),
                "{database}.{table}"
            );
        }
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
