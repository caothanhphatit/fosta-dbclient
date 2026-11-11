//! Vector tooling and the embedding API it can call. The vector operations are engine operations (`vector.*`, answered
//! by connections that advertise `Capability::Vector`) run through the statement gateway; `vector_search` embeds a text
//! query here first. Arguments and results are JSON: typed key cells in, typed result sets out.

use serde::Deserialize;
use serde_json::{json, Value};

use super::{gateway_context, keys};
pub use crate::contract::vector::KeyCell;
use crate::{
    config::AppConfig,
    contract::Capability,
    embedding::{self, EmbeddingSettings},
    error::{AppError, AppResult},
    gateway::{self, Caller},
    secrets, AppState,
};

const EMBEDDING_PROBE: &str = "Fosta Studio embedding test";

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
    // Engines without vectors (or a server too old, an extension not installed) do not advertise the capability.
    if !state
        .engines
        .handle(&connection_id)?
        .conn
        .capabilities()
        .contains(Capability::Vector)
    {
        return Ok(json!({ "supported": false, "columns": [], "indexes": [], "metrics": [] }));
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

/// Saves the embedding API settings. An empty key keeps the saved one, unless the endpoint or the way the key is sent
/// changed: then the saved key is forgotten, so it is never sent to an endpoint it was not saved for. The model and
/// the dimensions do not matter. `key_ref` picks a key saved for the same API instead of a typed one (see [`keys`]).
pub fn save_embedding_settings(
    state: &AppState,
    settings: Option<EmbeddingSettings>,
    api_key: Option<String>,
    key_ref: Option<String>,
) -> AppResult<AppConfig> {
    let config = state.config.snapshot();
    let api_key = typed_or_picked(&config, settings.as_ref(), api_key, key_ref)?;
    remember_embedding_key(config.embedding.as_ref(), settings.as_ref(), api_key)?;
    state.config.update(|config| {
        config.embedding = settings;
        Ok(())
    })
}

/// The typed key, else the secret behind `key_ref` when it was saved for the same API as `settings`.
fn typed_or_picked(
    config: &AppConfig,
    settings: Option<&EmbeddingSettings>,
    api_key: Option<String>,
    key_ref: Option<String>,
) -> AppResult<Option<String>> {
    match (api_key.filter(|key| !key.is_empty()), key_ref.filter(|id| !id.is_empty()), settings) {
        (Some(key), _, _) => Ok(Some(key)),
        (None, Some(id), Some(settings)) => keys::resolve(config, &id, &embedding::endpoint(&settings.url)).map(Some),
        _ => Ok(None),
    }
}

/// Whether a key saved for `saved` still belongs to `settings`: the same endpoint (as requests are sent) and the same
/// way of sending the key.
fn same_target(saved: &EmbeddingSettings, settings: &EmbeddingSettings) -> bool {
    embedding::endpoint(&saved.url) == embedding::endpoint(&settings.url) && saved.auth == settings.auth
}

fn remember_embedding_key(
    saved: Option<&EmbeddingSettings>,
    settings: Option<&EmbeddingSettings>,
    api_key: Option<String>,
) -> AppResult<()> {
    let Some(settings) = settings else {
        return secrets::forget_password(secrets::EMBEDDING_ACCOUNT);
    };
    match api_key.filter(|key| !key.is_empty()) {
        Some(key) => secrets::save_password(secrets::EMBEDDING_ACCOUNT, &key),
        None if saved.is_some_and(|saved| !same_target(saved, settings)) => secrets::forget_password(secrets::EMBEDDING_ACCOUNT),
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

    #[test]
    fn a_model_or_dimensions_change_keeps_the_saved_key() {
        let saved = settings("https://api.openai.com/v1/embeddings", EmbeddingAuth::Bearer);
        let mut next = saved.clone();
        next.model = "text-embedding-3-small".into();
        next.dimensions = Some(512);
        let store = secrets::testing::memory_store(&[(KEY, "sk")]);
        remember_embedding_key(Some(&saved), Some(&next), None).unwrap();
        assert_eq!(store.secrets.lock().unwrap()[KEY], "sk");
        // The base URL and the full URL send requests to the same place.
        let base = settings("https://api.openai.com/v1", EmbeddingAuth::Bearer);
        remember_embedding_key(Some(&saved), Some(&base), None).unwrap();
        assert_eq!(store.accounts(), [KEY]);
    }

    #[test]
    fn a_picked_key_is_used_only_for_the_same_api() {
        let config = keys::tests::config();
        let store = secrets::testing::memory_store(&[("ai:work", "sk-work")]);
        let same = settings("https://api.openai.com/v1/embeddings", EmbeddingAuth::Bearer);
        let picked = typed_or_picked(&config, Some(&same), None, Some("ai:work".into())).unwrap();
        assert_eq!(picked.as_deref(), Some("sk-work"));
        remember_embedding_key(config.embedding.as_ref(), Some(&same), picked).unwrap();
        assert_eq!(store.secrets.lock().unwrap()[KEY], "sk-work");
        // Another host: refused, nothing saved.
        let store = secrets::testing::memory_store(&[("ai:work", "sk-work")]);
        let elsewhere = settings("https://evil.example.com/v1/embeddings", EmbeddingAuth::Bearer);
        assert!(typed_or_picked(&config, Some(&elsewhere), None, Some("ai:work".into())).is_err());
        assert_eq!(store.accounts(), ["ai:work"]);
        // A typed key wins over a picked one.
        let typed = typed_or_picked(&config, Some(&elsewhere), Some("typed".into()), Some("ai:work".into())).unwrap();
        assert_eq!(typed.as_deref(), Some("typed"));
    }
}
