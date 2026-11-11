use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const ERROR_PREVIEW: usize = 400;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingAuth {
    #[default]
    Bearer,
    ApiKey,
    None,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingSettings {
    pub url: String,
    pub model: String,
    #[serde(default)]
    pub auth: EmbeddingAuth,
    #[serde(default)]
    pub dimensions: Option<u32>,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// The URL requests go to. The setting is meant to be the full `/embeddings` URL, but the assistant's settings take a
/// base URL (`https://api.openai.com/v1`), and a base URL typed here used to be POSTed as it was: the provider answered
/// 404. A path that ends at an API root (a version such as `v1` or `v1beta`, or `openai`) gets `/embeddings` added;
/// any other path (Azure's `/deployments/<name>/embeddings`, a gateway's own route) is used unchanged.
pub fn endpoint(url: &str) -> String {
    let url = url.trim();
    let (path, query) = url.split_once('?').map_or((url, None), |(path, query)| (path, Some(query)));
    let path = path.trim_end_matches('/');
    let route = path
        .split_once("://")
        .map_or(path, |(_, rest)| rest)
        .split_once('/')
        .map_or("", |(_, route)| route);
    let last = route.rsplit('/').next().unwrap_or("");
    let version = last.len() > 1
        && last.starts_with(['v', 'V'])
        && last[1..].starts_with(|c: char| c.is_ascii_digit())
        && last.chars().all(|c| c.is_ascii_alphanumeric());
    let mut sent = if version || last.eq_ignore_ascii_case("openai") {
        format!("{path}/embeddings")
    } else {
        path.to_owned()
    };
    if let Some(query) = query {
        sent.push('?');
        sent.push_str(query);
    }
    sent
}

/// Whether `url` points at this machine (`localhost`, a `*.localhost` name, a loopback address such as `127.0.0.1` or
/// `::1`): a local server such as Ollama that may need no key, so the settings may be saved without one.
pub fn is_local(url: &str) -> bool {
    let Some(host) = reqwest::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
    else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => {
            let name = host.trim_end_matches('.');
            name == "localhost" || name.ends_with(".localhost")
        }
    }
}

/// The error for a failed request. Without a key some providers answer 404 or 400 instead of 401, so the message says
/// when none was sent.
fn status_error(status: reqwest::StatusCode, key_sent: bool, body: &str) -> AppError {
    let preview: String = body.chars().take(ERROR_PREVIEW).collect();
    let no_key = if key_sent { "" } else { " (no API key was sent)" };
    AppError::new(format!("Embedding API returned {status}{no_key}: {preview}"))
}

pub async fn embed(settings: &EmbeddingSettings, api_key: Option<String>, text: &str) -> AppResult<Vec<f32>> {
    let mut body = serde_json::json!({ "model": settings.model, "input": text });
    if let Some(dimensions) = settings.dimensions {
        body["dimensions"] = dimensions.into();
    }
    let client = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build()?;
    let request = client.post(endpoint(&settings.url)).json(&body);
    let (request, key_sent) = match (settings.auth, api_key.filter(|key| !key.is_empty())) {
        (EmbeddingAuth::Bearer, Some(key)) => (request.bearer_auth(key), true),
        (EmbeddingAuth::ApiKey, Some(key)) => (request.header("api-key", key), true),
        _ => (request, false),
    };
    let response = request.send().await?;
    let status = response.status();
    let payload = response.text().await?;
    if !status.is_success() {
        return Err(status_error(status, key_sent, &payload));
    }
    let parsed: EmbeddingResponse = serde_json::from_str(&payload).map_err(|error| {
        AppError::new(format!(
            "Unexpected embedding response ({error}). Expected an OpenAI-compatible body with data[0].embedding."
        ))
    })?;
    parsed
        .data
        .into_iter()
        .next()
        .map(|data| data.embedding)
        .ok_or_else(|| AppError::new("The embedding API returned no vectors"))
}

#[cfg(test)]
mod tests {
    use super::{endpoint, is_local, status_error};

    #[test]
    fn only_this_machine_is_local() {
        for local in [
            "http://localhost:11434/v1",
            "http://LOCALHOST/v1",
            "http://ollama.localhost:8080/v1",
            "http://127.0.0.1:11434/v1",
            "http://127.1.2.3/v1",
            "http://[::1]:11434/v1",
        ] {
            assert!(is_local(local), "{local}");
        }
        for remote in [
            "https://api.openai.com/v1",
            "http://localhost.evil.com/v1",
            "http://192.168.1.10:11434/v1",
            "http://0.0.0.0/v1",
            "not a url",
        ] {
            assert!(!is_local(remote), "{remote}");
        }
    }

    #[test]
    fn a_failure_without_a_key_says_so() {
        let error = status_error(reqwest::StatusCode::NOT_FOUND, false, "Not found").to_string();
        assert!(error.contains("404 Not Found (no API key was sent)"), "{error}");
        let error = status_error(reqwest::StatusCode::UNAUTHORIZED, true, "bad key").to_string();
        assert!(!error.contains("no API key"), "{error}");
        assert!(error.contains("401"), "{error}");
    }

    #[test]
    fn a_base_url_gets_the_embeddings_route() {
        // These were POSTed as they were, and the provider answered 404.
        for (typed, sent) in [
            ("https://api.openai.com/v1", "https://api.openai.com/v1/embeddings"),
            ("https://api.openai.com/v1/", "https://api.openai.com/v1/embeddings"),
            ("https://admin.fosta.app/v1", "https://admin.fosta.app/v1/embeddings"),
            ("http://localhost:11434/v1", "http://localhost:11434/v1/embeddings"),
            (
                "https://generativelanguage.googleapis.com/v1beta/openai",
                "https://generativelanguage.googleapis.com/v1beta/openai/embeddings",
            ),
        ] {
            assert_eq!(endpoint(typed), sent);
        }
        // A full URL is left alone: no doubled route, no lost query.
        for full in [
            "https://api.openai.com/v1/embeddings",
            "https://x.openai.azure.com/openai/deployments/emb/embeddings?api-version=2024-02-01",
            "https://gateway.example.com/team/embed",
            "https://v1.example.com",
        ] {
            assert_eq!(endpoint(full), full);
        }
        assert_eq!(
            endpoint(" https://api.openai.com/v1/embeddings/ "),
            "https://api.openai.com/v1/embeddings"
        );
    }
}
