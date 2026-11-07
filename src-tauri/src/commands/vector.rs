//! MariaDB vector tooling and the embedding API it can call.

use tauri::State;

use crate::{
    config::AppConfig,
    drivers::types::CellValue,
    embedding::{self, EmbeddingSettings},
    error::{AppError, AppResult},
    secrets,
    vector::{self, HealthCheck, QueryVector, RecallReport, VectorInfo, VectorSearchRequest, VectorSearchResult},
    AppState,
};

const EMBEDDING_PROBE: &str = "DB Studio embedding test";

#[tauri::command]
pub async fn vector_info(state: State<'_, AppState>, connection_id: String, database: String, table: String) -> AppResult<VectorInfo> {
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
        QueryVector::Row { key } => vector::fetch_vector(&mut conn, &request.database, &request.table, &request.column, key).await?,
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
