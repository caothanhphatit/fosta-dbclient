//! Chat assistant. Talks to an LLM API with the user's own key and streams
//! the reply to the UI. The model only sees metadata (table names, column
//! definitions) and what the user shares (editor text, errors, log lines);
//! it never runs anything. SQL it writes goes into the editor for the user
//! to run by hand.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio_util::sync::CancellationToken;

use crate::{
    db::TableDesign,
    drivers::DbManager,
    error::{AppError, AppResult},
    lock::lock,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_TOOL_ROUNDS: usize = 8;
const MAX_OUTPUT_TOKENS: u32 = 8192;
const MAX_TABLES_IN_PROMPT: usize = 400;
const MAX_DDL_CHARS: usize = 8000;
const MAX_EDITOR_CHARS: usize = 6000;
const ERROR_PREVIEW: usize = 500;
const ANTHROPIC_VERSION: &str = "2026-10-21";
const STOPPED: &str = "Stopped";

/// Where requests go. Every provider except Anthropic speaks the OpenAI
/// `chat/completions` wire format (Gemini through Google's compatibility endpoint).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiProvider {
    #[default]
    Fosta,
    Openai,
    Gemini,
    Anthropic,
    /// Any OpenAI-compatible endpoint the user names (Ollama, LM Studio, OpenRouter, …).
    Custom,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Wire {
    Anthropic,
    OpenAi,
}

impl AiProvider {
    fn wire(self) -> Wire {
        match self {
            AiProvider::Anthropic => Wire::Anthropic,
            _ => Wire::OpenAi,
        }
    }

    fn default_url(self) -> &'static str {
        match self {
            AiProvider::Fosta => "https://admin.fosta.app/v1",
            AiProvider::Openai => "https://api.openai.com/v1",
            AiProvider::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            AiProvider::Anthropic => "https://api.anthropic.com",
            AiProvider::Custom => "",
        }
    }

    fn label(self) -> &'static str {
        match self {
            AiProvider::Fosta => "Fosta",
            AiProvider::Openai => "OpenAI",
            AiProvider::Gemini => "Gemini",
            AiProvider::Anthropic => "Anthropic",
            AiProvider::Custom => "Model",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    #[serde(default)]
    pub provider: AiProvider,
    /// Empty means the provider's public endpoint.
    #[serde(default)]
    pub base_url: String,
    pub model: String,
}

impl AiSettings {
    fn base_url(&self) -> AppResult<String> {
        let trimmed = self.base_url.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
                return Err(AppError::new(format!("Endpoint must be a URL starting with http:// or https://, not \"{trimmed}\" (assistant settings)")));
            }
            return Ok(trimmed.to_owned());
        }
        let default = self.provider.default_url();
        if default.is_empty() {
            return Err(AppError::new("Enter the endpoint URL of your OpenAI-compatible server (assistant settings)"));
        }
        Ok(default.to_owned())
    }

    fn label(&self) -> &'static str {
        self.provider.label()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// What the UI knows about where the user is; all optional.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatContext {
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub connection_name: Option<String>,
    #[serde(default)]
    pub driver_label: Option<String>,
    #[serde(default)]
    pub server_version: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub editor_text: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// Free-form extra context, e.g. the tail of a startup task log.
    #[serde(default)]
    pub extra: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub id: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub context: ChatContext,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEvent {
    /// A piece of the reply, in order.
    Text { text: String },
    /// The model looked something up; shown as a muted line.
    Tool { label: String },
    Done,
}

/// In-flight requests, so the UI can stop one.
#[derive(Default)]
pub struct AiRuns {
    tokens: Mutex<HashMap<String, CancellationToken>>,
}

impl AiRuns {
    fn begin(&self, id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        lock(&self.tokens).insert(id.to_owned(), token.clone());
        token
    }

    fn finish(&self, id: &str) {
        lock(&self.tokens).remove(id);
    }

    pub fn cancel(&self, id: &str) {
        if let Some(token) = lock(&self.tokens).get(id) {
            token.cancel();
        }
    }
}

struct ToolCall {
    id: String,
    name: String,
    input: Value,
}

/// One model turn: the streamed text, any tool calls, and the assistant
/// message in the provider's own shape (to append to the history).
struct Turn {
    text: String,
    calls: Vec<ToolCall>,
    assistant: Value,
}

pub async fn chat(
    settings: &AiSettings,
    api_key: Option<String>,
    db: &DbManager,
    runs: &AiRuns,
    request: ChatRequest,
    events: Channel<ChatEvent>,
) -> AppResult<String> {
    let token = runs.begin(&request.id);
    let id = request.id.clone();
    let result = tokio::select! {
        _ = token.cancelled() => Err(AppError::new(STOPPED)),
        outcome = run(settings, api_key, db, request, &events) => outcome,
    };
    runs.finish(&id);
    let _ = events.send(ChatEvent::Done);
    result
}

