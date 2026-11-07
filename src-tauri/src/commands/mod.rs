//! Commands: the whole backend surface the UI calls through `src/core/api.ts`.
//! Command names and argument names are part of that contract; keep them stable.
//!
//! The bodies are plain functions taking the [`AppState`]; the two front-ends expose them: the desktop app
//! as Tauri commands (`main.rs`) and the web server as `POST /api/invoke/<command>` (`server/`). Both are
//! generated from [`for_each_command!`](crate::for_each_command), so a command added there reaches both.

pub mod ai;
pub mod connections;
pub mod data;
pub mod files;
pub mod history;
pub mod tasks;
pub mod vector;
pub mod workspaces;

use serde::Serialize;
use serde_json::Value;

use crate::{
    config::AppConfig,
    error::{AppError, AppResult},
    tasks::TaskStatus,
    AppState, Capabilities,
};

pub fn get_config(state: &AppState) -> AppConfig {
    state.config.snapshot()
}

/// What this front-end supports (startup tasks, "Show in folder", where downloads go); the UI hides the rest.
pub fn capabilities(state: &AppState) -> Capabilities {
    state.capabilities.clone()
}

/// Calls `$callback!` with every command both front-ends share, one entry per command:
/// `[sync|async] name(argument: Type, …) -> ReturnType = path::to::function;`.
/// Each function takes `&AppState` first, then the listed arguments, whose camelCase names are the JSON keys.
///
/// Not listed (each front-end handles them itself): `ai_chat` (it streams events), and the desktop-only
/// `save_download` / `reveal_file` (the web edition downloads through the browser).
#[macro_export]
macro_rules! for_each_command {
    ($callback:ident) => {
        $callback! {
            [sync] get_config() -> $crate::config::AppConfig = $crate::commands::get_config;
            [sync] capabilities() -> $crate::Capabilities = $crate::commands::capabilities;

            [sync] save_connection(profile: $crate::config::ConnectionProfile, password: Option<String>, workspace_id: Option<String>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::connections::save_connection;
            [async] delete_connection(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::connections::delete_connection;
            [async] test_connection(profile: $crate::config::ConnectionProfile, password: Option<String>)
                -> $crate::error::AppResult<String> = $crate::commands::connections::test_connection;
            [async] connect(id: String, password: Option<String>) -> $crate::error::AppResult<String> = $crate::commands::connections::connect;
            [async] disconnect(id: String) -> $crate::error::AppResult<()> = $crate::commands::connections::disconnect;

            [async] list_databases(connection_id: String) -> $crate::error::AppResult<Vec<String>> = $crate::commands::data::list_databases;
            [async] create_database(connection_id: String, name: String, first_collection: Option<String>,
                confirmation: Option<$crate::gateway::Confirmation>)
                -> $crate::error::AppResult<()> = $crate::commands::data::create_database;
            [async] schema_graph(connection_id: String, database: String)
                -> $crate::error::AppResult<$crate::schema::SchemaGraph> = $crate::commands::data::schema_graph;
            [async] list_tables(connection_id: String, database: String)
                -> $crate::error::AppResult<Vec<$crate::drivers::types::TableInfo>> = $crate::commands::data::list_tables;
            [async] describe_table(connection_id: String, database: String, table: String)
                -> $crate::error::AppResult<$crate::drivers::types::TableDesign> = $crate::commands::data::describe_table;
            [async] browse_table(request: $crate::drivers::types::BrowseRequest)
                -> $crate::error::AppResult<$crate::drivers::types::TablePage> = $crate::commands::data::browse_table;
            [async] apply_changes(connection_id: String, database: String, table: String, changes: Vec<$crate::drivers::types::RowChange>,
                caller: Option<$crate::gateway::Caller>, import_id: Option<String>, confirmation: Option<$crate::gateway::Confirmation>)
                -> $crate::error::AppResult<u64> = $crate::commands::data::apply_changes;
            [async] table_relations(connection_id: String, database: String, table: String)
                -> $crate::error::AppResult<$crate::drivers::relations::TableRelations> = $crate::commands::data::table_relations;
            [async] open_session(connection_id: String, session_id: String) -> $crate::error::AppResult<()> = $crate::commands::data::open_session;
            [sync] close_session(session_id: String) -> () = $crate::commands::data::close_session;
            [async] run_sql(session_id: String, database: Option<String>, sql: String, confirmation: Option<$crate::gateway::Confirmation>)
                -> $crate::error::AppResult<$crate::drivers::types::QueryOutcome> = $crate::commands::data::run_sql;
            [async] cancel_sql(session_id: String) -> $crate::error::AppResult<()> = $crate::commands::data::cancel_sql;
            [async] sql_transaction(session_id: String, op: $crate::drivers::types::TransactionOp)
                -> $crate::error::AppResult<$crate::drivers::types::TransactionState> = $crate::commands::data::sql_transaction;

            [sync] task_statuses() -> Vec<$crate::tasks::TaskStatus> = $crate::commands::tasks::task_statuses;
            [sync] task_logs(id: String) -> Vec<String> = $crate::commands::tasks::task_logs;
            [sync] start_task(id: String) -> $crate::error::AppResult<()> = $crate::commands::tasks::start_task;
            [sync] stop_task(id: String) -> () = $crate::commands::tasks::stop_task;
            [async] restart_task(id: String) -> $crate::error::AppResult<()> = $crate::commands::tasks::restart_task;
            [sync] save_task(task: $crate::tasks::TaskDef, workspace_id: Option<String>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::tasks::save_task;
            [sync] delete_task(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::tasks::delete_task;

            [sync] save_workspace(workspace: $crate::config::WorkspaceMeta, relax_confirmation: Option<String>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::workspaces::save_workspace;
            [sync] set_active_workspace(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::workspaces::set_active_workspace;
            [async] delete_workspace(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::workspaces::delete_workspace;

            [async] vector_info(connection_id: String, database: String, table: String)
                -> $crate::error::AppResult<$crate::vector::VectorInfo> = $crate::commands::vector::vector_info;
            [async] fetch_vector(connection_id: String, database: String, table: String, column: String, key: Vec<$crate::drivers::types::CellValue>)
                -> $crate::error::AppResult<Vec<f32>> = $crate::commands::vector::fetch_vector;
            [async] vector_search(request: $crate::vector::VectorSearchRequest)
                -> $crate::error::AppResult<$crate::vector::VectorSearchResult> = $crate::commands::vector::vector_search;
            [async] vector_health(connection_id: String, database: String, table: String, column: String, sample: u32)
                -> $crate::error::AppResult<Vec<$crate::vector::HealthCheck>> = $crate::commands::vector::vector_health;
            [async] index_recall(connection_id: String, database: String, table: String, column: String, k: u32, queries: u32)
                -> $crate::error::AppResult<$crate::vector::RecallReport> = $crate::commands::vector::index_recall;
            [sync] save_embedding_settings(settings: Option<$crate::embedding::EmbeddingSettings>, api_key: Option<String>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::vector::save_embedding_settings;
            [async] test_embedding() -> $crate::error::AppResult<usize> = $crate::commands::vector::test_embedding;

            [sync] save_ai_connection(connection: $crate::ai::AiConnection, api_key: Option<String>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::ai::save_ai_connection;
            [sync] delete_ai_connection(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::ai::delete_ai_connection;
            [sync] set_ai_active(connection_id: String, model: String)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::ai::set_ai_active;
            [async] list_ai_models(settings: $crate::ai::AiSettings, api_key: Option<String>, connection_id: Option<String>)
                -> $crate::error::AppResult<Vec<String>> = $crate::commands::ai::list_ai_models;
            [async] test_ai(settings: $crate::ai::AiSettings, api_key: Option<String>, connection_id: Option<String>)
                -> $crate::error::AppResult<String> = $crate::commands::ai::test_ai;
            [sync] ai_cancel(id: String) -> () = $crate::commands::ai::ai_cancel;

            [async] history_append(entry: $crate::history::HistoryEntry)
                -> $crate::error::AppResult<Option<$crate::history::HistoryEntry>> = $crate::commands::history::history_append;
            [async] history_list(query: $crate::history::HistoryQuery)
                -> $crate::error::AppResult<Vec<$crate::history::HistoryEntry>> = $crate::commands::history::history_list;
            [async] history_delete(ids: Vec<String>) -> $crate::error::AppResult<usize> = $crate::commands::history::history_delete;
            [async] history_clear() -> $crate::error::AppResult<()> = $crate::commands::history::history_clear;
            [sync] set_query_history(enabled: bool) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::history::set_query_history;
        }
    };
}

/// A command's return value as the JSON the UI receives, or the error it rejects with.
pub trait CommandOutput {
    fn into_json(self) -> AppResult<Value>;
}

impl<T: Serialize> CommandOutput for AppResult<T> {
    fn into_json(self) -> AppResult<Value> {
        Ok(serde_json::to_value(self?)?)
    }
}

macro_rules! infallible_output {
    ($($output:ty),* $(,)?) => {
        $(impl CommandOutput for $output {
            fn into_json(self) -> AppResult<Value> {
                Ok(serde_json::to_value(self)?)
            }
        })*
    };
}

infallible_output!((), AppConfig, Capabilities, Vec<String>, Vec<TaskStatus>);

/// Rejects a command the current front-end does not offer.
pub fn unsupported(command: &str) -> AppError {
    AppError::new(format!("'{command}' is not available in this edition of Fosta Studio"))
}
