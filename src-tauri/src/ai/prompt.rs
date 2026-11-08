//! The system prompt: fixed rules plus what the UI shares about where the user is.

use super::{catalog, ChatContext};
use crate::{
    contract::{CallContext, EngineDescriptor},
    runtime::ConnectionManager,
};

const MAX_TABLES_IN_PROMPT: usize = 400;
const MAX_EDITOR_CHARS: usize = 6000;

const INTRO: &str = "You are the assistant built into Fosta Studio, a desktop database client. You help the user write and \
understand queries for the database they have open.\n\nRules:\n";

const RULES: &str = "\
- Before writing a query against a table, look at its columns with describe_table unless the structure is already known from this conversation. Never guess column names. For joins across several tables, read schema_overview once instead of describing each table.\n\
- You can drive the app: open_diagram to show structure or relationships, open_table to show rows (with a filter), open_query to put a longer script in its own tab. These only open views; nothing runs. Mention what you opened in one short line.\n\
- Use the dialect of the connected engine and point out engine-specific syntax when it matters (LIMIT vs TOP / OFFSET FETCH, backticks vs brackets, date functions).\n\
- Write statements that change data or schema (UPDATE, DELETE, DROP, ALTER, TRUNCATE) only when asked. Say what they affect, keep a WHERE on the key, and suggest a SELECT to check first.\n\
- Answer in the language the user writes in. Keep prose short; the query is the answer. Do not repeat the user's question.\n\
- For MongoDB, write what the query tab runs: a database command object, e.g. { \"find\": \"orders\", \"filter\": {...}, \"limit\": 50 } or { \"aggregate\": \"orders\", \"pipeline\": [...], \"cursor\": {} }.\n\
- When asked for test data, produce INSERT statements (or insertMany commands) with realistic, varied values that respect the column types and keys.\n\
- When asked to translate between engines, keep the semantics and mention anything that has no direct equivalent.\n\n\
Context:\n";

/// The system prompt. `read_queries`: the model has `run_read_query` (the user allows it for this workspace).
pub(super) async fn system_prompt(db: &ConnectionManager, context: &ChatContext, read_queries: bool) -> String {
    let descriptor = context
        .connection_id
        .as_deref()
        .and_then(|id| db.handle(id).ok())
        .map(|handle| handle.engine.descriptor().clone());
    let mut text = String::from(INTRO);
    text.push_str(&format!("- {}\n", fence_rule(descriptor.as_ref())));
    text.push_str(if read_queries {
        "- You can run one read-only statement at a time with run_read_query to look at real data (at most 50 rows, long \
         values cut). The rows are sent to you and to the AI provider, so read only what you need. Anything that writes, \
         changes the schema or administers the server is refused: put it in a code block, and the user runs it with the Run button.\n"
    } else {
        "- You cannot run anything yourself. The user runs the code blocks you write (Run, Insert or New tab under each block).\n"
    });
    text.push_str(RULES);
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

/// How to fence runnable code, from the connected engine's languages (each with the tag the chat recognizes).
fn fence_rule(descriptor: Option<&EngineDescriptor>) -> String {
    let tags: Vec<String> = descriptor
        .map(|descriptor| {
            descriptor
                .languages
                .iter()
                .filter(|language| !language.fence().is_empty())
                .map(|language| format!("```{} for {}", language.fence(), language.label))
                .collect()
        })
        .unwrap_or_default();
    let how = if tags.is_empty() {
        String::from("```sql for SQL, ```json for MongoDB commands and pipelines")
    } else {
        tags.join(", ")
    };
    format!(
        "Put every runnable query in its own fenced code block ({how}) so it can be run, inserted or opened as-is. \
         Independent statements go in separate blocks."
    )
}

async fn table_listing(db: &ConnectionManager, context: &ChatContext, database: &str) -> Option<String> {
    let handle = db.handle(context.connection_id.as_ref()?).ok()?;
    let listing = catalog::objects(&handle, &CallContext::default(), database).await.ok()?;
    let objects = &listing.objects;
    if objects.is_empty() {
        return Some(format!("- {database} has no tables yet.\n"));
    }
    let shown: Vec<String> = objects
        .iter()
        .take(MAX_TABLES_IN_PROMPT)
        .map(|object| match &object.detail {
            Some(detail) => format!("{} ({}, {detail})", object.label, object.kind),
            None => format!("{} ({})", object.label, object.kind),
        })
        .collect();
    let more = if objects.len() > shown.len() {
        format!(" … and {} more", objects.len() - shown.len())
    } else if let Some(note) = &listing.note {
        format!(" ({note})")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::testing::fake;

    #[test]
    fn the_fence_rule_comes_from_the_engine_languages() {
        let rule = fence_rule(Some(&fake::descriptor()));
        assert!(rule.contains("```sql for SQL"), "{rule}");
        assert!(fence_rule(None).contains("```json for MongoDB"));
    }

    #[tokio::test]
    async fn the_prompt_says_whether_the_assistant_can_run_reads() {
        let db = ConnectionManager::default();
        let context = ChatContext::default();
        assert!(system_prompt(&db, &context, false)
            .await
            .contains("You cannot run anything yourself"));
        let allowed = system_prompt(&db, &context, true).await;
        assert!(allowed.contains("run_read_query") && allowed.contains("AI provider"), "{allowed}");
    }
}
