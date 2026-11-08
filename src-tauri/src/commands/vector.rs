//! MariaDB vector tooling and the embedding API it can call.

use crate::{
    config::AppConfig,
    drivers::types::CellValue,
    embedding::{self, EmbeddingSettings},
    error::{AppError, AppResult},
    gateway, secrets,
    vector::{self, HealthCheck, QueryVector, RecallReport, VectorInfo, VectorSearchRequest, VectorSearchResult},
    AppState,
};

const EMBEDDING_PROBE: &str = "Fosta Studio embedding test";

pub async fn vector_info(state: &AppState, connection_id: String, database: String, table: String) -> AppResult<VectorInfo> {
    if !state.engines.is_mysql(&connection_id)? {
        return Ok(VectorInfo {
            supported: false,
            columns: Vec::new(),
            indexes: Vec::new(),
        });
    }
    let mut conn = state.engines.mysql_conn(&connection_id).await?;
    vector::vector_info(&mut conn, &database, &table).await
}

pub async fn fetch_vector(
    state: &AppState,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    key: Vec<CellValue>,
) -> AppResult<Vec<f32>> {
    let mut conn = state.engines.mysql_conn(&connection_id).await?;
    vector::fetch_vector(&mut conn, &database, &table, &column, &key).await
}

pub async fn vector_search(state: &AppState, request: VectorSearchRequest) -> AppResult<VectorSearchResult> {
    // The optional filter is spliced into the search query: it must be one condition, like a grid filter.
    let context = super::data::gateway_context(state, &request.connection_id)?;
    gateway::check_filter(&state.engines, &context, &request.connection_id, request.filter.as_deref())?;
    let mut conn = state.engines.mysql_conn(&request.connection_id).await?;
    let query = match &request.query {
        QueryVector::Row { key } => vector::fetch_vector(&mut conn, &request.database, &request.table, &request.column, key).await?,
        QueryVector::Literal { text } => vector::parse_literal(text)?,
        QueryVector::Text { text } => embed_text(state, text).await?,
    };
    vector::search(&mut conn, &request, &query).await
}

pub async fn vector_health(
    state: &AppState,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    sample: u32,
) -> AppResult<Vec<HealthCheck>> {
    let mut conn = state.engines.mysql_conn(&connection_id).await?;
    vector::health(&mut conn, &database, &table, &column, sample).await
}

pub async fn index_recall(
    state: &AppState,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    k: u32,
    queries: u32,
) -> AppResult<RecallReport> {
    let mut conn = state.engines.mysql_conn(&connection_id).await?;
    vector::index_recall(&mut conn, &database, &table, &column, k, queries).await
}

pub fn save_embedding_settings(state: &AppState, settings: Option<EmbeddingSettings>, api_key: Option<String>) -> AppResult<AppConfig> {
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

pub async fn test_embedding(state: &AppState) -> AppResult<usize> {
    Ok(embed_text(state, EMBEDDING_PROBE).await?.len())
}

async fn embed_text(state: &AppState, text: &str) -> AppResult<Vec<f32>> {
    let settings = state
        .config
        .snapshot()
        .embedding
        .ok_or_else(|| AppError::new("Configure an embedding API first (toolbar → Embedding)"))?;
    let api_key = secrets::load_password(secrets::EMBEDDING_ACCOUNT)?;
    embedding::embed(&settings, api_key, text).await
}
