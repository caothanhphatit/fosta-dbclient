//! Saved API keys picked by reference instead of typed again. The UI only gets a reference (the key's account name,
//! `ai:<id>` or `app:embedding`) and a label, never the secret; saving with a reference copies the secret into the
//! account of what is being saved, so keys stay stored the way they are now.
//!
//! A key is offered, and accepted, only for the same API it was saved for: same scheme, host and port, and one
//! endpoint's path inside the other's (`https://api.openai.com/v1` and `https://api.openai.com/v1/embeddings`).
//! A saved key never goes to another host this way.

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::{
    ai::{AiProvider, AiSettings},
    config::AppConfig,
    embedding::{self, EmbeddingAuth},
    error::{AppError, AppResult},
    secrets::{self, SecretKey},
    AppState,
};

/// A saved key as the UI sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedKeyRef {
    pub id: String,
    pub label: String,
}

/// What a key is wanted for: the embedding API, or an LLM connection (`connection_id` when editing one; its own
/// key is not offered, the dialog keeps it anyway).
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum KeyTarget {
    #[serde(rename_all = "camelCase")]
    Embedding { url: String },
    #[serde(rename_all = "camelCase")]
    Ai {
        provider: AiProvider,
        #[serde(default)]
        base_url: String,
        #[serde(default)]
        connection_id: Option<String>,
    },
}

impl KeyTarget {
    fn endpoint(&self) -> AppResult<String> {
        match self {
            Self::Embedding { url } => Ok(embedding::endpoint(url)),
            Self::Ai { provider, base_url, .. } => ai_endpoint(*provider, base_url),
        }
    }

    fn own_key(&self) -> Option<SecretKey> {
        match self {
            Self::Ai {
                connection_id: Some(id), ..
            } => Some(SecretKey::AiKey(id.clone())),
            _ => None,
        }
    }
}

/// Where requests of an LLM connection go.
pub(crate) fn ai_endpoint(provider: AiProvider, base_url: &str) -> AppResult<String> {
    AiSettings {
        provider,
        base_url: base_url.to_owned(),
        model: String::new(),
    }
    .base_url()
}

/// A key that can be saved, with the endpoint it belongs to.
struct Owner {
    key: SecretKey,
    label: String,
    endpoint: String,
}

/// Everything in `config` that can own a key: the embedding API (unless it sends none) and every LLM connection.
fn owners(config: &AppConfig) -> Vec<Owner> {
    let embedding = config
        .embedding
        .as_ref()
        .filter(|settings| settings.auth != EmbeddingAuth::None)
        .map(|settings| {
            let endpoint = embedding::endpoint(&settings.url);
            Owner {
                key: SecretKey::Embedding,
                label: format!("Embedding API ({})", host(&endpoint)),
                endpoint,
            }
        });
    let ai = config.ai_connections.iter().filter_map(|connection| {
        let endpoint = ai_endpoint(connection.provider, &connection.base_url).ok()?;
        Some(Owner {
            key: SecretKey::AiKey(connection.id.clone()),
            label: format!("{} ({})", connection.name, host(&endpoint)),
            endpoint,
        })
    });
    embedding.into_iter().chain(ai).collect()
}

/// Host and, when not the default one, port of `url`; the URL itself when it does not parse.
fn host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|parsed| {
            let host = parsed.host_str()?.to_owned();
            Some(match parsed.port() {
                Some(port) => format!("{host}:{port}"),
                None => host,
            })
        })
        .unwrap_or_else(|| url.to_owned())
}

/// Whether a key saved for endpoint `a` may be sent to endpoint `b`: same scheme, host and port, and one path inside
/// the other. The query is ignored.
pub fn same_api(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (Url::parse(a.trim()), Url::parse(b.trim())) else {
        return false;
    };
    let path = |url: &Url| url.path().trim_end_matches('/').to_owned();
    let (path_a, path_b) = (path(&a), path(&b));
    let inside = |inner: &str, outer: &str| inner == outer || inner.starts_with(&format!("{outer}/"));
    a.scheme() == b.scheme()
        && a.host_str().is_some()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
        && (inside(&path_a, &path_b) || inside(&path_b, &path_a))
}

/// The saved keys that may be used for `target`, as references.
pub fn saved_api_keys(state: &AppState, target: KeyTarget) -> AppResult<Vec<SavedKeyRef>> {
    let config = state.config.snapshot();
    saved_keys_for(&config, &target)
}

fn saved_keys_for(config: &AppConfig, target: &KeyTarget) -> AppResult<Vec<SavedKeyRef>> {
    // A URL still being typed matches nothing.
    let Ok(endpoint) = target.endpoint() else {
        return Ok(Vec::new());
    };
    let own = target.own_key();
    let mut found = Vec::new();
    for owner in owners(config) {
        if Some(&owner.key) != own.as_ref() && same_api(&owner.endpoint, &endpoint) && secrets::contains(&owner.key)? {
            found.push(SavedKeyRef {
                id: owner.key.account(),
                label: owner.label,
            });
        }
    }
    Ok(found)
}

