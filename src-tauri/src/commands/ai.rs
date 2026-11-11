//! LLM provider connections and the assistant chat.

use crate::{
    ai::{self, AiConnection, AiProvider, AiSettings, ChatRequest, ChatSink},
    commands::keys,
    config::AppConfig,
    error::{AppError, AppResult, PASSWORD_REQUIRED},
    gateway::{assistant::reads_allowed, Context},
    secrets, AppState,
};

/// Adds or updates an LLM connection; an empty key keeps the saved one, unless the provider or endpoint changed:
/// then the saved key is forgotten, so it is never sent to an endpoint it was not saved for. The name and the default
/// model do not matter. `key_ref` picks a key saved for the same API instead of a typed one (see [`keys`]).
pub fn save_ai_connection(
    state: &AppState,
    connection: AiConnection,
    api_key: Option<String>,
    key_ref: Option<String>,
) -> AppResult<AppConfig> {
    let connection = connection.normalized()?;
    let config = state.config.snapshot();
    let saved = config.ai_connection(&connection.id).ok();
    let api_key = match (api_key.filter(|key| !key.is_empty()), key_ref.filter(|id| !id.is_empty())) {
        (Some(key), _) => Some(key),
        (None, Some(id)) => Some(keys::resolve(
            &config,
            &id,
            &keys::ai_endpoint(connection.provider, &connection.base_url)?,
        )?),
        (None, None) => None,
    };
    remember_ai_key(saved, &connection, api_key)?;
    state.config.update(|config| {
        config.put_ai_connection(connection);
        Ok(())
    })
}

/// Stores a typed key, or forgets the saved one when `connection` no longer uses the `saved` provider and endpoint.
fn remember_ai_key(saved: Option<&AiConnection>, connection: &AiConnection, api_key: Option<String>) -> AppResult<()> {
    match api_key.filter(|key| !key.is_empty()) {
        Some(key) => secrets::save_ai_key(&connection.id, &key),
        None if saved.is_some_and(|saved| !same_endpoint(saved, connection.provider, &connection.base_url)) => {
            secrets::forget_ai_key(&connection.id)
        }
        None => Ok(()),
    }
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
    key_ref: Option<String>,
) -> AppResult<Vec<String>> {
    let api_key = draft_key(state, &settings, api_key, connection_id.as_deref(), key_ref)?;
    ai::list_models(&settings, api_key).await
}

/// "Test connection" in the dialog: tries the settings as typed, before saving.
pub async fn test_ai(
    state: &AppState,
    settings: AiSettings,
    api_key: Option<String>,
    connection_id: Option<String>,
    key_ref: Option<String>,
) -> AppResult<String> {
    let api_key = draft_key(state, &settings, api_key, connection_id.as_deref(), key_ref)?;
    ai::probe(&settings, api_key).await
}

/// Streams one assistant reply over `on_event`; resolves with the full text.
pub async fn ai_chat(state: &AppState, request: ChatRequest, on_event: &dyn ChatSink) -> AppResult<String> {
    let config = state.config.snapshot();
    let (settings, api_key) = chat_settings(&config, &request)?;
    let reads = request
        .context
        .connection_id
        .as_deref()
        .and_then(|connection_id| read_query_context(&config, connection_id));
    ai::chat(
        &settings,
        api_key,
        &state.engines,
        &state.ai,
        Some(&state.profiles),
        request,
        reads,
        on_event,
    )
    .await
}

/// Settings → "Allow the assistant to run read-only queries"; each workspace can override it.
pub fn set_assistant_read_queries(state: &AppState, enabled: bool) -> AppResult<AppConfig> {
    state.config.update(|config| {
        config.assistant_read_queries = enabled;
        Ok(())
    })
}

/// The gateway context for the assistant's read-only queries on `connection_id`, when the user allows them there.
fn read_query_context(config: &AppConfig, connection_id: &str) -> Option<Context> {
    let (workspace, _) = config.connection_scope(connection_id)?;
    let context = Context::for_connection(config, connection_id).ok()?;
    reads_allowed(
        config.assistant_read_queries,
        workspace.assistant_queries,
        &context.safety,
        &workspace.icon,
    )
    .then_some(context)
}

pub fn ai_cancel(state: &AppState, id: String) {
    state.ai.cancel(&id);
}

/// The key typed in the dialog, else `connection_id`'s saved key, but only while the dialog still points at the
/// saved provider and endpoint; a saved key is never sent to an endpoint it was not saved for. `key_ref` picks a key
/// saved for the same API instead (see [`keys`]).
fn draft_key(
    state: &AppState,
    settings: &AiSettings,
    supplied: Option<String>,
    connection_id: Option<&str>,
    key_ref: Option<String>,
) -> AppResult<Option<String>> {
    if let Some(key) = supplied.filter(|key| !key.is_empty()) {
        return Ok(Some(key));
    }
    if let Some(id) = key_ref.filter(|id| !id.is_empty()) {
        let config = state.config.snapshot();
        return keys::resolve(&config, &id, &settings.base_url()?).map(Some);
    }
    let Some(id) = connection_id else {
        return Ok(None);
    };
    let saved = state.config.read(|config| config.ai_connection(id).ok().cloned());
    match saved {
        Some(saved) if same_endpoint(&saved, settings.provider, &settings.base_url) => secrets::load_ai_key(id),
        Some(_) if secrets::contains(&secrets::SecretKey::AiKey(id.to_owned()))? => Err(AppError::coded(
            PASSWORD_REQUIRED,
            "The endpoint differs from the saved one: enter the API key again",
        )),
        _ => Ok(None),
    }
}

