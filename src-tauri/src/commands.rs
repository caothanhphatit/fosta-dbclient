use tauri::{ipc::Channel, State};

use crate::{
    ai::{self, AiSettings, ChatEvent, ChatRequest},
    config::{AppConfig, ConnectionProfile, WorkspaceMeta},
    db::{BrowseRequest, CellValue, QueryOutcome, RowChange, TableDesign, TableInfo, TablePage},
    drivers,
    embedding::{self, EmbeddingSettings},
    error::{AppError, AppResult, PASSWORD_REQUIRED},
    relations::TableRelations,
    secrets,
    tasks::{TaskDef, TaskStatus},
    vector::{self, HealthCheck, QueryVector, RecallReport, VectorInfo, VectorSearchRequest, VectorSearchResult},
    AppState,
};

const EMBEDDING_PROBE: &str = "DB Studio embedding test";

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> AppConfig {
    state.config.snapshot()
}

#[tauri::command]
pub fn save_connection(
    state: State<'_, AppState>,
    profile: ConnectionProfile,
    password: Option<String>,
    workspace_id: Option<String>,
) -> AppResult<AppConfig> {
    remember_password(&profile, password.as_deref())?;
    let workspace_id = workspace_id.unwrap_or_else(|| state.config.snapshot().active_workspace);
    state.config.update(|config| config.put_connection(&workspace_id, profile))
}

#[tauri::command]
pub async fn delete_connection(state: State<'_, AppState>, id: String) -> AppResult<AppConfig> {
    state.db.disconnect(&id).await;
    secrets::forget_password(&id)?;
    state.config.update(|config| {
        config.remove_connection(&id);
        Ok(())
    })
}

#[tauri::command]
pub async fn test_connection(profile: ConnectionProfile, password: Option<String>) -> AppResult<String> {
    let password = resolve_password(&profile.id, password)?;
    drivers::probe(&profile, &password).await
}

#[tauri::command]
pub async fn connect(state: State<'_, AppState>, id: String, password: Option<String>) -> AppResult<String> {
    let profile = state.config.connection(&id)?;
    let password = resolve_password(&id, password)?;
    state.db.connect(&profile, &password).await
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.db.disconnect(&id).await;
    Ok(())
}

#[tauri::command]
pub async fn list_databases(state: State<'_, AppState>, connection_id: String) -> AppResult<Vec<String>> {
    state.db.driver(&connection_id)?.list_databases().await
}

#[tauri::command]
pub async fn list_tables(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
) -> AppResult<Vec<TableInfo>> {
    state.db.driver(&connection_id)?.list_tables(&database).await
}

#[tauri::command]
pub async fn describe_table(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
) -> AppResult<TableDesign> {
    state.db.driver(&connection_id)?.describe_table(&database, &table).await
}

#[tauri::command]
pub async fn browse_table(state: State<'_, AppState>, request: BrowseRequest) -> AppResult<TablePage> {
    state.db.driver(&request.connection_id)?.browse_table(&request).await
}

#[tauri::command]
pub async fn apply_changes(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
    changes: Vec<RowChange>,
) -> AppResult<u64> {
    state.db.driver(&connection_id)?.apply_changes(&database, &table, changes).await
}

#[tauri::command]
pub async fn open_session(state: State<'_, AppState>, connection_id: String, session_id: String) -> AppResult<()> {
    state.db.open_session(&connection_id, session_id).await
}

#[tauri::command]
pub fn close_session(state: State<'_, AppState>, session_id: String) {
    state.db.close_session(&session_id);
}

#[tauri::command]
pub async fn run_sql(
    state: State<'_, AppState>,
    session_id: String,
    database: Option<String>,
    sql: String,
) -> AppResult<QueryOutcome> {
    state.db.run_query(&session_id, database, sql).await
}

#[tauri::command]
pub async fn cancel_sql(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    state.db.cancel_query(&session_id).await
}

#[tauri::command]
pub fn task_statuses(state: State<'_, AppState>) -> Vec<TaskStatus> {
    let config = state.config.snapshot();
    let tasks: Vec<TaskDef> = config.all_tasks().cloned().collect();
    state.tasks.statuses(&tasks)
}

#[tauri::command]
pub fn task_logs(state: State<'_, AppState>, id: String) -> Vec<String> {
    state.tasks.logs(&id)
}

