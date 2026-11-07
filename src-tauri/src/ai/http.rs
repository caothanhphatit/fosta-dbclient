//! HTTP plumbing shared by both wire formats.

use std::time::Duration;

use serde_json::Value;

use crate::error::{AppError, AppResult};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const ERROR_PREVIEW: usize = 500;

pub(super) fn http_client() -> AppResult<reqwest::Client> {
    Ok(reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).build()?)
}

pub(super) async fn send(request: reqwest::RequestBuilder, label: &str) -> AppResult<reqwest::Response> {
    let response = request
        .send()
        .await
        .map_err(|error| AppError::new(format!("{label} API: {error}")))?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.pointer("/message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(ERROR_PREVIEW).collect());
    Err(AppError::new(format!("{label} API returned {status}: {detail}")))
}

/// Reads a Server-Sent Events body and hands every `data:` payload to `on_data`.
pub(super) async fn read_sse(mut response: reqwest::Response, mut on_data: impl FnMut(&str) -> AppResult<()>) -> AppResult<()> {
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let chunk = response.chunk().await?;
        let Some(chunk) = chunk else { break };
        buffer.extend_from_slice(&chunk);
        while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=position).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(payload) = line.strip_prefix("data:") {
                let payload = payload.trim();
                if !payload.is_empty() && payload != "[DONE]" {
                    on_data(payload)?;
                }
            }
        }
    }
    Ok(())
}
