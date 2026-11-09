//! The stdio bridge (`fosta-studio mcp`, `fosta-studio-server mcp`) for MCP clients that only start a command (Claude
//! Desktop): newline-delimited JSON-RPC on stdin/stdout, relayed to the running app's HTTP endpoint with its token.
//! The bridge itself holds no connection and runs no tool; when the app is not running, every request gets a clear
//! JSON-RPC error. When the MCP server is off in the settings it exits at once without sending the token anywhere.
//! Only JSON-RPC goes to stdout; diagnostics go to stderr.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
    task::JoinSet,
};

use super::http::{SESSION_HEADER, VERSION_HEADER};
use crate::{
    config::{self, AppConfig},
    lock::lock,
    secrets::{self, SecretKey},
};

/// Refuses to relay when the MCP server is off in the settings: the bridge then sends nothing, the token included.
pub fn require_enabled(config: &AppConfig) -> Result<(), String> {
    if config.mcp.enabled {
        Ok(())
    } else {
        Err("the MCP server is off; turn it on in Fosta Studio (Settings → MCP server), then restart this client.".to_owned())
    }
}

/// `fosta-studio mcp`: the desktop app's endpoint and token, from its settings and Credential Manager.
pub fn run_desktop() -> i32 {
    let config = match config::read_saved() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("fosta-studio mcp: cannot read the settings: {error}");
            return 1;
        }
    };
    if let Err(error) = require_enabled(&config) {
        eprintln!("fosta-studio mcp: {error}");
        return 1;
    }
    let token = match secrets::get(&SecretKey::McpToken) {
        Ok(Some(token)) => token,
        Ok(None) => {
            eprintln!("fosta-studio mcp: no MCP token yet; start Fosta Studio with Settings → MCP server on.");
            return 1;
        }
        Err(error) => {
            eprintln!("fosta-studio mcp: cannot read the MCP token: {error}");
            return 1;
        }
    };
    run(format!("http://127.0.0.1:{}/mcp", config.mcp.port), token)
}

/// Relays stdin to `endpoint` until stdin closes. Returns the process exit code.
pub fn run(endpoint: String, token: String) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("fosta-studio mcp: {error}");
            return 1;
        }
    };
    runtime.block_on(relay(endpoint, token))
}

pub(super) struct Relay {
    client: reqwest::Client,
    endpoint: String,
    token: String,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    /// The client's `initialize`, replayed when the app restarted and forgot the session.
    initialize: Mutex<Option<Value>>,
}

async fn relay(endpoint: String, token: String) -> i32 {
    let relay = match Relay::new(endpoint, token) {
        Ok(relay) => Arc::new(relay),
        Err(error) => {
            eprintln!("fosta-studio mcp: {error}");
            return 1;
        }
    };
    let (output, mut lines_out) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = lines_out.recv().await {
            if stdout.write_all(format!("{line}\n").as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });
    let mut in_flight = JoinSet::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            let _ = output.send(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Not JSON" } }).to_string());
            continue;
        };
        if message["method"] == "initialize" {
            // Nothing else is sent before the answer, and later requests need the session it opens.
            relay.forward(message, &output).await;
        } else {
            let (relay, output) = (Arc::clone(&relay), output.clone());
            in_flight.spawn(async move { relay.forward(message, &output).await });
        }
    }
    while in_flight.join_next().await.is_some() {}
    relay.end_session().await;
    drop(output);
    let _ = writer.await;
    0
}

impl Relay {
    pub(super) fn new(endpoint: String, token: String) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().no_proxy().build()?,
            endpoint,
            token,
            session: Mutex::default(),
            version: Mutex::default(),
            initialize: Mutex::default(),
        })
    }

    /// The session the app gave this bridge, if any.
    #[cfg(test)]
    pub(super) fn session(&self) -> Option<String> {
        lock(&self.session).clone()
    }

    async fn post(&self, message: &Value, with_session: bool) -> Result<reqwest::Response, reqwest::Error> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
            .json(message);
        if with_session {
            if let Some(session) = lock(&self.session).clone() {
                request = request.header(SESSION_HEADER, session);
            }
            if let Some(version) = lock(&self.version).clone() {
                request = request.header(VERSION_HEADER, version);
            }
        }
        request.send().await
    }

    /// Sends one message and writes what comes back; failures become JSON-RPC errors for requests.
    pub(super) async fn forward(&self, message: Value, output: &mpsc::UnboundedSender<String>) {
        let id = message.get("id").cloned().filter(|_| message.get("method").is_some());
        let initialize = message["method"] == "initialize";
        if initialize {
            *lock(&self.initialize) = Some(message.clone());
        }
        let mut retried = false;
        loop {
            let response = match self.post(&message, !initialize).await {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("fosta-studio mcp: {error}");
                    self.fail(
                        output,
                        id.as_ref(),
                        &format!(
                            "Cannot reach Fosta Studio at {}. Start it and turn on Settings → MCP server.",
                            self.endpoint
                        ),
                    );
                    return;
                }
            };
            let status = response.status();
            if status == reqwest::StatusCode::NOT_FOUND && !initialize && !retried && self.reinitialize().await {
                retried = true;
                continue;
            }
            if initialize {
                if let Some(session) = response.headers().get(SESSION_HEADER).and_then(|value| value.to_str().ok()) {
                    *lock(&self.session) = Some(session.to_owned());
                }
            }
            if status == reqwest::StatusCode::ACCEPTED {
                return;
            }
            let event_stream = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/event-stream"));
            let text = response.text().await.unwrap_or_default();
            let messages: Vec<Value> = if event_stream {
                text.lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .filter_map(|data| serde_json::from_str(data.trim()).ok())
                    .collect()
            } else {
                serde_json::from_str(&text).into_iter().collect()
            };
            let rpc: Vec<&Value> = messages.iter().filter(|value| value.get("jsonrpc").is_some()).collect();
            if rpc.is_empty() {
                let reason = messages
                    .first()
                    .and_then(|value| value["error"].as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Fosta Studio answered {status}"));
                let reason = if status == reqwest::StatusCode::UNAUTHORIZED {
                    format!("{reason}. Turn on Settings → MCP server in Fosta Studio; after rotating the token, restart this client.")
                } else {
                    reason
                };
                self.fail(output, id.as_ref(), &reason);
                return;
            }
            for value in rpc {
                if initialize {
                    if let Some(version) = value["result"]["protocolVersion"].as_str() {
                        *lock(&self.version) = Some(version.to_owned());
                    }
                }
                let _ = output.send(value.to_string());
            }
            return;
        }
    }

    fn fail(&self, output: &mpsc::UnboundedSender<String>, id: Option<&Value>, message: &str) {
        match id {
            Some(id) => {
                let _ = output.send(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": message } }).to_string());
            }
            None => eprintln!("fosta-studio mcp: {message}"),
        }
    }

    /// The app restarted (unknown session): initialize again with the client's own `initialize`, quietly.
    async fn reinitialize(&self) -> bool {
        let Some(initialize) = lock(&self.initialize).clone() else {
            return false;
        };
        let Ok(response) = self.post(&initialize, false).await else {
            return false;
        };
        let Some(session) = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
        else {
            return false;
        };
        *lock(&self.session) = Some(session);
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        self.post(&initialized, true).await.is_ok()
    }

    async fn end_session(&self) {
        if lock(&self.session).is_none() {
            return;
        }
        let mut request = self.client.delete(&self.endpoint).bearer_auth(&self.token);
        if let Some(session) = lock(&self.session).clone() {
            request = request.header(SESSION_HEADER, session);
        }
        let _ = request.send().await;
    }
}
