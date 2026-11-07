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

pub async fn embed(settings: &EmbeddingSettings, api_key: Option<String>, text: &str) -> AppResult<Vec<f32>> {
    let mut body = serde_json::json!({ "model": settings.model, "input": text });
    if let Some(dimensions) = settings.dimensions {
        body["dimensions"] = dimensions.into();
    }
    let client = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build()?;
    let request = client.post(&settings.url).json(&body);
    let request = match (settings.auth, api_key) {
        (EmbeddingAuth::Bearer, Some(key)) => request.bearer_auth(key),
        (EmbeddingAuth::ApiKey, Some(key)) => request.header("api-key", key),
        _ => request,
    };
    let response = request.send().await?;
    let status = response.status();
    let payload = response.text().await?;
    if !status.is_success() {
        let preview: String = payload.chars().take(ERROR_PREVIEW).collect();
        return Err(AppError::new(format!("Embedding API returned {status}: {preview}")));
    }
    let parsed: EmbeddingResponse = serde_json::from_str(&payload)
        .map_err(|error| AppError::new(format!("Unexpected embedding response ({error}). Expected an OpenAI-compatible body with data[0].embedding.")))?;
    parsed
        .data
        .into_iter()
        .next()
        .map(|data| data.embedding)
        .ok_or_else(|| AppError::new("The embedding API returned no vectors"))
}
