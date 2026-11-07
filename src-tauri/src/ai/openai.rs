//! OpenAI `chat/completions` streaming wire format, used by every provider
//! except Anthropic (Gemini through Google's compatibility endpoint).

use serde_json::{json, Value};
use tauri::ipc::Channel;

use super::{
    http::{read_sse, send},
    tools::ToolSpec,
    AiSettings, ChatEvent, ToolCall, Turn,
};
use crate::error::{AppError, AppResult};

pub(super) fn mentions_tools(error: &AppError) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("400") && (text.contains("tool") || text.contains("function"))
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

pub(super) async fn turn(
    client: &reqwest::Client,
    settings: &AiSettings,
    api_key: Option<&str>,
    system: &str,
    history: &[Value],
    tools: &[ToolSpec],
    events: &Channel<ChatEvent>,
) -> AppResult<Turn> {
    let mut messages = vec![json!({ "role": "system", "content": system })];
    messages.extend(history.iter().cloned());
    let mut body = json!({ "model": settings.model, "stream": true, "messages": messages });
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|tool| json!({ "type": "function", "function": { "name": tool.name, "description": tool.description, "parameters": tool.schema } }))
            .collect();
    }
    let mut request = client.post(format!("{}/chat/completions", settings.base_url()?)).json(&body);
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
        request = request.bearer_auth(key);
    }
    let response = send(request, settings.label()).await?;

    let mut text = String::new();
    let mut pending: Vec<PendingCall> = Vec::new();
    read_sse(response, |payload| {
        let event: Value = serde_json::from_str(payload)?;
        if let Some(message) = event.pointer("/error/message").and_then(Value::as_str) {
            return Err(AppError::new(format!("{} API: {message}", settings.label())));
        }
        let Some(delta) = event.pointer("/choices/0/delta") else {
            return Ok(());
        };
        if let Some(piece) = delta["content"].as_str().filter(|piece| !piece.is_empty()) {
            text.push_str(piece);
            let _ = events.send(ChatEvent::Text { text: piece.to_owned() });
        }
        if let Some(parts) = delta["tool_calls"].as_array() {
            for part in parts {
                let index = part["index"].as_u64().unwrap_or(0) as usize;
                while pending.len() <= index {
                    pending.push(PendingCall::default());
                }
                let slot = &mut pending[index];
                if let Some(id) = part["id"].as_str() {
                    slot.id = id.to_owned();
                }
                if let Some(name) = part.pointer("/function/name").and_then(Value::as_str) {
                    slot.name.push_str(name);
                }
                if let Some(arguments) = part.pointer("/function/arguments").and_then(Value::as_str) {
                    slot.arguments.push_str(arguments);
                }
            }
        }
        Ok(())
    })
    .await?;

    let mut calls = Vec::new();
    let mut raw_calls = Vec::new();
    for (index, slot) in pending.into_iter().enumerate() {
        if slot.name.is_empty() {
            continue;
        }
        let id = if slot.id.is_empty() { format!("call_{index}") } else { slot.id };
        let input: Value = if slot.arguments.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&slot.arguments).unwrap_or(json!({}))
        };
        raw_calls.push(json!({ "id": id, "type": "function", "function": { "name": slot.name, "arguments": input.to_string() } }));
        calls.push(ToolCall {
            id,
            name: slot.name,
            input,
        });
    }
    let mut assistant = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text.clone()) } });
    if !raw_calls.is_empty() {
        assistant["tool_calls"] = Value::Array(raw_calls);
    }
    Ok(Turn { text, calls, assistant })
}
