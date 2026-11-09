//! Commands: the whole backend surface the UI calls through `src/core/api.ts`.
//! Command names and argument names are part of that contract; keep them stable.
//!
//! The bodies are plain functions taking the [`AppState`]; the two front-ends expose them: the desktop app
//! as Tauri commands (`main.rs`) and the web server as `POST /api/invoke/<command>` (`server/`). Both are
//! generated from [`for_each_command!`](crate::for_each_command), so a command added there reaches both.

pub mod ai;
pub mod capability;
pub mod connections;
pub mod diagnostics;
pub mod engines;
pub mod files;
pub mod history;
pub mod lang;
pub mod objects;
pub mod query;
pub mod tasks;
pub mod vector;
pub mod workspaces;

use serde::Serialize;
use serde_json::Value;

use crate::{
    config::AppConfig,
    error::{AppError, AppResult},
    gateway::Context,
    tasks::TaskStatus,
    AppState, Capabilities,
};

/// The gateway context (workspace, connection, rules) of a saved connection.
pub(crate) fn gateway_context(state: &AppState, connection_id: &str) -> AppResult<Context> {
    state.config.read(|config| Context::for_connection(config, connection_id))
}

pub fn get_config(state: &AppState) -> AppConfig {
    state.config.snapshot()
}

/// What this front-end supports (startup tasks, "Show in folder", where downloads go); the UI hides the rest.
pub fn capabilities(state: &AppState) -> Capabilities {
    state.capabilities.clone()
}

