//! The MCP server: lets AI clients (Claude Code, Claude Desktop, Cursor, VS Code, Zed) use saved connections through the
//! same rules as the app, without ever seeing a password. See `docs/mcp.md`.
//!
//! - Off by default (Settings → MCP server, [`McpSettings`]), and opt-in per workspace ([`McpAccess`]): a client only
//!   sees connections in workspaces that allow MCP; Production and Read-only workspaces allow at most read-only access.
//! - Transport ([`http`]): Streamable HTTP on `127.0.0.1` (desktop) or `/mcp` on the web server, both with a random
//!   MCP-only bearer token kept as a secret (`app:mcp-token`; on the web it never signs into the UI). [`bridge`]
//!   relays stdio for clients that only speak stdio.
//! - Protocol ([`protocol`]): initialize, ping, tools/list, tools/call. Tools ([`tools`]): catalog metadata,
//!   `run_read_query` and, where the workspace asks before writes, `run_query`, which waits for the user to approve
//!   it in the app ([`McpState::request_approval`]).
//! - Every call is audited with caller `mcp` and the client's name.

pub mod bridge;
pub mod http;
mod protocol;
mod tools;

#[cfg(test)]
mod tests;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::{
    config::AppConfig,
    error::{AppError, AppResult},
    events::EventEmitter,
    gateway::{
        rules::{Preset, SafetySettings, PRODUCTION_ICON},
        Context,
    },
    lock::lock,
    secrets::{self, SecretKey},
    AppState,
};

/// The default port of the desktop app's MCP server.
pub const DEFAULT_PORT: u16 = 3910;
/// How long `run_query` waits for the user before it is denied.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Sessions kept at once; the least recently used is dropped beyond this (its client initializes again).
pub(crate) const MAX_SESSIONS: usize = 64;
/// A session unused this long expires (its client initializes again).
pub const SESSION_IDLE: Duration = Duration::from_secs(30 * 60);
/// Approvals waiting at once; more are refused straight away.
pub(crate) const MAX_PENDING: usize = 16;

/// What MCP clients may do with a workspace's connections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McpAccess {
    /// Not visible to MCP clients (the default).
    #[default]
    Off,
    /// Catalog metadata and `run_read_query`.
    ReadOnly,
    /// Also `run_query`, each statement approved by the user in the app.
    AskBeforeWrites,
}

impl McpAccess {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ReadOnly => "read-only",
            Self::AskBeforeWrites => "ask before writes",
        }
    }
}

/// Settings → MCP server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct McpSettings {
    pub enabled: bool,
    /// Desktop: the port on 127.0.0.1. The web edition serves `/mcp` on its own port.
    pub port: u16,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
        }
    }
}

/// Production and Read-only rules allow at most read-only access, whatever the workspace setting says.
pub fn capped(level: McpAccess, icon: &str, safety: &SafetySettings) -> McpAccess {
    let read_only = icon == PRODUCTION_ICON || safety.preset == Preset::ReadOnly || safety.effective().read_only();
    if read_only {
        level.min(McpAccess::ReadOnly)
    } else {
        level
    }
}

/// What MCP clients may do with a saved connection: its workspace's choice, capped by the workspace's and the
/// connection's own rules. `Off` for a connection that does not exist.
pub fn connection_access(config: &AppConfig, connection_id: &str) -> McpAccess {
    let Some((workspace, _)) = config.connection_scope(connection_id) else {
        return McpAccess::Off;
    };
    let Ok(context) = Context::for_connection(config, connection_id) else {
        return McpAccess::Off;
    };
    let level = capped(workspace.mcp, &workspace.icon, &workspace.safety());
    capped(level, &workspace.icon, &context.safety)
}

/// The client behind an MCP session, as it introduced itself in `initialize` (self-reported, so only a label).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    /// `clientInfo.name`, cleaned (e.g. `claude-code`).
    pub name: String,
    /// What the app shows: "Claude Code" for `claude-code`, else the name.
    pub label: String,
    pub version: String,
}

impl ClientInfo {
    pub fn new(name: &str, version: &str) -> Self {
        let name = clean(name, 60);
        let name = if name.is_empty() { "unknown".to_owned() } else { name };
        Self {
            label: client_label(&name),
            version: clean(version, 30),
            name,
        }
    }
}

/// Printable characters only, at most `max` of them.
fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Friendly names of well-known clients.
pub fn client_label(name: &str) -> String {
    let known = match name.to_ascii_lowercase().as_str() {
        "claude-code" => "Claude Code",
        "claude-ai" | "claude desktop" | "claude-desktop" => "Claude Desktop",
        "cursor" | "cursor-vscode" => "Cursor",
        "visual studio code" | "vscode" | "visual studio code - insiders" => "VS Code",
        "zed" => "Zed",
        "mcp-inspector" | "inspector-client" => "MCP Inspector",
        _ => return name.to_owned(),
    };
    known.to_owned()
}

