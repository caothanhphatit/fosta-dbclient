//! Chat assistant. Talks to an LLM API with the user's own key and streams
//! the reply to the UI. The model sees metadata (table names, column
//! definitions), what the user shares (editor text, errors, log lines) and,
//! where the user allows it, the rows of read-only queries it runs itself
//! (`run_read_query`, through `gateway::assistant`). Anything else it writes
//! goes into a code block for the user to insert or run.
//!
//! - this module: settings types, the chat loop and model listing
//! - [`prompt`]: the system prompt built from the UI context
//! - [`tools`]: the read-only metadata tools, the read-only query tool and the "open a view" actions
//! - [`anthropic`] / [`openai`]: the two streaming wire formats
//! - [`http`]: HTTP client, error mapping and Server-Sent Events parsing

mod anthropic;
pub(crate) mod catalog;
mod collections;
mod http;
mod openai;
pub mod profile;
mod prompt;
mod tools;

pub(crate) use tools::{framed, metadata_tool};

use std::{collections::HashMap, sync::Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use tokio_util::sync::CancellationToken;

use self::{
    http::{http_client, send},
    prompt::system_prompt,
    tools::{append_tool_results, describe_call, run_tool, tool_definitions, ToolSpec},
};
use crate::{
    error::{AppError, AppResult},
    gateway,
    lock::lock,
    runtime::ConnectionManager,
};

const MAX_TOOL_ROUNDS: usize = 8;
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

    /// The name the provider cards show; used to name a migrated connection.
    pub fn display_name(self) -> &'static str {
        match self {
            AiProvider::Anthropic => "Claude",
            AiProvider::Custom => "Custom",
            other => other.label(),
        }
    }
}

/// One saved LLM provider account, like a database connection. Its key is in
/// the secret store under `secrets::SecretKey::AiKey(id)`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub provider: AiProvider,
    /// Empty means the provider's public endpoint.
    #[serde(default)]
    pub base_url: String,
    pub default_model: String,
}

impl AiConnection {
    pub fn settings(&self, model: &str) -> AiSettings {
        AiSettings {
            provider: self.provider,
            base_url: self.base_url.clone(),
            model: model.to_owned(),
        }
    }

    /// Trims the editable fields and refuses an unusable connection.
    pub fn normalized(mut self) -> AppResult<Self> {
        self.name = self.name.trim().to_owned();
        self.default_model = self.default_model.trim().to_owned();
        self.base_url = self.base_url.trim().to_owned();
        if self.id.trim().is_empty() {
            return Err(AppError::new("Provider id is required"));
        }
        if self.name.is_empty() {
            return Err(AppError::new("Name is required"));
        }
        if self.default_model.is_empty() {
            return Err(AppError::new("Default model is required"));
        }
        self.settings(&self.default_model).base_url()?;
        Ok(self)
    }
}

/// The connection and model the chat uses.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiActive {
    pub connection_id: String,
    pub model: String,
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
    pub(crate) fn base_url(&self) -> AppResult<String> {
        let trimmed = self.base_url.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
                return Err(AppError::new(format!(
                    "Endpoint must be a URL starting with http:// or https://, not \"{trimmed}\" (assistant settings)"
                )));
            }
            return Ok(trimmed.to_owned());
        }
        let default = self.provider.default_url();
        if default.is_empty() {
            return Err(AppError::new(
                "Enter the endpoint URL of your OpenAI-compatible server (assistant settings)",
            ));
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
    /// Free-form extra context, e.g. the tail of a command log.
    #[serde(default)]
    pub extra: Option<String>,
    /// Documents sampled in the view (a collection workspace: what reaches the active stage), already cut by the UI.
    /// Row data: it reaches the model only where the user lets the assistant read data (the same gate as
    /// `run_read_query`); otherwise the prompt leaves it out.
    #[serde(default)]
    pub sampled_documents: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub id: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub context: ChatContext,
    /// LLM connection for this request; defaults to the active one.
    #[serde(default)]
    pub connection_id: Option<String>,
    /// Model for this request; defaults to the active (or the connection's default) model.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEvent {
    /// A piece of the reply, in order.
    Text {
        text: String,
    },
    /// The model looked something up; shown as a muted line.
    Tool {
        label: String,
    },
    /// Something the app should show (a diagram, a table, a prefilled query tab).
    /// It never changes data: queries open in the editor for the user to run.
    Action {
        action: UiAction,
    },
    /// The model ran a read-only query (`run_read_query`): the statement, the row count and the rows it was sent
    /// (cells as text, cut like the model saw them). Shown as an expandable tool call.
    #[serde(rename_all = "camelCase")]
    Query {
        statement: String,
        database: Option<String>,
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
        truncated: bool,
        elapsed_ms: u64,
        /// Set when the gateway refused the statement or the engine failed.
        error: Option<String>,
    },
    Done,
}

/// Where a chat run streams its [`ChatEvent`]s: a Tauri `Channel` on the desktop, the HTTP response on the
/// web server. Any `Fn(ChatEvent)` closure is one.
pub trait ChatSink: Send + Sync {
    fn send(&self, event: ChatEvent) -> AppResult<()>;
}