/// Models the provider offers, from its `/models` listing (same shape on
/// every wire: `data[].id`). Gemini prefixes ids with `models/`, dropped here.
pub async fn list_models(settings: &AiSettings, api_key: Option<String>) -> AppResult<Vec<String>> {
    let client = http_client()?;
    let base = settings.base_url()?;
    let key = api_key.as_deref().unwrap_or_default();
    let request = match settings.provider.wire() {
        Wire::Anthropic => client
            .get(format!("{base}/v1/models?limit=1000"))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("x-api-key", key),
        Wire::OpenAi => {
            let request = client.get(format!("{base}/models"));
            if key.is_empty() { request } else { request.bearer_auth(key) }
        }
    };
    let response = send(request, settings.label()).await?;
    let payload: Value = response.json().await?;
    let items = payload["data"]
        .as_array()
        .or_else(|| payload["models"].as_array())
        .or_else(|| payload.as_array())
        .ok_or_else(|| AppError::new(format!("{} returned no model list", settings.label())))?;
    let mut models: Vec<String> = items
        .iter()
        .filter_map(|item| item["id"].as_str().or_else(|| item["name"].as_str()))
        .map(|id| id.strip_prefix("models/").unwrap_or(id).to_owned())
        .collect();
    models.sort_unstable();
    models.dedup();
    Ok(models)
}

/// "Save & test": one short exchange, no tools.
pub async fn probe(settings: &AiSettings, api_key: Option<String>) -> AppResult<String> {
    let client = http_client()?;
    let history = vec![json!({ "role": "user", "content": "Reply with the single word OK." })];
    let events = Channel::new(|_| Ok(()));
    let turn = stream_turn(&client, settings, api_key.as_deref(), "You are a connectivity test.", &history, &[], &events).await?;
    Ok(turn.text.trim().to_owned())
}

async fn run(
    settings: &AiSettings,
    api_key: Option<String>,
    db: &DbManager,
    request: ChatRequest,
    events: &Channel<ChatEvent>,
) -> AppResult<String> {
    if request.messages.is_empty() {
        return Err(AppError::new("Nothing to send"));
    }
    let client = http_client()?;
    let context = request.context;
    let system = system_prompt(db, &context).await;
    let tools = if context.connection_id.is_some() { tool_definitions() } else { Vec::new() };
    let mut history: Vec<Value> = request
        .messages
        .iter()
        .map(|message| json!({ "role": message.role, "content": message.content }))
        .collect();
    let mut transcript = String::new();

    for _ in 0..MAX_TOOL_ROUNDS {
        let turn = stream_turn(&client, settings, api_key.as_deref(), &system, &history, &tools, events).await?;
        transcript.push_str(&turn.text);
        if turn.calls.is_empty() {
            return Ok(transcript);
        }
        history.push(turn.assistant);
        let mut results = Vec::new();
        for call in &turn.calls {
            let _ = events.send(ChatEvent::Tool { label: describe_call(call, &context) });
            let outcome = run_tool(db, &context, call).await;
            results.push((call, outcome));
        }
        append_tool_results(settings.provider.wire(), &mut history, &results);
        transcript.push('\n');
    }
    Err(AppError::new("The model kept looking things up without answering. Try a more specific question."))
}

fn http_client() -> AppResult<reqwest::Client> {
    Ok(reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).build()?)
}

// --- Prompt ---------------------------------------------------------------