struct Session {
    client: ClientInfo,
    protocol_version: String,
    last_used: Instant,
}

/// A `run_query` waiting for the user, as the app shows it (`mcp-approval` event, [`McpStatus::pending`]).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpApproval {
    pub id: String,
    /// The client's self-reported name and label.
    pub client: String,
    pub client_label: String,
    pub connection_id: String,
    pub connection: String,
    pub workspace: String,
    pub database: Option<String>,
    /// The full statement.
    pub statement: String,
    /// The gateway's `CONFIRMATION_REQUIRED` payload without its token (categories, statements, level, expected).
    pub problem: Value,
    /// Milliseconds left when it was sent.
    pub timeout_ms: u64,
}

struct Pending {
    approval: McpApproval,
    answer: oneshot::Sender<Answer>,
}

/// The user's answer to an approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Approved {
        typed: Option<String>,
    },
    Denied,
    TimedOut,
    /// Withdrawn because the server was turned off, its settings changed or its token was rotated.
    Withdrawn,
}

/// What Settings → MCP server shows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub enabled: bool,
    pub port: u16,
    /// Desktop: the server is listening. Web: the endpoint answers.
    pub running: bool,
    /// Why the server could not start (port in use…).
    pub error: Option<String>,
    /// The endpoint clients connect to (desktop: `http://127.0.0.1:<port>/mcp`; web: the path `/mcp`).
    pub url: String,
    /// Tool calls since the app started.
    pub calls: u64,
    /// The stdio bridge command for clients that only speak stdio (desktop: this executable with `mcp`).
    pub bridge_command: Vec<String>,
    /// Labels of the clients connected now.
    pub clients: Vec<String>,
    /// `run_query` statements waiting for the user.
    pub pending: Vec<McpApproval>,
}

struct Listener {
    port: u16,
    stop: oneshot::Sender<()>,
}

/// The MCP server's state, part of [`AppState`].
pub struct McpState {
    events: Arc<dyn EventEmitter>,
    executor: tokio::runtime::Handle,
    app: OnceLock<Weak<AppState>>,
    sessions: Mutex<HashMap<String, Session>>,
    pending: Mutex<HashMap<String, Pending>>,
    calls: AtomicU64,
    token: Mutex<Option<String>>,
    listener: Mutex<Option<Listener>>,
    error: Mutex<Option<String>>,
    approval_timeout: Mutex<Duration>,
    session_idle: Mutex<Duration>,
    /// Serializes connect-on-demand so two calls do not open the same connection twice.
    connecting: tokio::sync::Mutex<()>,
}

impl McpState {
    pub fn new(events: Arc<dyn EventEmitter>, executor: tokio::runtime::Handle) -> Self {
        Self {
            events,
            executor,
            app: OnceLock::new(),
            sessions: Mutex::default(),
            pending: Mutex::default(),
            calls: AtomicU64::new(0),
            token: Mutex::default(),
            listener: Mutex::default(),
            error: Mutex::default(),
            approval_timeout: Mutex::new(APPROVAL_TIMEOUT),
            session_idle: Mutex::new(SESSION_IDLE),
            connecting: tokio::sync::Mutex::new(()),
        }
    }

    /// Gives the server the app state it serves (once, right after the state is created).
    pub fn attach(&self, app: &Arc<AppState>) {
        let _ = self.app.set(Arc::downgrade(app));
    }

    pub(crate) fn app(&self) -> Option<Arc<AppState>> {
        self.app.get().and_then(Weak::upgrade)
    }

    #[cfg(test)]
    pub(crate) fn set_approval_timeout(&self, timeout: Duration) {
        *lock(&self.approval_timeout) = timeout;
    }

    #[cfg(test)]
    pub(crate) fn set_session_idle(&self, idle: Duration) {
        *lock(&self.session_idle) = idle;
    }

    #[cfg(test)]
    pub(crate) fn set_token(&self, token: &str) {
        *lock(&self.token) = Some(token.to_owned());
    }

    // --- Token -----------------------------------------------------------------------------

    /// The MCP bearer token (`app:mcp-token`, in Credential Manager on the desktop and in the encrypted secret store on
    /// the web): the saved one, or a new one saved now. On the web it is separate from the server's access token.
    pub fn token(&self) -> AppResult<String> {
        let mut cached = lock(&self.token);
        if let Some(token) = cached.as_ref() {
            return Ok(token.clone());
        }
        let token = match secrets::get(&SecretKey::McpToken)? {
            Some(token) if token.len() >= 32 => token,
            _ => {
                let token = new_token()?;
                secrets::set(&SecretKey::McpToken, &token)?;
                token
            }
        };
        *cached = Some(token.clone());
        Ok(token)
    }

