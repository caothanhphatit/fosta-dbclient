//! Editor support: the schema the completion needs, and SQL formatting. Both are placeholders for the language service
//! work (`src-tauri/src/lang`); their signatures are fixed.

use serde::Serialize;

use crate::{contract::SqlDialectId, error::AppResult, AppState};

/// Names the editor completes, per database.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionSchema {
    pub database: String,
    /// Objects (tables, views, collections) with their column names.
    pub objects: Vec<CompletionObject>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionObject {
    pub name: String,
    pub schema: Option<String>,
    pub kind: String,
    pub columns: Vec<String>,
}

/// Empty until the language service fills it from `tree_children` and `describe_object`.
pub async fn completion_schema(_state: &AppState, _connection_id: String, database: String) -> AppResult<CompletionSchema> {
    Ok(CompletionSchema {
        database,
        objects: Vec::new(),
    })
}

/// Returns the text unchanged until a formatter is wired.
pub fn format_sql(_state: &AppState, dialect: Option<SqlDialectId>, text: String) -> AppResult<String> {
    Ok(crate::lang::format_sql(dialect, &text))
}