/// The secret behind `key_ref`, refused unless it was saved for the same API as `endpoint`.
pub(crate) fn resolve(config: &AppConfig, key_ref: &str, endpoint: &str) -> AppResult<String> {
    let owner = owners(config)
        .into_iter()
        .find(|owner| owner.key.account() == key_ref)
        .ok_or_else(|| AppError::new("The saved key you picked no longer exists; type the key"))?;
    if !same_api(&owner.endpoint, endpoint) {
        return Err(AppError::new(format!(
            "The key saved for {} belongs to another endpoint and is not sent to {}; type the key",
            owner.label,
            host(endpoint)
        )));
    }
    secrets::get(&owner.key)?.ok_or_else(|| AppError::new(format!("No key is saved for {} any more; type the key", owner.label)))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{ai::AiConnection, embedding::EmbeddingSettings};

    pub(crate) fn config() -> AppConfig {
        let mut config = AppConfig::default();
        config.embedding = Some(EmbeddingSettings {
            url: "https://api.openai.com/v1/embeddings".into(),
            model: "text-embedding-3-large".into(),
            auth: EmbeddingAuth::Bearer,
            dimensions: None,
        });
        config.ai_connections = vec![
            AiConnection {
                id: "work".into(),
                name: "Work OpenAI".into(),
                provider: AiProvider::Openai,
                base_url: String::new(),
                default_model: "gpt-5".into(),
            },
            AiConnection {
                id: "local".into(),
                name: "Ollama".into(),
                provider: AiProvider::Custom,
                base_url: "http://localhost:11434/v1".into(),
                default_model: "llama".into(),
            },
        ];
        config
    }

    #[test]
    fn same_api_needs_the_same_origin_and_a_shared_path() {
        assert!(same_api("https://api.openai.com/v1", "https://api.openai.com/v1/embeddings"));
        assert!(same_api("https://api.openai.com/v1/embeddings", "https://API.openai.com:443/v1/"));
        assert!(same_api("https://gw.example.com", "https://gw.example.com/team/v1"));
        for other in [
            "https://api.openai.com.evil.com/v1",
            "http://api.openai.com/v1",
            "https://api.openai.com:8443/v1",
            "https://api.openai.com/v10",
            "https://evil.com/api.openai.com/v1",
            "not a url",
        ] {
            assert!(!same_api("https://api.openai.com/v1", other), "{other}");
        }
    }

    #[test]
    fn only_keys_for_the_same_api_are_offered() {
        let config = config();
        let _store = secrets::testing::memory_store(&[("app:embedding", "emb"), ("ai:work", "sk"), ("ai:local", "ol")]);
        let embedding = |url: &str| KeyTarget::Embedding { url: url.into() };
        let ids = |target: &KeyTarget| -> Vec<String> { saved_keys_for(&config, target).unwrap().into_iter().map(|key| key.id).collect() };
        assert_eq!(
            ids(&embedding("https://api.openai.com/v1/embeddings")),
            ["app:embedding", "ai:work"]
        );
        assert_eq!(ids(&embedding("http://localhost:11434/v1")), ["ai:local"]);
        assert!(ids(&embedding("https://evil.example.com/v1/embeddings")).is_empty());
        assert!(ids(&embedding("https://api.open")).is_empty(), "a URL still being typed");
        let ai = KeyTarget::Ai {
            provider: AiProvider::Openai,
            base_url: String::new(),
            connection_id: Some("work".into()),
        };
        assert_eq!(ids(&ai), ["app:embedding"], "its own key is not offered");
        let labels = saved_keys_for(&config, &embedding("https://api.openai.com/v1")).unwrap();
        assert_eq!(labels[1].label, "Work OpenAI (api.openai.com)");
    }

    #[test]
    fn a_key_is_only_offered_while_it_is_saved() {
        let config = config();
        let _store = secrets::testing::memory_store(&[("ai:work", "sk")]);
        let target = KeyTarget::Embedding {
            url: "https://api.openai.com/v1/embeddings".into(),
        };
        let ids: Vec<String> = saved_keys_for(&config, &target).unwrap().into_iter().map(|key| key.id).collect();
        assert_eq!(ids, ["ai:work"]);
    }

    #[test]
    fn a_reference_resolves_only_for_the_same_api() {
        let config = config();
        let _store = secrets::testing::memory_store(&[("ai:work", "sk"), ("conn:c1:password", "pw")]);
        assert_eq!(resolve(&config, "ai:work", "https://api.openai.com/v1/embeddings").unwrap(), "sk");
        let refused = resolve(&config, "ai:work", "https://evil.example.com/v1/embeddings").unwrap_err();
        assert!(refused.to_string().contains("another endpoint"), "{refused}");
        assert!(resolve(&config, "ai:missing", "https://api.openai.com/v1").is_err());
        assert!(
            resolve(&config, "conn:c1:password", "https://api.openai.com/v1").is_err(),
            "only API keys"
        );
        assert!(resolve(&config, "app:embedding", "https://api.openai.com/v1").is_err(), "not saved");
    }
}