    /// A new token: clients must be set up again; their sessions end and their pending approvals are withdrawn now.
    pub fn rotate_token(&self) -> AppResult<String> {
        let token = new_token()?;
        secrets::set(&SecretKey::McpToken, &token)?;
        *lock(&self.token) = Some(token.clone());
        self.end_all();
        Ok(token)
    }

    /// Whether `candidate` is the saved MCP token, without creating one when there is none (the web server's sign-in
    /// uses this to refuse it).
    pub fn is_token(&self, candidate: &str) -> bool {
        let cached = lock(&self.token).clone();
        let token = cached.or_else(|| secrets::get(&SecretKey::McpToken).ok().flatten());
        token.is_some_and(|token| token.len() >= 32 && constant_time_eq(token.as_bytes(), candidate.trim().as_bytes()))
    }

    /// Whether `candidate` is the MCP token (compared in constant time).
    pub(crate) fn token_matches(&self, candidate: &str) -> bool {
        match self.token() {
            Ok(token) => constant_time_eq(token.as_bytes(), candidate.trim().as_bytes()),
            Err(error) => {
                tracing::warn!(%error, "MCP token unavailable");
                false
            }
        }
    }

    // --- Sessions ------------------------------------------------------------------------

    /// Drops the sessions unused for longer than the idle timeout.
    fn expire_sessions(&self, sessions: &mut HashMap<String, Session>, now: Instant) {
        let idle = *lock(&self.session_idle);
        sessions.retain(|_, session| now.duration_since(session.last_used) < idle);
    }