#[tauri::command]
pub fn start_task(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.tasks.start(state.config.task(&id)?)
}

#[tauri::command]
pub fn stop_task(state: State<'_, AppState>, id: String) {
    state.tasks.stop(&id);
}

#[tauri::command]
pub async fn restart_task(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let task = state.config.task(&id)?;
    state.tasks.restart(task).await
}

#[tauri::command]
pub fn save_task(state: State<'_, AppState>, task: TaskDef, workspace_id: Option<String>) -> AppResult<AppConfig> {
    let workspace_id = workspace_id.unwrap_or_else(|| state.config.snapshot().active_workspace);
    state.config.update(|config| config.put_task(&workspace_id, task))
}

#[tauri::command]
pub fn delete_task(state: State<'_, AppState>, id: String) -> AppResult<AppConfig> {
    state.tasks.stop(&id);
    state.config.update(|config| {
        config.remove_task(&id);
        Ok(())
    })
}

#[tauri::command]
pub fn save_workspace(state: State<'_, AppState>, workspace: WorkspaceMeta) -> AppResult<AppConfig> {
    if workspace.name.trim().is_empty() {
        return Err(AppError::new("Workspace name is required"));
    }
    state.config.update(|config| {
        config.put_workspace(workspace);
        Ok(())
    })
}

/// Switches the active workspace and starts its auto-start tasks. Tasks of
/// other workspaces keep running so a port-forward in use is not cut off.
#[tauri::command]
pub fn set_active_workspace(state: State<'_, AppState>, id: String) -> AppResult<AppConfig> {
    let config = state.config.update(|config| config.set_active(&id))?;
    state.tasks.start_auto(&config.active().tasks);
    Ok(config)
}

#[tauri::command]
pub async fn delete_workspace(state: State<'_, AppState>, id: String) -> AppResult<AppConfig> {
    let doomed = state.config.snapshot().workspace(&id)?.clone();
    for task in &doomed.tasks {
        state.tasks.stop(&task.id);
    }
    for profile in &doomed.connections {
        state.db.disconnect(&profile.id).await;
        secrets::forget_password(&profile.id)?;
    }
    state.config.update(|config| config.remove_workspace(&id).map(|_| ()))
}

#[tauri::command]
pub async fn table_relations(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
) -> AppResult<TableRelations> {
    state.db.driver(&connection_id)?.table_relations(&database, &table).await
}

#[tauri::command]
pub async fn vector_info(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
) -> AppResult<VectorInfo> {
    if !state.db.is_mysql(&connection_id)? {
        return Ok(VectorInfo {
            supported: false,
            columns: Vec::new(),
            indexes: Vec::new(),
        });
    }
    let mut conn = state.db.mysql_conn(&connection_id).await?;
    vector::vector_info(&mut conn, &database, &table).await
}

#[tauri::command]
pub async fn fetch_vector(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    key: Vec<CellValue>,
) -> AppResult<Vec<f32>> {
    let mut conn = state.db.mysql_conn(&connection_id).await?;
    vector::fetch_vector(&mut conn, &database, &table, &column, &key).await
}

#[tauri::command]
pub async fn vector_search(state: State<'_, AppState>, request: VectorSearchRequest) -> AppResult<VectorSearchResult> {
    let mut conn = state.db.mysql_conn(&request.connection_id).await?;
    let query = match &request.query {
        QueryVector::Row { key } => {
            vector::fetch_vector(&mut conn, &request.database, &request.table, &request.column, key).await?
        }
        QueryVector::Literal { text } => vector::parse_literal(text)?,
        QueryVector::Text { text } => embed_text(&state, text).await?,
    };
    vector::search(&mut conn, &request, &query).await
}

#[tauri::command]
pub async fn vector_health(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    sample: u32,
) -> AppResult<Vec<HealthCheck>> {
    let mut conn = state.db.mysql_conn(&connection_id).await?;
    vector::health(&mut conn, &database, &table, &column, sample).await
}

#[tauri::command]
pub async fn index_recall(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    k: u32,
    queries: u32,
) -> AppResult<RecallReport> {
    let mut conn = state.db.mysql_conn(&connection_id).await?;
    vector::index_recall(&mut conn, &database, &table, &column, k, queries).await
}

