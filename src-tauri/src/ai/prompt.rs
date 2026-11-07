//! The system prompt: fixed rules plus what the UI shares about where the user is.

use super::ChatContext;
use crate::drivers::DbManager;

const MAX_TABLES_IN_PROMPT: usize = 400;
const MAX_EDITOR_CHARS: usize = 6000;

pub(super) async fn system_prompt(db: &DbManager, context: &ChatContext) -> String {
    let mut text = String::from(
        "You are the assistant built into DB Studio, a desktop database client. You help the user write and understand queries for the database they have open.\n\n\
Rules:\n\
- You cannot run anything. The user runs queries themselves from the editor, so put every runnable query in its own fenced code block (```sql for SQL engines, ```json for MongoDB commands and pipelines) so it can be inserted as-is. Independent statements go in separate blocks.\n\
- Before writing a query against a table, look at its columns with describe_table unless the structure is already known from this conversation. Never guess column names. For joins across several tables, read schema_overview once instead of describing each table.\n\
- You can drive the app: open_diagram to show structure or relationships, open_table to show rows (with a filter), open_query to put a longer script in its own tab. These only open views; nothing runs. Mention what you opened in one short line.\n\
- Use the dialect of the connected engine and point out engine-specific syntax when it matters (LIMIT vs TOP / OFFSET FETCH, backticks vs brackets, date functions).\n\
- Write statements that change data or schema (UPDATE, DELETE, DROP, ALTER, TRUNCATE) only when asked. Say what they affect, keep a WHERE on the key, and suggest a SELECT to check first.\n\
- Answer in the language the user writes in. Keep prose short; the query is the answer. Do not repeat the user's question.\n\
- For MongoDB, write what the query tab runs: a database command object, e.g. { \"find\": \"orders\", \"filter\": {...}, \"limit\": 50 } or { \"aggregate\": \"orders\", \"pipeline\": [...], \"cursor\": {} }.\n\
- When asked for test data, produce INSERT statements (or insertMany commands) with realistic, varied values that respect the column types and keys.\n\
- When asked to translate between engines, keep the semantics and mention anything that has no direct equivalent.\n\n\
Context:\n",
    );
    match (&context.connection_id, &context.connection_name) {
        (Some(_), name) => {
            let name = name.as_deref().unwrap_or("connection");
            let engine = match (&context.driver_label, &context.server_version) {
                (Some(label), Some(version)) => format!("{label} {version}"),
                (Some(label), None) => label.clone(),
                _ => String::from("unknown engine"),
            };
            text.push_str(&format!("- Connection: {name} ({engine})\n"));
        }
        _ => text.push_str("- No connection is open. Write generic SQL and say which engine you assumed.\n"),
    }
    if let Some(database) = &context.database {
        text.push_str(&format!("- Database: {database}\n"));
        if let Some(listing) = table_listing(db, context, database).await {
            text.push_str(&listing);
        }
    }
    if let Some(table) = &context.table {
        text.push_str(&format!("- Table the user is looking at: {table}\n"));
    }
    if let Some(editor) = context.editor_text.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        text.push_str(&format!(
            "- Current editor text:\n```\n{}\n```\n",
            truncate(editor, MAX_EDITOR_CHARS)
        ));
    }
    if let Some(error) = context.last_error.as_deref().filter(|value| !value.trim().is_empty()) {
        text.push_str(&format!("- Last error from the server: {}\n", truncate(error, 1000)));
    }
    if let Some(extra) = context.extra.as_deref().filter(|value| !value.trim().is_empty()) {
        text.push_str(&format!("- {}\n", truncate(extra, MAX_EDITOR_CHARS)));
    }
    text
}

async fn table_listing(db: &DbManager, context: &ChatContext, database: &str) -> Option<String> {
    let driver = db.driver(context.connection_id.as_ref()?).ok()?;
    let tables = driver.list_tables(database).await.ok()?;
    if tables.is_empty() {
        return Some(format!("- {database} has no tables yet.\n"));
    }
    let shown: Vec<String> = tables
        .iter()
        .take(MAX_TABLES_IN_PROMPT)
        .map(|table| match table.rows {
            Some(rows) => format!("{} ({}, ~{rows} rows)", table.name, table.kind),
            None => format!("{} ({})", table.name, table.kind),
        })
        .collect();
    let more = if tables.len() > shown.len() {
        format!(" … and {} more", tables.len() - shown.len())
    } else {
        String::new()
    };
    Some(format!("- Tables in {database}: {}{more}\n", shown.join(", ")))
}

pub(super) fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}\n… (truncated)")
}
