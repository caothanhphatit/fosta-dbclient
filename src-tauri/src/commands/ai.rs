//! LLM provider connections and the assistant chat.

use crate::{
    ai::{self, AiConnection, AiSettings, ChatRequest, ChatSink},
    config::AppConfig,
    error::{AppError, AppResult, PASSWORD_REQUIRED},
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
    state: &AppState,
    settings: AiSettings,
    api_key: Option<String>,
    connection_id: Option<String>,
) -> AppResult<Vec<String>> {
    let api_key = draft_key(state, &settings, api_key, connection_id.as_deref())?;
    ai::list_models(&settings, api_key).await
}

/// "Test connection" in the dialog: tries the settings as typed, before saving.
pub async fn test_ai(state: &AppState, settings: AiSettings, api_key: Option<String>, connection_id: Option<String>) -> AppResult<String> {
    let api_key = draft_key(state, &settings, api_key, connection_id.as_deref())?;
    ai::probe(&settings, api_key).await
}

/// Streams one assistant reply over `on_event`; resolves with the full text.
pub async fn ai_chat(state: &AppState, request: ChatRequest, on_event: &dyn ChatSink) -> AppResult<String> {
    let (settings, api_key) = chat_settings(&state.config.snapshot(), &request)?;
    ai::chat(&settings, api_key, &state.engines, &state.ai, request, on_event).await
}

pub fn ai_cancel(state: &AppState, id: String) {
    state.ai.cancel(&id);
}

/// The key typed in the dialog, else `connection_id`'s saved key, but only while the dialog still points at the
/// saved provider and endpoint; a saved key is never sent to an endpoint it was not saved for.
fn draft_key(state: &AppState, settings: &AiSettings, supplied: Option<String>, connection_id: Option<&str>) -> AppResult<Option<String>> {
    if let Some(key) = supplied.filter(|key| !key.is_empty()) {
        return Ok(Some(key));
    }
    let Some(id) = connection_id else {
        return Ok(None);
    };
    let saved = state.config.read(|config| config.ai_connection(id).ok().cloned());
    match saved {
        Some(saved) if same_endpoint(&saved, settings) => secrets::load_ai_key(id),
        Some(_) if secrets::contains(&secrets::SecretKey::AiKey(id.to_owned()))? => Err(AppError::coded(
            PASSWORD_REQUIRED,
            "The endpoint differs from the saved one: enter the API key again",
        )),
        _ => Ok(None),
    }
}

fn same_endpoint(saved: &AiConnection, draft: &AiSettings) -> bool {
    let url = |text: &str| text.trim().trim_end_matches('/').to_owned();
    saved.provider == draft.provider && url(&saved.base_url) == url(&draft.base_url)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::AiProvider;

    #[test]
    fn saved_keys_are_bound_to_provider_and_endpoint() {
        let saved = AiConnection {
            id: "a".into(),
            name: "A".into(),
            provider: AiProvider::Openai,
            base_url: "https://api.example.com/v1".into(),
            default_model: "m".into(),
        };
        let draft = |provider, base_url: &str| AiSettings {
            provider,
            base_url: base_url.into(),
            model: "m".into(),
        };
        assert!(same_endpoint(&saved, &draft(AiProvider::Openai, " https://api.example.com/v1/ ")));
        assert!(!same_endpoint(&saved, &draft(AiProvider::Openai, "https://evil.example.com/v1")));
        assert!(!same_endpoint(&saved, &draft(AiProvider::Anthropic, "https://api.example.com/v1")));
    }
}
