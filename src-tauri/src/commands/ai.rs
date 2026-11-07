//! LLM provider connections and the assistant chat.

use crate::{
    ai::{self, AiConnection, AiSettings, ChatRequest, ChatSink},
    config::AppConfig,
    error::{AppError, AppResult},
    secrets, AppState,
};

/// Adds or updates an LLM connection; an empty key keeps the saved one.
pub fn save_ai_connection(state: &AppState, connection: AiConnection, api_key: Option<String>) -> AppResult<AppConfig> {
    let connection = connection.normalized()?;
    if let Some(key) = api_key.as_deref().filter(|key| !key.is_empty()) {
        secrets::save_ai_key(&connection.id, key)?;
    }
    state.config.update(|config| {
        config.put_ai_connection(connection);
        Ok(())
    })
}

pub fn delete_ai_connection(state: &AppState, id: String) -> AppResult<AppConfig> {
    secrets::forget_ai_key(&id)?;
    state.config.update(|config| {
        config.remove_ai_connection(&id);
        Ok(())
    })
}

/// Picks the connection and model the chat uses.
pub fn set_ai_active(state: &AppState, connection_id: String, model: String) -> AppResult<AppConfig> {
    state.config.update(|config| config.set_ai_active(&connection_id, &model))
}

/// Lists the models of `settings` as typed in the dialog (not yet saved), or
/// of a saved connection; an empty key falls back to `connection_id`'s saved one.
pub async fn list_ai_models(
    _state: &AppState,
    settings: AiSettings,
    api_key: Option<String>,
    connection_id: Option<String>,
) -> AppResult<Vec<String>> {
    let api_key = draft_key(api_key, connection_id.as_deref())?;
    ai::list_models(&settings, api_key).await
}

/// "Test connection" in the dialog: tries the settings as typed, before saving.
pub async fn test_ai(_state: &AppState, settings: AiSettings, api_key: Option<String>, connection_id: Option<String>) -> AppResult<String> {
    let api_key = draft_key(api_key, connection_id.as_deref())?;
    ai::probe(&settings, api_key).await
}

/// Streams one assistant reply over `on_event`; resolves with the full text.
pub async fn ai_chat(state: &AppState, request: ChatRequest, on_event: &dyn ChatSink) -> AppResult<String> {
    let (settings, api_key) = chat_settings(&state.config.snapshot(), &request)?;
    ai::chat(&settings, api_key, &state.db, &state.ai, request, on_event).await
}

pub fn ai_cancel(state: &AppState, id: String) {
    state.ai.cancel(&id);
}

fn draft_key(supplied: Option<String>, connection_id: Option<&str>) -> AppResult<Option<String>> {
    match (supplied.filter(|key| !key.is_empty()), connection_id) {
        (Some(key), _) => Ok(Some(key)),
        (None, Some(id)) => secrets::load_ai_key(id),
        (None, None) => Ok(None),
    }
}

/// Provider, endpoint, model and key for a chat request: the request's own
/// connection/model when given, otherwise the active ones.
fn chat_settings(config: &AppConfig, request: &ChatRequest) -> AppResult<(AiSettings, Option<String>)> {
    let active = config.ai_active.as_ref();
    let connection_id = request
        .connection_id
        .as_deref()
        .or(active.map(|active| active.connection_id.as_str()))
        .ok_or_else(|| AppError::new("Add an LLM provider first (Assistant → Set up…)"))?;
    let connection = config.ai_connection(connection_id)?;
    let model = match (request.model.as_deref().map(str::trim).filter(|model| !model.is_empty()), active) {
        (Some(model), _) => model,
        (None, Some(active)) if active.connection_id == connection.id => active.model.as_str(),
        (None, _) => connection.default_model.as_str(),
    };
    Ok((connection.settings(model), secrets::load_ai_key(&connection.id)?))
}