/// Calls `$callback!` with every command both front-ends share, one entry per command:
/// `[sync|async] name(argument: Type, …) -> ReturnType = path::to::function;`, or
/// `[stream] name(…) -> ReturnType => EventType = path;` for commands that stream events while they run.
/// Each function takes `&AppState` first, then the listed arguments, whose camelCase names are the JSON keys; a stream
/// command also takes a trailing `crate::events::EventSink<EventType>` (desktop: a Tauri channel `onEvent`; web: NDJSON).
///
/// Not listed (each front-end handles them itself): `ai_chat` (it streams events), and the desktop-only
/// `save_download` / `reveal_file` / `open_log_folder` (the web edition downloads through the browser).
#[macro_export]
macro_rules! for_each_command {
    ($callback:ident) => {
        $callback! {
            [sync] get_config() -> $crate::config::AppConfig = $crate::commands::get_config;
            [sync] capabilities() -> $crate::Capabilities = $crate::commands::capabilities;

            [sync] save_connection(profile: $crate::config::ConnectionProfile, password: Option<String>, workspace_id: Option<String>, secrets: Option<std::collections::HashMap<$crate::contract::SecretSlot, String>>, relax_confirmation: Option<String>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::connections::save_connection;
            [async] delete_connection(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::connections::delete_connection;
            [async] test_connection(profile: $crate::config::ConnectionProfile, password: Option<String>,
                secrets: Option<std::collections::HashMap<$crate::contract::SecretSlot, String>>)
                -> $crate::error::AppResult<$crate::contract::ConnectInfo> = $crate::commands::connections::test_connection;
            [async] connect(id: String, password: Option<String>)
                -> $crate::error::AppResult<$crate::contract::ConnectInfo> = $crate::commands::connections::connect;
            [sync] trust_host_key(host: String, port: u16, public_key: String) -> $crate::error::AppResult<()>
                = $crate::commands::connections::trust_host_key;
            [async] disconnect(id: String) -> $crate::error::AppResult<()> = $crate::commands::connections::disconnect;

            [sync] engine_descriptors() -> Vec<$crate::contract::EngineDescriptor> = $crate::commands::engines::engine_descriptors;
            [sync] parse_connection_string(engine: String, text: String)
                -> $crate::error::AppResult<$crate::commands::engines::ParsedConnectionString> = $crate::commands::engines::parse_connection_string;
            [sync] oracle_client_status() -> $crate::error::AppResult<serde_json::Value> = $crate::commands::engines::oracle_client_status;
            [stream] oracle_client_download() -> $crate::error::AppResult<serde_json::Value> => serde_json::Value
                = $crate::commands::engines::oracle_client_download;
            [sync] oracle_client_cancel() -> () = $crate::commands::engines::oracle_client_cancel;

            [async] tree_children(connection_id: String, request: $crate::contract::TreeRequest)
                -> $crate::error::AppResult<$crate::contract::TreeChildren> = $crate::commands::objects::tree_children;
            [async] describe_object(connection_id: String, object: $crate::contract::ObjectRef)
                -> $crate::error::AppResult<$crate::contract::ObjectDescription> = $crate::commands::objects::describe_object;
            [async] browse_object(connection_id: String, request: $crate::contract::BrowseRequest)
                -> $crate::error::AppResult<$crate::contract::Page> = $crate::commands::objects::browse_object;
            [async] apply_edits(connection_id: String, changes: $crate::contract::RowChangeSet, caller: Option<$crate::gateway::Caller>,
                import_id: Option<String>, confirmation: Option<$crate::gateway::Confirmation>)
                -> $crate::error::AppResult<$crate::contract::ApplyResult> = $crate::commands::objects::apply_edits;
            [async] schema_graph(connection_id: String, database: String)
                -> $crate::error::AppResult<$crate::contract::SchemaGraph> = $crate::commands::objects::schema_graph;
            [async] create_database(connection_id: String, name: String, first_collection: Option<String>,
                confirmation: Option<$crate::gateway::Confirmation>)
                -> $crate::error::AppResult<()> = $crate::commands::objects::create_database;

            [async] open_session(connection_id: String, session_id: String, options: Option<$crate::contract::SessionOptions>)
                -> $crate::error::AppResult<$crate::contract::SessionInfo> = $crate::commands::query::open_session;
            [async] close_session(session_id: String) -> $crate::error::AppResult<()> = $crate::commands::query::close_session;
            [stream] run(session_id: String, request: $crate::contract::RunRequest, confirmation: Option<$crate::gateway::Confirmation>, max_rows: Option<usize>)
                -> $crate::error::AppResult<$crate::contract::RunSummary> => $crate::contract::StreamEvent = $crate::commands::query::run;
            [async] cancel_run(session_id: String) -> $crate::error::AppResult<()> = $crate::commands::query::cancel_run;
            [async] transaction(session_id: String, op: $crate::contract::TransactionOp)
                -> $crate::error::AppResult<$crate::contract::TransactionState> = $crate::commands::query::transaction;
            [async] invoke_capability(connection_id: String, name: String, args: Option<serde_json::Value>,
                confirmation: Option<$crate::gateway::Confirmation>)
                -> $crate::error::AppResult<serde_json::Value> = $crate::commands::capability::invoke_capability;
            [async] completion_schema(connection_id: String, database: Option<String>)
                -> $crate::error::AppResult<$crate::lang::CompletionSchema> = $crate::commands::lang::completion_schema;
            [sync] format_sql(dialect: Option<$crate::contract::SqlDialectId>, text: String, options: Option<$crate::lang::FormatOptions>)
                -> $crate::error::AppResult<String> = $crate::commands::lang::format_sql;

            [sync] task_statuses() -> Vec<$crate::tasks::TaskStatus> = $crate::commands::tasks::task_statuses;
            [sync] task_logs(id: String) -> Vec<String> = $crate::commands::tasks::task_logs;
            [sync] start_task(id: String) -> $crate::error::AppResult<()> = $crate::commands::tasks::start_task;
            [sync] stop_task(id: String) -> () = $crate::commands::tasks::stop_task;
            [async] restart_task(id: String) -> $crate::error::AppResult<()> = $crate::commands::tasks::restart_task;
            [sync] save_task(task: $crate::tasks::TaskDef, workspace_id: Option<String>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::tasks::save_task;
            [sync] delete_task(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::tasks::delete_task;
            [sync] tunnel_status(connection_id: String) -> $crate::tasks::TaskStatus = $crate::commands::tasks::tunnel_status;
            [sync] tunnel_logs(connection_id: String) -> Vec<String> = $crate::commands::tasks::tunnel_logs;

            [sync] save_workspace(workspace: $crate::config::WorkspaceMeta, relax_confirmation: Option<String>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::workspaces::save_workspace;
            [sync] skip_confirmations(connection_id: String, categories: Vec<$crate::gateway::classify::Category>)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::workspaces::skip_confirmations;
            [sync] set_active_workspace(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::workspaces::set_active_workspace;
            [async] delete_workspace(id: String) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::workspaces::delete_workspace;

            [async] vector_info(connection_id: String, database: String, table: String)
                -> $crate::error::AppResult<serde_json::Value> = $crate::commands::vector::vector_info;
            [async] fetch_vector(connection_id: String, database: String, table: String, column: String, key: Vec<$crate::commands::vector::KeyCell>)
                -> $crate::error::AppResult<Vec<f32>> = $crate::commands::vector::fetch_vector;
            [async] vector_search(request: $crate::commands::vector::VectorSearchRequest)
                -> $crate::error::AppResult<serde_json::Value> = $crate::commands::vector::vector_search;
            [async] vector_health(connection_id: String, database: String, table: String, column: String, sample: u32)
                -> $crate::error::AppResult<serde_json::Value> = $crate::commands::vector::vector_health;
            [async] index_recall(connection_id: String, database: String, table: String, column: String, k: u32, queries: u32)
                -> $crate::error::AppResult<serde_json::Value> = $crate::commands::vector::index_recall;
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
            [sync] set_assistant_read_queries(enabled: bool)
                -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::ai::set_assistant_read_queries;

            [async] history_append(entry: $crate::history::HistoryEntry)
                -> $crate::error::AppResult<Option<$crate::history::HistoryEntry>> = $crate::commands::history::history_append;
            [async] history_list(query: $crate::history::HistoryQuery)
                -> $crate::error::AppResult<Vec<$crate::history::HistoryEntry>> = $crate::commands::history::history_list;
            [async] history_delete(ids: Vec<String>) -> $crate::error::AppResult<usize> = $crate::commands::history::history_delete;
            [async] history_clear() -> $crate::error::AppResult<()> = $crate::commands::history::history_clear;
            [sync] set_query_history(enabled: bool) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::history::set_query_history;

            [sync] diagnostics() -> $crate::commands::diagnostics::Diagnostics = $crate::commands::diagnostics::diagnostics;
            [sync] set_debug_logging(enabled: bool) -> $crate::error::AppResult<$crate::config::AppConfig> = $crate::commands::diagnostics::set_debug_logging;
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

infallible_output!(
    (),
    AppConfig,
    Capabilities,
    Vec<String>,
    Vec<TaskStatus>,
    TaskStatus,
    diagnostics::Diagnostics,
    Vec<crate::contract::EngineDescriptor>
);

/// Rejects a command the current front-end does not offer.
pub fn unsupported(command: &str) -> AppError {
    AppError::new(format!("'{command}' is not available in this edition of Fosta Studio"))
}
