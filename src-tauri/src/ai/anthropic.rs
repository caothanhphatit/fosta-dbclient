//! Anthropic Messages API (`/v1/messages`) with streaming and tool use.

use serde_json::{json, Value};
use tauri::ipc::Channel;

use super::{
    http::{read_sse, send},
    tools::ToolSpec,
    AiSettings, ChatEvent, ToolCall, Turn, ANTHROPIC_VERSION,
};
use crate::error::{AppError, AppResult};

const MAX_OUTPUT_TOKENS: u32 = 8192;

enum Block {
    Text(String),
    ToolUse { id: String, name: String, json: String },
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
    let mut body = json!({
        "model": settings.model,
        "max_tokens": MAX_OUTPUT_TOKENS,
        "stream": true,
        "system": [{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }],
        "messages": history,
    });
    if !tools.is_empty() {
        body["tools"] = tools
            .iter()
            .map(|tool| json!({ "name": tool.name, "description": tool.description, "input_schema": tool.schema }))
            .collect();
    }
    let request = client
        .post(format!("{}/v1/messages", settings.base_url()?))
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("x-api-key", api_key.unwrap_or_default())
        .json(&body);
    let response = send(request, settings.label()).await?;

    let mut blocks: Vec<Block> = Vec::new();
    let mut text = String::new();
    read_sse(response, |payload| {
        let event: Value = serde_json::from_str(payload)?;
        match event["type"].as_str().unwrap_or_default() {
            "content_block_start" => {
                let block = &event["content_block"];
                blocks.push(match block["type"].as_str().unwrap_or_default() {
                    "tool_use" => Block::ToolUse {
                        id: block["id"].as_str().unwrap_or_default().to_owned(),
                        name: block["name"].as_str().unwrap_or_default().to_owned(),
                        json: String::new(),
                    },
                    _ => Block::Text(block["text"].as_str().unwrap_or_default().to_owned()),
                });
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                match (delta["type"].as_str().unwrap_or_default(), blocks.last_mut()) {
                    ("text_delta", Some(Block::Text(current))) => {
                        let piece = delta["text"].as_str().unwrap_or_default();
                        current.push_str(piece);
                        text.push_str(piece);
                        let _ = events.send(ChatEvent::Text { text: piece.to_owned() });
                    }
                    ("input_json_delta", Some(Block::ToolUse { json, .. })) => {
                        json.push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            "error" => {
                let message = event.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error");
                return Err(AppError::new(format!("Anthropic API: {message}")));
            }
            _ => {}
        }
        Ok(())
    })
    .await?;

    let mut calls = Vec::new();
    let mut content = Vec::new();
    for block in blocks {
        match block {
            Block::Text(value) if !value.is_empty() => content.push(json!({ "type": "text", "text": value })),
            Block::Text(_) => {}
            Block::ToolUse { id, name, json } => {
                let input: Value = if json.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&json).unwrap_or(json!({}))
                };
                content.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
                calls.push(ToolCall { id, name, input });
            }
        }
    }
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "(no reply)" }));
    }
    Ok(Turn {
        text,
        calls,
        assistant: json!({ "role": "assistant", "content": content }),
    })
}