async fn system_prompt(db: &DbManager, context: &ChatContext) -> String {
    let mut text = String::from(
        "You are the assistant built into DB Studio, a desktop database client. You help the user write and understand queries for the database they have open.\n\n\
Rules:\n\
- You cannot run anything. The user runs queries themselves from the editor, so put every runnable query in its own fenced code block (```sql for SQL engines, ```json for MongoDB commands and pipelines) so it can be inserted as-is. Independent statements go in separate blocks.\n\
- Before writing a query against a table, look at its columns with describe_table unless the structure is already known from this conversation. Never guess column names.\n\
- Use the dialect of the connected engine and point out engine-specific syntax when it matters (LIMIT vs TOP / OFFSET FETCH, backticks vs brackets, date functions).\n\
- Write statements that change data or schema (UPDATE, DELETE, DROP, ALTER, TRUNCATE) only when asked. Say what they affect, keep a WHERE on the key, and suggest a SELECT to check first.\n\
- Answer in the language the user writes in. Keep prose short; the query is the answer. Do not repeat the user's question.\n\
- For MongoDB, write what the query tab runs: a database command object, e.g. { \"find\": \"orders\", \"filter\": {...}, \"limit\": 50 } or { \"aggregate\": \"orders\", \"pipeline\": [...], \"cursor\": {} }.\n\
- When asked for test data, produce INSERT statements (or insertMany commands) with realistic, varied values that respect the column types and keys.\n\
- When asked to translate between engines, keep the semantics and mention anything that has no direct equivalent.\n\n\
Context:\n",
    );
    match (&context.connection_id, &context.connection_name) {
        (Some(_), name) => {
            let name = name.as_deref().unwrap_or("connection");
            let engine = match (&context.driver_label, &context.server_version) {
                (Some(label), Some(version)) => format!("{label} {version}"),
                (Some(label), None) => label.clone(),
                _ => String::from("unknown engine"),
            };
            text.push_str(&format!("- Connection: {name} ({engine})\n"));
        }
        _ => text.push_str("- No connection is open. Write generic SQL and say which engine you assumed.\n"),
    }
    if let Some(database) = &context.database {
        text.push_str(&format!("- Database: {database}\n"));
        if let Some(listing) = table_listing(db, context, database).await {
            text.push_str(&listing);
        }
    }
    if let Some(table) = &context.table {
        text.push_str(&format!("- Table the user is looking at: {table}\n"));
    }
    if let Some(editor) = context.editor_text.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        text.push_str(&format!("- Current editor text:\n```\n{}\n```\n", truncate(editor, MAX_EDITOR_CHARS)));
    }
    if let Some(error) = context.last_error.as_deref().filter(|value| !value.trim().is_empty()) {
        text.push_str(&format!("- Last error from the server: {}\n", truncate(error, 1000)));
    }
    if let Some(extra) = context.extra.as_deref().filter(|value| !value.trim().is_empty()) {
        text.push_str(&format!("- {}\n", truncate(extra, MAX_EDITOR_CHARS)));
    }
    text
}

async fn table_listing(db: &DbManager, context: &ChatContext, database: &str) -> Option<String> {
    let driver = db.driver(context.connection_id.as_ref()?).ok()?;
    let tables = driver.list_tables(database).await.ok()?;
    if tables.is_empty() {
        return Some(format!("- {database} has no tables yet.\n"));
    }
    let shown: Vec<String> = tables
        .iter()
        .take(MAX_TABLES_IN_PROMPT)
        .map(|table| match table.rows {
            Some(rows) => format!("{} ({}, ~{rows} rows)", table.name, table.kind),
            None => format!("{} ({})", table.name, table.kind),
        })
        .collect();
    let more = if tables.len() > shown.len() { format!(" … and {} more", tables.len() - shown.len()) } else { String::new() };
    Some(format!("- Tables in {database}: {}{more}\n", shown.join(", ")))
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let head: String = text.chars().take(limit).collect();
    format!("{head}\n… (truncated)")
}

// --- Tools (metadata only) ------------------------------------------------

struct ToolSpec {
    name: &'static str,
    description: &'static str,
    schema: Value,
}

fn tool_definitions() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "list_databases",
            description: "Lists the databases (schemas) available on the current connection.",
            schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
        ToolSpec {
            name: "list_tables",
            description: "Lists tables and views (or collections) of a database with approximate row counts.",
            schema: json!({
                "type": "object",
                "properties": { "database": { "type": "string", "description": "Database name; defaults to the current one." } },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "describe_table",
            description: "Returns the columns (name, type, nullability, key, default) and the CREATE statement of one table or view. For MongoDB it returns the fields seen in a sample of documents and the indexes.",
            schema: json!({
                "type": "object",
                "properties": {
                    "table": { "type": "string", "description": "Table, view or collection name (schema.table for SQL Server)." },
                    "database": { "type": "string", "description": "Database name; defaults to the current one." }
                },
                "required": ["table"],
                "additionalProperties": false
            }),
        },
    ]
}

fn describe_call(call: &ToolCall, context: &ChatContext) -> String {
    let argument = |key: &str| call.input.get(key).and_then(Value::as_str).map(str::to_owned);
    let database = argument("database").or_else(|| context.database.clone()).unwrap_or_default();
    match call.name.as_str() {
        "list_databases" => String::from("Listed the databases"),
        "list_tables" => format!("Listed the tables of {database}"),
        "describe_table" => format!("Looked at {}", argument("table").unwrap_or_default()),
        other => format!("Called {other}"),
    }
}