impl<F: Fn(ChatEvent) + Send + Sync> ChatSink for F {
    fn send(&self, event: ChatEvent) -> AppResult<()> {
        self(event);
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
// The variant names are the wire tags the UI matches on (`openDiagram`, …).
#[allow(clippy::enum_variant_names)]
pub enum UiAction {
    #[serde(rename_all = "camelCase")]
    OpenDiagram { database: String, focus_table: Option<String> },
    /// The filter is only pre-filled, marked as suggested by the assistant: nothing runs until
    /// the user applies it (the table view does not even load before that).
    #[serde(rename_all = "camelCase")]
    OpenTable {
        database: String,
        table: String,
        suggested_filter: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    OpenQuery { database: Option<String>, sql: String },
    /// A collection's workspace on `section` ("documents" or "aggregations"). The filter, sort and projection (or the
    /// pipeline stages) are only pre-filled, marked as suggested by the assistant: nothing runs until the user does.
    #[serde(rename_all = "camelCase")]
    OpenCollectionWorkspace {
        database: String,
        collection: String,
        section: String,
        suggested_filter: Option<String>,
        suggested_sort: Option<String>,
        suggested_project: Option<String>,
        suggested_pipeline: Option<Vec<String>>,
    },
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

/// Streams one reply. `reads` is the gateway context of the chat's connection when the user allows the assistant to run
/// read-only queries there (`gateway::assistant::reads_allowed`); `None` leaves the query tool out entirely.
pub async fn chat(
    settings: &AiSettings,
    api_key: Option<String>,
    db: &ConnectionManager,
    runs: &AiRuns,
    request: ChatRequest,
    reads: Option<gateway::Context>,
    events: &dyn ChatSink,
) -> AppResult<String> {
    let token = runs.begin(&request.id);
    let id = request.id.clone();
    // Stopping drops the run; a query the model started is cancelled and its session closed by `gateway::assistant`.
    let result = tokio::select! {
        _ = token.cancelled() => Err(AppError::new(STOPPED)),
        outcome = run(settings, api_key, db, request, reads.as_ref(), events) => outcome,
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
            if key.is_empty() {
                request
            } else {
                request.bearer_auth(key)
            }
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
    let events = |_: ChatEvent| {};
    let turn = stream_turn(
        &client,
        settings,
        api_key.as_deref(),
        "You are a connectivity test.",
        &history,
        &[],
        &events,
    )
    .await?;
    Ok(turn.text.trim().to_owned())
}

async fn run(
    settings: &AiSettings,
    api_key: Option<String>,
    db: &ConnectionManager,
    request: ChatRequest,
    reads: Option<&gateway::Context>,
    events: &dyn ChatSink,
) -> AppResult<String> {
    if request.messages.is_empty() {
        return Err(AppError::new("Nothing to send"));
    }
    let client = http_client()?;
    let context = request.context;
    // The query tool only for the chat's own connection.
    let reads = reads.filter(|reads| context.connection_id.as_deref() == Some(reads.connection_id.as_str()));
    let system = system_prompt(db, &context, reads.is_some()).await;
    let tools = if context.connection_id.is_some() {
        tool_definitions(reads.is_some(), collections::has_collections(db, &context))
    } else {
        Vec::new()
    };
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
            let _ = events.send(ChatEvent::Tool {
                label: describe_call(call, &context),
            });
            let outcome = run_tool(db, &context, reads, call, events).await;
            results.push((call, outcome));
        }
        append_tool_results(settings.provider.wire(), &mut history, &results);
        transcript.push('\n');
    }
    Err(AppError::new(
        "The model kept looking things up without answering. Try a more specific question.",
    ))
}

/// One model turn on the provider's wire format.
async fn stream_turn(
    client: &reqwest::Client,
    settings: &AiSettings,
    api_key: Option<&str>,
    system: &str,
    history: &[Value],
    tools: &[ToolSpec],
    events: &dyn ChatSink,
) -> AppResult<Turn> {
    match settings.provider.wire() {
        Wire::Anthropic => anthropic::turn(client, settings, api_key, system, history, tools, events).await,
        Wire::OpenAi => {
            match openai::turn(client, settings, api_key, system, history, tools, events).await {
                // Local models often reject tool definitions; answer without lookups then.
                Err(error) if !tools.is_empty() && openai::mentions_tools(&error) => {
                    openai::turn(client, settings, api_key, system, history, &[], events).await
                }
                outcome => outcome,
            }
        }
    }
}

/// The assistant's tools without a model, for the contract suites (`contract::testing::suites::suite_assistant`).
#[cfg(test)]
pub(crate) mod testing {
    use serde_json::Value;

    use super::{tools, ChatContext, ChatEvent, ToolCall};
    use crate::{gateway, runtime::ConnectionManager};

    /// Runs tool `name` on `connection_id` as the model would; `reads` allows `run_read_query`.
    pub async fn tool(
        db: &ConnectionManager,
        connection_id: &str,
        database: Option<&str>,
        reads: Option<&gateway::Context>,
        name: &str,
        input: Value,
    ) -> Result<String, String> {
        let context = ChatContext {
            connection_id: Some(connection_id.to_owned()),
            database: database.map(str::to_owned),
            ..ChatContext::default()
        };
        let call = ToolCall {
            id: "suite".into(),
            name: name.to_owned(),
            input,
        };
        let events = |_: ChatEvent| {};
        tools::run_tool(db, &context, reads, &call, &events).await
    }
}
