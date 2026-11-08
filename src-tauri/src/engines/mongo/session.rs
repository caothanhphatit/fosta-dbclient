//! Query-tab sessions in the `mongo-command` language: the text is one database command in relaxed Extended JSON
//! (`{ "find": … }`, `{ "aggregate": … }`, `$oid` / `$date` / `$numberDecimal` allowed), run against the request's
//! database.
//!
//! Cursor replies (`find`, `aggregate`, `listCollections` …) are read with `getMore` in the session that opened the
//! cursor until the row limit, then the cursor is killed; the documents become rows over the union of their top-level
//! keys (nested values stay documents and arrays). Other commands return their reply as one row. A reply with
//! `writeErrors` or a `writeConcernError` is an error (`ENGINE_ERROR` with the first error's code). Sessions have no
//! transactions and no Stop.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use mongodb::{
    bson::{doc, Bson, Document},
    Client, ClientSession, Database,
};
use serde_json::json;

use super::values;
use crate::{
    contract::{Batcher, ResultSink, RunOptions, RunRequest, Session, StreamEvent},
    engines::common::sqlclassify::excerpt,
    error::{AppError, AppResult},
};

pub const LANGUAGE: &str = "mongo-command";
/// Commands that read a cursor and accept `maxTimeMS`; the statement timeout is sent with them.
const TIMED_COMMANDS: [&str; 4] = ["find", "aggregate", "count", "distinct"];
/// MongoDB's own code for a command it cannot parse.
const FAILED_TO_PARSE: i32 = 9;

pub struct MongoSession {
    client: Client,
    database: Option<String>,
}

impl MongoSession {
    pub fn new(client: Client, database: Option<String>) -> Self {
        Self { client, database }
    }
}

/// Parses the query tab's text into a command; a malformed command is reported like the server's `FailedToParse`.
pub fn parse_command(text: &str) -> AppResult<Document> {
    let command = values::parse_document(text).map_err(|error| {
        AppError::coded(
            "ENGINE_ERROR",
            format!(
                "MongoDB queries are database commands written as JSON, for example \
                 {{ \"find\": \"products\", \"filter\": {{ \"stock\": 0 }}, \"limit\": 50 }}. ({error})"
            ),
        )
        .with_vendor_code(FAILED_TO_PARSE)
        .with("codeName", "FailedToParse")
    })?;
    if command.is_empty() {
        return Err(
            AppError::coded("ENGINE_ERROR", "The command is empty: its first key names the command.")
                .with_vendor_code(FAILED_TO_PARSE)
                .with("codeName", "FailedToParse"),
        );
    }
    Ok(command)
}

/// Write errors in a command reply (`ok: 1` with `writeErrors` or `writeConcernError`) as an error: the first write
/// error's code as the vendor code, its `codeName` and `errInfo`, and how many documents were written anyway.
pub fn reply_error(reply: &Document) -> Option<AppError> {
    let applied = reply.get("n").and_then(|n| match n {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        _ => None,
    });
    let (error, more) = match reply.get_array("writeErrors") {
        Ok(errors) if !errors.is_empty() => (errors.first().and_then(Bson::as_document)?, errors.len() - 1),
        _ => (reply.get_document("writeConcernError").ok()?, 0),
    };
    let code = match error.get("code") {
        Some(Bson::Int32(code)) => i64::from(*code),
        Some(Bson::Int64(code)) => *code,
        _ => 0,
    };
    let name = error.get_str("codeName").unwrap_or_default();
    let message = error.get_str("errmsg").unwrap_or("The write failed");
    let mut text = if name.is_empty() {
        message.to_owned()
    } else {
        format!("{message} ({name})")
    };
    if more > 0 {
        text.push_str(&format!(" and {more} more write error(s)"));
    }
    let mut out = AppError::coded("ENGINE_ERROR", text).with_vendor_code(code).with("codeName", name);
    if let Ok(info) = error.get_document("errInfo") {
        out = out.with("errInfo", Bson::Document(info.clone()).into_relaxed_extjson());
    }
    if let Some(applied) = applied {
        out = out.with("applied", json!(applied));
    }
    Some(out)
}

fn cursor_id(cursor: &Document) -> i64 {
    match cursor.get("id") {
        Some(Bson::Int64(id)) => *id,
        Some(Bson::Int32(id)) => i64::from(*id),
        _ => 0,
    }
}