    pub(crate) fn open_session(&self, client: ClientInfo, protocol_version: &str) -> AppResult<String> {
        let id = new_token()?;
        let now = Instant::now();
        let mut sessions = lock(&self.sessions);
        self.expire_sessions(&mut sessions, now);
        while sessions.len() >= MAX_SESSIONS {
            let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, session)| session.last_used)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            sessions.remove(&oldest);
        }
        tracing::info!(client = %client.name, version = %client.version, "MCP client connected");
        sessions.insert(
            id.clone(),
            Session {
                client,
                protocol_version: protocol_version.to_owned(),
                last_used: now,
            },
        );
        drop(sessions);
        self.emit_activity(None);
        Ok(id)
    }

    /// The session's client and protocol version; using a session keeps it alive. An expired session is gone.
    pub(crate) fn session(&self, id: &str) -> Option<(ClientInfo, String)> {
        let now = Instant::now();
        let mut sessions = lock(&self.sessions);
        self.expire_sessions(&mut sessions, now);
        let session = sessions.get_mut(id)?;
        session.last_used = now;
        Some((session.client.clone(), session.protocol_version.clone()))
    }

    #[cfg(test)]
    pub(crate) fn session_count(&self) -> usize {
        lock(&self.sessions).len()
    }

    /// Ends every session and withdraws every pending approval (server off, settings changed, token rotated).
    fn end_all(&self) {
        lock(&self.sessions).clear();
        self.withdraw_all();
        self.emit_activity(None);
    }

    pub(crate) fn close_session(&self, id: &str) -> bool {
        let removed = lock(&self.sessions).remove(id).is_some();
        if removed {
            self.emit_activity(None);
        }
        removed
    }

    // --- Activity --------------------------------------------------------------------------

    pub(crate) fn record_call(&self, client: &ClientInfo, tool: &str) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.emit_activity(Some((client, tool)));
    }

    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    fn emit_activity(&self, call: Option<(&ClientInfo, &str)>) {
        let clients = self.client_labels();
        self.events.emit(
            "mcp-activity",
            json!({
                "calls": self.calls(),
                "clients": clients,
                "client": call.map(|(client, _)| client.label.clone()),
                "tool": call.map(|(_, tool)| tool.to_owned()),
            }),
        );
    }

    fn client_labels(&self) -> Vec<String> {
        let mut sessions = lock(&self.sessions);
        self.expire_sessions(&mut sessions, Instant::now());
        let mut labels: Vec<String> = sessions.values().map(|session| session.client.label.clone()).collect();
        drop(sessions);
        labels.sort();
        labels.dedup();
        labels
    }

    // --- Approvals -------------------------------------------------------------------------

    /// Shows `approval` in every open window of the app and waits for the first answer, at most the approval timeout.
    /// Dropping the future (the client went away) withdraws it.
    pub async fn request_approval(&self, mut approval: McpApproval) -> AppResult<Answer> {
        let timeout = *lock(&self.approval_timeout);
        approval.id = new_token()?;
        approval.timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = lock(&self.pending);
            if pending.len() >= MAX_PENDING {
                return Err(AppError::new(
                    "Refused: too many statements are already waiting for the user's approval. Wait for an answer first.",
                ));
            }
            pending.insert(
                approval.id.clone(),
                Pending {
                    approval: approval.clone(),
                    answer: sender,
                },
            );
        }
        let guard = Withdraw {
            state: self,
            id: approval.id.clone(),
        };
        self.events.emit("mcp-approval", serde_json::to_value(&approval)?);
        let answer = match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Answer::Denied,
            Err(_) => Answer::TimedOut,
        };
        drop(guard);
        Ok(answer)
    }

    /// The user's answer from the app (`mcp_answer`). The first answer wins; later ones find nothing.
    pub fn answer(&self, id: &str, approve: bool, typed: Option<String>) -> AppResult<()> {
        let pending = lock(&self.pending).remove(id);
        let Some(pending) = pending else {
            return Err(AppError::new(
                "This request is no longer waiting: it was answered elsewhere or timed out.",
            ));
        };
        let answer = if approve { Answer::Approved { typed } } else { Answer::Denied };
        let _ = pending.answer.send(answer);
        self.events.emit("mcp-approval-closed", json!({ "id": id }));
        Ok(())
    }

    /// Answers every pending approval with [`Answer::Withdrawn`] and closes its dialogs.
    pub fn withdraw_all(&self) {
        let drained: Vec<(String, Pending)> = lock(&self.pending).drain().collect();
        for (id, pending) in drained {
            let _ = pending.answer.send(Answer::Withdrawn);
            self.events.emit("mcp-approval-closed", json!({ "id": id }));
        }
    }

    pub fn pending(&self) -> Vec<McpApproval> {
        let mut pending: Vec<McpApproval> = lock(&self.pending).values().map(|pending| pending.approval.clone()).collect();
        pending.sort_by(|a, b| a.id.cmp(&b.id));
        pending
    }

    // --- Status and the desktop listener -----------------------------------------------------

    pub fn status(&self, config: &AppConfig, web: bool) -> McpStatus {
        let settings = &config.mcp;
        let running = if web {
            settings.enabled
        } else {
            lock(&self.listener).as_ref().is_some_and(|listener| listener.port == settings.port)
        };
        let bridge_command = if web {
            vec!["fosta-studio-server".to_owned(), "mcp".to_owned()]
        } else {
            let exe = std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "fosta-studio".into());
            vec![exe, "mcp".to_owned()]
        };
        McpStatus {
            enabled: settings.enabled,
            port: settings.port,
            running,
            error: lock(&self.error).clone(),
            url: if web {
                "/mcp".to_owned()
            } else {
                format!("http://127.0.0.1:{}/mcp", settings.port)
            },
            calls: self.calls(),
            bridge_command,
            clients: self.client_labels(),
            pending: self.pending(),
        }
    }

    /// Matches the settings. Desktop: starts, restarts or stops the listener. Web (which serves `/mcp` from its own
    /// router): makes sure the MCP token exists. Turning the server off or changing its port ends every session and
    /// withdraws every pending approval.
    pub fn apply(&self) -> AppResult<()> {
        let Some(app) = self.app() else {
            return Ok(());
        };
        let settings = app.config.read(|config| config.mcp.clone());
        if app.capabilities.edition == "web" {
            if !settings.enabled {
                self.end_all();
                return Ok(());
            }
            return self.token().map(|_| ());
        }
        let mut listener = lock(&self.listener);
        if settings.enabled && listener.as_ref().is_some_and(|running| running.port == settings.port) {
            return Ok(());
        }
        if let Some(running) = listener.take() {
            let _ = running.stop.send(());
            tracing::info!(port = running.port, "MCP server stopped");
        }
        *lock(&self.error) = None;
        self.end_all();
        if !settings.enabled {
            return Ok(());
        }
        self.token()?;
        match http::serve_loopback(&self.executor, Arc::downgrade(&app), settings.port) {
            Ok((_, stop)) => {
                tracing::info!(port = settings.port, "MCP server listening on 127.0.0.1");
                *listener = Some(Listener { port: settings.port, stop });
                Ok(())
            }
            Err(error) => {
                *lock(&self.error) = Some(error.to_string());
                Err(error)
            }
        }
    }
}

/// Withdraws an approval when its waiting future ends or is dropped, and closes its dialogs.
struct Withdraw<'a> {
    state: &'a McpState,
    id: String,
}

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        if lock(&self.state.pending).remove(&self.id).is_some() {
            self.state.events.emit("mcp-approval-closed", json!({ "id": self.id }));
        }
    }
}

/// 32 random bytes as hex.
fn new_token() -> AppResult<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| AppError::new(format!("No secure random numbers: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0u8, |difference, (a, b)| difference | (a ^ b)) == 0
}