#[tauri::command]
pub fn save_embedding_settings(
    state: State<'_, AppState>,
    settings: Option<EmbeddingSettings>,
    api_key: Option<String>,
) -> AppResult<AppConfig> {
    match (&settings, api_key.as_deref()) {
        (None, _) => secrets::forget_password(secrets::EMBEDDING_ACCOUNT)?,
        (Some(_), Some(key)) if !key.is_empty() => secrets::save_password(secrets::EMBEDDING_ACCOUNT, key)?,
        (Some(_), _) => {}
    }
    state.config.update(|config| {
        config.embedding = settings;
        Ok(())
    })
}

#[tauri::command]
pub async fn test_embedding(state: State<'_, AppState>) -> AppResult<usize> {
    Ok(embed_text(&state, EMBEDDING_PROBE).await?.len())
}

async fn embed_text(state: &State<'_, AppState>, text: &str) -> AppResult<Vec<f32>> {
    let settings = state
        .config
        .snapshot()
        .embedding
        .ok_or_else(|| AppError::new("Configure an embedding API first (toolbar → Embedding)"))?;
    let api_key = secrets::load_password(secrets::EMBEDDING_ACCOUNT)?;
    embedding::embed(&settings, api_key, text).await
}

#[tauri::command]
pub fn save_ai_settings(
    state: State<'_, AppState>,
    settings: Option<AiSettings>,
    api_key: Option<String>,
) -> AppResult<AppConfig> {
    if let Some(settings) = &settings {
        if settings.model.trim().is_empty() {
            return Err(AppError::new("Model is required"));
        }
    }
    match (&settings, api_key.as_deref()) {
        (None, _) => secrets::forget_password(secrets::AI_ACCOUNT)?,
        (Some(_), Some(key)) if !key.is_empty() => secrets::save_password(secrets::AI_ACCOUNT, key)?,
        (Some(_), _) => {}
    }
    state.config.update(|config| {
        config.ai = settings;
        Ok(())
    })
}

/// Lists the models of `settings` as typed in the dialog (not yet saved);
/// an empty key falls back to the saved one.
#[tauri::command]
pub async fn list_ai_models(
    state: State<'_, AppState>,
    settings: AiSettings,
    api_key: Option<String>,
) -> AppResult<Vec<String>> {
    let api_key = match api_key.filter(|key| !key.is_empty()) {
        Some(key) => Some(key),
        None => secrets::load_password(secrets::AI_ACCOUNT)?,
    };
    let _ = &state;
    ai::list_models(&settings, api_key).await
}

#[tauri::command]
pub async fn test_ai(state: State<'_, AppState>) -> AppResult<String> {
    let (settings, api_key) = ai_settings(&state)?;
    ai::probe(&settings, api_key).await
}

/// Streams one assistant reply over `on_event`; resolves with the full text.
#[tauri::command]
pub async fn ai_chat(
    state: State<'_, AppState>,
    request: ChatRequest,
    on_event: Channel<ChatEvent>,
) -> AppResult<String> {
    let (settings, api_key) = ai_settings(&state)?;
    ai::chat(&settings, api_key, &state.db, &state.ai, request, on_event).await
}

#[tauri::command]
pub fn ai_cancel(state: State<'_, AppState>, id: String) {
    state.ai.cancel(&id);
}

fn ai_settings(state: &State<'_, AppState>) -> AppResult<(AiSettings, Option<String>)> {
    let settings = state
        .config
        .snapshot()
        .ai
        .ok_or_else(|| AppError::new("Set up the assistant first (toolbar → Assistant → settings)"))?;
    Ok((settings, secrets::load_password(secrets::AI_ACCOUNT)?))
}

fn resolve_password(connection_id: &str, supplied: Option<String>) -> AppResult<String> {
    match supplied {
        Some(password) => Ok(password),
        None => secrets::load_password(connection_id)?.ok_or_else(|| AppError::new(PASSWORD_REQUIRED)),
    }
}

fn remember_password(profile: &ConnectionProfile, password: Option<&str>) -> AppResult<()> {
    match (profile.save_password, password) {
        (true, Some(password)) => secrets::save_password(&profile.id, password),
        (true, None) => Ok(()),
        (false, _) => secrets::forget_password(&profile.id),
    }
}
