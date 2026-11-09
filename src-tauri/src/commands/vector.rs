//! Vector tooling and the embedding API it can call. The vector operations are engine operations (`vector.*`, answered
//! by connections that advertise `Capability::Vector`) run through the statement gateway; `vector_search` embeds a text
//! query here first. Arguments and results are JSON: typed key cells in, typed result sets out.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::gateway_context;
use crate::{
    config::AppConfig,
    contract::{self, Capability},
    embedding::{self, EmbeddingSettings},
    error::{AppError, AppResult},
    gateway::{self, Caller},
    secrets, AppState,
};

const EMBEDDING_PROBE: &str = "Fosta Studio embedding test";

/// A key cell of a row: the column and its typed value, as the grid holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyCell {
    pub column: String,
    pub value: contract::Value,
}

/// The query vector as the vector view sends it.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum QueryVector {
    Row { key: Vec<KeyCell> },
    Literal { text: String },
    Text { text: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorSearchRequest {
    pub connection_id: String,
    pub database: String,
    pub table: String,
    pub column: String,
    pub query: QueryVector,
    pub k: u32,
    /// `"auto"`, `"cosine"` or `"euclidean"`; the engine checks it.
    pub metric: String,
    pub filter: Option<String>,
}

/// Runs one vector operation through the gateway, as an engine view.
async fn invoke(state: &AppState, connection_id: &str, name: &str, args: Value) -> AppResult<Value> {
    let context = gateway_context(state, connection_id)?;
    gateway::invoke(&state.engines, &context, Caller::EngineView, name, args, None).await
}

pub async fn vector_info(state: &AppState, connection_id: String, database: String, table: String) -> AppResult<Value> {
    // Other engines (and MariaDB before 11.7) simply have no vector support.
    if !state
        .engines
        .handle(&connection_id)?
        .conn
        .capabilities()
        .contains(Capability::Vector)
    {
        return Ok(json!({ "supported": false, "columns": [], "indexes": [] }));
    }
    invoke(
        state,
        &connection_id,
        "vector.info",
        json!({ "database": database, "table": table }),
    )
    .await
}

pub async fn fetch_vector(
    state: &AppState,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    key: Vec<KeyCell>,
) -> AppResult<Vec<f32>> {
    let args = json!({ "database": database, "table": table, "column": column, "key": key });
    Ok(serde_json::from_value(invoke(state, &connection_id, "vector.fetch", args).await?)?)
}

pub async fn vector_search(state: &AppState, request: VectorSearchRequest) -> AppResult<Value> {
    // The optional filter is spliced into the search query: it must be one condition, like a grid filter.
    let context = gateway_context(state, &request.connection_id)?;
    gateway::check_filter(&state.engines, &context, &request.connection_id, request.filter.as_deref())?;
    let query = match request.query {
        QueryVector::Row { key } => json!({ "kind": "row", "key": key }),
        QueryVector::Literal { text } => json!({ "kind": "literal", "text": text }),
        QueryVector::Text { text } => json!({ "kind": "vector", "values": embed_text(state, &text).await? }),
    };
    let args = json!({
        "database": request.database,
        "table": request.table,
        "column": request.column,
        "query": query,
        "k": request.k,
        "metric": request.metric,
        "filter": request.filter,
    });
    invoke(state, &request.connection_id, "vector.search", args).await
}

pub async fn vector_health(
    state: &AppState,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    sample: u32,
) -> AppResult<Value> {
    let args = json!({ "database": database, "table": table, "column": column, "sample": sample });
    invoke(state, &connection_id, "vector.health", args).await
}

pub async fn index_recall(
    state: &AppState,
    connection_id: String,
    database: String,
    table: String,
    column: String,
    k: u32,
    queries: u32,
) -> AppResult<Value> {
    let args = json!({ "database": database, "table": table, "column": column, "k": k, "queries": queries });
    invoke(state, &connection_id, "vector.recall", args).await
}

/// Saves the embedding API settings. An empty key keeps the saved one, unless the URL or the way the key is sent
/// changed: then the saved key is forgotten, so it is never sent to an endpoint it was not saved for.
pub fn save_embedding_settings(state: &AppState, settings: Option<EmbeddingSettings>, api_key: Option<String>) -> AppResult<AppConfig> {
    let saved = state.config.read(|config| config.embedding.clone());
    remember_embedding_key(saved.as_ref(), settings.as_ref(), api_key)?;
    state.config.update(|config| {
        config.embedding = settings;
        Ok(())
    })
}

fn remember_embedding_key(
    saved: Option<&EmbeddingSettings>,
    settings: Option<&EmbeddingSettings>,
    api_key: Option<String>,
) -> AppResult<()> {
    let url = |settings: &EmbeddingSettings| settings.url.trim().trim_end_matches('/').to_owned();
    let Some(settings) = settings else {
        return secrets::forget_password(secrets::EMBEDDING_ACCOUNT);
    };
    match api_key.filter(|key| !key.is_empty()) {
        Some(key) => secrets::save_password(secrets::EMBEDDING_ACCOUNT, &key),
        None if saved.is_some_and(|saved| url(saved) != url(settings) || saved.auth != settings.auth) => {
            secrets::forget_password(secrets::EMBEDDING_ACCOUNT)
        }
        None => Ok(()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::EmbeddingAuth;

    const KEY: &str = "app:embedding";

    fn settings(url: &str, auth: EmbeddingAuth) -> EmbeddingSettings {
        EmbeddingSettings {
            url: url.into(),
            model: "m".into(),
            auth,
            dimensions: None,
        }
    }

    #[test]
    fn saving_another_endpoint_without_a_key_forgets_the_saved_key() {
        let saved = settings("https://api.example.com/v1/embeddings", EmbeddingAuth::Bearer);
        let store = secrets::testing::memory_store(&[(KEY, "sk")]);
        let mut renamed = settings(" https://api.example.com/v1/embeddings/ ", EmbeddingAuth::Bearer);
        renamed.model = "other".into();
        remember_embedding_key(Some(&saved), Some(&renamed), None).unwrap();
        assert_eq!(store.accounts(), [KEY], "model and spacing do not matter");
        for moved in [
            settings("https://evil.example.com/v1/embeddings", EmbeddingAuth::Bearer),
            settings("https://api.example.com/v1/embeddings", EmbeddingAuth::ApiKey),
        ] {
            let store = secrets::testing::memory_store(&[(KEY, "sk")]);
            remember_embedding_key(Some(&saved), Some(&moved), Some(String::new())).unwrap();
            assert!(store.accounts().is_empty(), "{}", moved.url);
        }
        let store = secrets::testing::memory_store(&[(KEY, "sk")]);
        remember_embedding_key(
            Some(&saved),
            Some(&settings("https://other.example.com", EmbeddingAuth::Bearer)),
            Some("new".into()),
        )
        .unwrap();
        assert_eq!(store.secrets.lock().unwrap()[KEY], "new");
        // Removing the settings forgets the key.
        let store = secrets::testing::memory_store(&[(KEY, "sk")]);
        remember_embedding_key(Some(&saved), None, Some("ignored".into())).unwrap();
        assert!(store.accounts().is_empty());
    }
}