/// Reads a command cursor to its end (the command's own `limit` ends it) or past `max_rows`, with getMore after the
/// first batch, and kills it when documents are left over. Returns the documents and whether some were cut.
pub async fn drain_cursor(
    db: &Database,
    session: &mut ClientSession,
    cursor: &Document,
    max_rows: usize,
    timeout: Option<Duration>,
) -> AppResult<(Vec<Document>, bool)> {
    let first = cursor.get_array("firstBatch").map_err(|error| AppError::new(error.to_string()))?;
    let mut documents: Vec<Document> = first.iter().filter_map(|item| item.as_document().cloned()).collect();
    let mut id = cursor_id(cursor);
    let collection = cursor
        .get_str("ns")
        .ok()
        .and_then(|ns| ns.split_once('.'))
        .map(|(_, name)| name.to_owned())
        .unwrap_or_default();
    while id != 0 && documents.len() <= max_rows && !collection.is_empty() {
        let wanted = i64::try_from(max_rows + 1 - documents.len()).unwrap_or(i64::MAX);
        let mut get_more = doc! { "getMore": id, "collection": collection.as_str(), "batchSize": wanted };
        if let Some(timeout) = timeout {
            get_more.insert("maxTimeMS", i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX));
        }
        let reply = db.run_command(get_more).session(&mut *session).await?;
        let next = reply.get_document("cursor").map_err(|error| AppError::new(error.to_string()))?;
        if let Ok(batch) = next.get_array("nextBatch") {
            documents.extend(batch.iter().filter_map(|item| item.as_document().cloned()));
        }
        id = cursor_id(next);
    }
    if id != 0 && !collection.is_empty() {
        // Release the server-side cursor instead of leaving it to time out.
        let _ = db
            .run_command(doc! { "killCursors": collection.as_str(), "cursors": [id] })
            .session(&mut *session)
            .await;
    }
    let truncated = documents.len() > max_rows || id != 0;
    documents.truncate(max_rows);
    Ok((documents, truncated))
}

/// Streams documents as one result set: columns over the union of their keys, rows in batches.
fn emit(documents: &[Document], truncated: bool, started: Instant, options: &RunOptions, sink: &mut dyn ResultSink) {
    let columns = values::columns_of(documents);
    sink.event(StreamEvent::Columns {
        index: 0,
        columns: columns.clone(),
    });
    let mut batcher = Batcher::new(
        0,
        &RunOptions {
            row_limit: usize::MAX,
            ..options.clone()
        },
    );
    for row in values::rows_of(documents, &columns) {
        if !batcher.push(row, sink) {
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

#[async_trait]
impl Session for MongoSession {
    fn language(&self) -> &str {
        LANGUAGE
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        let database = request
            .database
            .clone()
            .or_else(|| self.database.clone())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| AppError::new("Pick a database first: MongoDB commands run against one database."))?;
        let mut command = parse_command(&request.text)?;
        let name = command.keys().next().cloned().unwrap_or_default();
        if let Some(timeout) = options.timeout {
            if TIMED_COMMANDS.contains(&name.as_str()) && !command.contains_key("maxTimeMS") {
                command.insert("maxTimeMS", i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX));
            }
        }
        sink.event(StreamEvent::Statement {
            index: 0,
            sql: excerpt(&request.text),
        });
        let started = Instant::now();
        let db = self.client.database(&database);
        // getMore must run in the session that created the cursor, so the command gets an explicit one.
        let mut session = self.client.start_session().await?;
        let reply = db.run_command(command).session(&mut session).await?;
        if let Some(error) = reply_error(&reply) {
            return Err(error);
        }
        match reply
            .get_document("cursor")
            .ok()
            .filter(|cursor| cursor.get_array("firstBatch").is_ok())
        {
            Some(cursor) => {
                let (documents, truncated) = drain_cursor(&db, &mut session, cursor, options.row_limit, options.timeout).await?;
                emit(&documents, truncated, started, options, sink);
            }
            None => emit(std::slice::from_ref(&reply), false, started, options, sink),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_commands_are_failed_to_parse_errors() {
        let error = parse_command(r#"{ "find": "#).unwrap_err();
        assert_eq!(error.code(), Some("ENGINE_ERROR"));
        assert_eq!(error.field("vendorCode"), Some(&json!(9)));
        assert_eq!(error.field("codeName"), Some(&json!("FailedToParse")));
        assert!(error.message().contains("database commands written as JSON"));
        assert!(parse_command("{}").is_err());
        assert!(parse_command("[]").is_err());
        let command = parse_command(r#"{ "insert": "c", "documents": [ { "_id": { "$oid": "65f0c0ffee0000000000abcd" } } ] }"#).unwrap();
        assert_eq!(command.keys().next().map(String::as_str), Some("insert"));
    }

    #[test]
    fn write_errors_in_replies_become_engine_errors() {
        let reply = doc! {
            "n": 0, "ok": 1.0,
            "writeErrors": [ { "index": 0, "code": 11000, "codeName": "DuplicateKey", "errmsg": "E11000 duplicate key error", "keyValue": { "_id": 1 } } ],
        };
        let error = reply_error(&reply).expect("an error");
        assert_eq!(error.code(), Some("ENGINE_ERROR"));
        assert_eq!(error.field("vendorCode"), Some(&json!(11000)));
        assert_eq!(error.field("codeName"), Some(&json!("DuplicateKey")));
        assert_eq!(error.field("applied"), Some(&json!(0)));
        assert!(error.message().starts_with("E11000 duplicate key error (DuplicateKey)"));
        let concern = doc! { "n": 1, "ok": 1, "writeConcernError": { "code": 64, "codeName": "WriteConcernFailed", "errmsg": "waiting for replication timed out", "errInfo": { "wtimeout": true } } };
        let error = reply_error(&concern).unwrap();
        assert_eq!(error.field("vendorCode"), Some(&json!(64)));
        assert_eq!(error.field("errInfo"), Some(&json!({ "wtimeout": true })));
        assert!(reply_error(&doc! { "n": 1, "ok": 1 }).is_none());
    }
}