fn same_endpoint(saved: &AiConnection, provider: AiProvider, base_url: &str) -> bool {
    let url = |text: &str| text.trim().trim_end_matches('/').to_owned();
    saved.provider == provider && url(&saved.base_url) == url(base_url)
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
    use crate::{
        ai::AiProvider,
        config::ConnectionProfile,
        gateway::{
            assistant::AssistantQueries,
            rules::{Preset, SafetySettings, PRODUCTION_ICON},
        },
    };

    #[test]
    fn read_queries_follow_the_setting_the_workspace_and_the_connection() {
        let mut config = AppConfig::default();
        config.workspaces[0].connections.push(ConnectionProfile {
            id: "c".into(),
            name: "C".into(),
            engine: "mariadb".into(),
            ..ConnectionProfile::default()
        });
        config.workspaces[0].safety = Some(SafetySettings::preset(Preset::Guarded));
        assert!(read_query_context(&config, "c").is_some(), "on by default");
        assert!(read_query_context(&config, "missing").is_none());
        config.assistant_read_queries = false;
        assert!(read_query_context(&config, "c").is_none());
        config.workspaces[0].assistant_queries = AssistantQueries::Allow;
        assert!(read_query_context(&config, "c").is_some(), "the workspace overrides the setting");
        config.assistant_read_queries = true;
        config.workspaces[0].assistant_queries = AssistantQueries::Default;
        config.workspaces[0].icon = PRODUCTION_ICON.into();
        assert!(read_query_context(&config, "c").is_none(), "off for Production");
        config.workspaces[0].icon = "laptop".into();
        config.workspaces[0].connections[0].safety = Some(SafetySettings::preset(Preset::ReadOnly));
        assert!(read_query_context(&config, "c").is_none(), "off for a Read-only connection");
    }

    #[test]
    fn saved_keys_are_bound_to_provider_and_endpoint() {
        let saved = AiConnection {
            id: "a".into(),
            name: "A".into(),
            provider: AiProvider::Openai,
            base_url: "https://api.example.com/v1".into(),
            default_model: "m".into(),
        };
        assert!(same_endpoint(&saved, AiProvider::Openai, " https://api.example.com/v1/ "));
        assert!(!same_endpoint(&saved, AiProvider::Openai, "https://evil.example.com/v1"));
        assert!(!same_endpoint(&saved, AiProvider::Anthropic, "https://api.example.com/v1"));
    }

    #[test]
    fn saving_another_endpoint_without_a_key_forgets_the_saved_key() {
        const KEY: &str = "ai:a";
        let saved = AiConnection {
            id: "a".into(),
            name: "A".into(),
            provider: AiProvider::Openai,
            base_url: "https://api.example.com/v1".into(),
            default_model: "m".into(),
        };
        let with = |provider, base_url: &str| AiConnection {
            provider,
            base_url: base_url.into(),
            name: "Renamed".into(),
            ..saved.clone()
        };
        // Same endpoint, no key: kept.
        let store = secrets::testing::memory_store(&[(KEY, "sk")]);
        remember_ai_key(Some(&saved), &with(AiProvider::Openai, "https://api.example.com/v1/"), None).unwrap();
        remember_ai_key(Some(&saved), &saved, Some(String::new())).unwrap();
        assert_eq!(store.accounts(), [KEY]);
        // Another endpoint or provider without a key: forgotten.
        for moved in [
            with(AiProvider::Openai, "https://evil.example.com/v1"),
            with(AiProvider::Anthropic, "https://api.example.com/v1"),
        ] {
            let store = secrets::testing::memory_store(&[(KEY, "sk")]);
            remember_ai_key(Some(&saved), &moved, None).unwrap();
            assert!(store.accounts().is_empty(), "{moved:?}");
        }
        // Another endpoint with a new key: the new key is saved.
        let store = secrets::testing::memory_store(&[(KEY, "sk")]);
        remember_ai_key(
            Some(&saved),
            &with(AiProvider::Openai, "https://other.example.com"),
            Some("new".into()),
        )
        .unwrap();
        assert_eq!(store.secrets.lock().unwrap()[KEY], "new");
    }

    #[test]
    fn a_model_change_keeps_the_key_and_a_picked_key_must_fit() {
        const KEY: &str = "ai:work";
        let config = keys::tests::config();
        let saved = config.ai_connection("work").unwrap().clone();
        let store = secrets::testing::memory_store(&[(KEY, "sk"), ("app:embedding", "emb")]);
        let other_model = AiConnection {
            default_model: "gpt-5-mini".into(),
            ..saved.clone()
        };
        remember_ai_key(Some(&saved), &other_model, None).unwrap();
        assert_eq!(store.secrets.lock().unwrap()[KEY], "sk");
        // The embedding key was saved for api.openai.com/v1/embeddings: it fits OpenAI, not Ollama.
        let openai = keys::ai_endpoint(AiProvider::Openai, "").unwrap();
        assert_eq!(keys::resolve(&config, "app:embedding", &openai).unwrap(), "emb");
        let ollama = keys::ai_endpoint(AiProvider::Custom, "http://localhost:11434/v1").unwrap();
        assert!(keys::resolve(&config, "app:embedding", &ollama).is_err());
    }
}