async fn run_tool(db: &DbManager, context: &ChatContext, call: &ToolCall) -> Result<String, String> {
    let connection_id = context.connection_id.as_deref().ok_or("No connection is open")?;
    let driver = db.driver(connection_id).map_err(|error| error.to_string())?;
    let argument = |key: &str| call.input.get(key).and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned);
    let database = || argument("database").or_else(|| context.database.clone()).ok_or_else(|| String::from("No database selected; pass `database`."));
    let outcome = match call.name.as_str() {
        "list_databases" => driver.list_databases().await.map(|names| names.join("\n")),
        "list_tables" => {
            let database = database()?;
            driver.list_tables(&database).await.map(|tables| {
                tables
                    .iter()
                    .map(|table| match table.rows {
                        Some(rows) => format!("{}\t{}\t~{rows} rows", table.name, table.kind),
                        None => format!("{}\t{}", table.name, table.kind),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        "describe_table" => {
            let table = argument("table").ok_or("`table` is required")?;
            let database = database()?;
            driver.describe_table(&database, &table).await.map(|design| format_design(&table, &design))
        }
        other => return Err(format!("Unknown tool {other}")),
    };
    outcome.map_err(|error| error.to_string())
}

fn format_design(table: &str, design: &TableDesign) -> String {
    let mut text = format!("Columns of {table}:\n");
    for column in &design.columns {
        let nullable = if column.nullable { "NULL" } else { "NOT NULL" };
        let key = if column.key.is_empty() { String::new() } else { format!(" [{}]", column.key) };
        let default = match &column.default_value {
            Some(value) => format!(" default {value}"),
            None => String::new(),
        };
        let extra = if column.extra.is_empty() { String::new() } else { format!(" {}", column.extra) };
        let comment = if column.comment.is_empty() { String::new() } else { format!(" -- {}", column.comment) };
        text.push_str(&format!("- {} {} {nullable}{key}{default}{extra}{comment}\n", column.name, column.column_type));
    }
    if !design.ddl.trim().is_empty() {
        text.push_str("\nDefinition:\n");
        text.push_str(&truncate(&design.ddl, MAX_DDL_CHARS));
    }
    text
}

fn append_tool_results(wire: Wire, history: &mut Vec<Value>, results: &[(&ToolCall, Result<String, String>)]) {
    match wire {
        Wire::Anthropic => {
            let blocks: Vec<Value> = results
                .iter()
                .map(|(call, outcome)| match outcome {
                    Ok(text) => json!({ "type": "tool_result", "tool_use_id": call.id, "content": text }),
                    Err(error) => json!({ "type": "tool_result", "tool_use_id": call.id, "content": error, "is_error": true }),
                })
                .collect();
            history.push(json!({ "role": "user", "content": blocks }));
        }
        Wire::OpenAi => {
            for (call, outcome) in results {
                let content = match outcome {
                    Ok(text) => text.clone(),
                    Err(error) => format!("Error: {error}"),
                };
                history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
            }
        }
    }
}

// --- Wire formats ---------------------------------------------------------

async fn stream_turn(
    client: &reqwest::Client,
    settings: &AiSettings,
    api_key: Option<&str>,
    system: &str,
    history: &[Value],
    tools: &[ToolSpec],
    events: &Channel<ChatEvent>,
) -> AppResult<Turn> {
    match settings.provider.wire() {
        Wire::Anthropic => anthropic_turn(client, settings, api_key, system, history, tools, events).await,
        Wire::OpenAi => {
            match openai_turn(client, settings, api_key, system, history, tools, events).await {
                // Local models often reject tool definitions; answer without lookups then.
                Err(error) if !tools.is_empty() && mentions_tools(&error) => {
                    openai_turn(client, settings, api_key, system, history, &[], events).await
                }
                outcome => outcome,
            }
        }
    }
}

fn mentions_tools(error: &AppError) -> bool {
    let text = error.to_string().to_lowercase();
    text.contains("400") && (text.contains("tool") || text.contains("function"))
}

async fn send(request: reqwest::RequestBuilder, label: &str) -> AppResult<reqwest::Response> {
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
async fn read_sse(mut response: reqwest::Response, mut on_data: impl FnMut(&str) -> AppResult<()>) -> AppResult<()> {
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

enum Block {
    Text(String),
    ToolUse { id: String, name: String, json: String },
}

async fn anthropic_turn(
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
                let input: Value = if json.trim().is_empty() { json!({}) } else { serde_json::from_str(&json).unwrap_or(json!({})) };
                content.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
                calls.push(ToolCall { id, name, input });
            }
        }
    }
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "(no reply)" }));
    }
    Ok(Turn { text, calls, assistant: json!({ "role": "assistant", "content": content }) })
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    arguments: String,
}

async fn openai_turn(
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
        let Some(delta) = event.pointer("/choices/0/delta") else { return Ok(()) };
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
        let input: Value = if slot.arguments.trim().is_empty() { json!({}) } else { serde_json::from_str(&slot.arguments).unwrap_or(json!({})) };
        raw_calls.push(json!({ "id": id, "type": "function", "function": { "name": slot.name, "arguments": input.to_string() } }));
        calls.push(ToolCall { id, name: slot.name, input });
    }
    let mut assistant = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text.clone()) } });
    if !raw_calls.is_empty() {
        assistant["tool_calls"] = Value::Array(raw_calls);
    }
    Ok(Turn { text, calls, assistant })
}
