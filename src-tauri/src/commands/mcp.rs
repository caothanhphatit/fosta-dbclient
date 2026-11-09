//! Settings → MCP server, and the answers to `run_query` approvals (see `crate::mcp`).

use crate::{
    audit::{AuditEntry, Outcome},
    config::AppConfig,
    error::{AppError, AppResult},
    mcp::McpStatus,
    AppState,
};

fn is_web(state: &AppState) -> bool {
    state.capabilities.edition == "web"
}

pub fn mcp_status(state: &AppState) -> McpStatus {
    let config = state.config.snapshot();
    state.mcp.status(&config, is_web(state))
}

/// Turns the server on or off and sets its port (desktop). Turning it on is recorded in the audit log; when the port
/// cannot be opened the server stays off and the error is returned.
pub fn set_mcp_settings(state: &AppState, enabled: bool, port: Option<u16>) -> AppResult<AppConfig> {
    if let Some(port) = port {
        if port < 1024 {
            return Err(AppError::new("Pick a port from 1024 to 65535"));
        }
    }
    let before = state.config.read(|config| config.mcp.clone());
    let config = state.config.update(|config| {
        config.mcp.enabled = enabled;
        if let Some(port) = port {
            config.mcp.port = port;
        }
        Ok(())
    })?;
    if let Err(error) = state.mcp.apply() {
        state.config.update(|config| {
            config.mcp.enabled = false;
            Ok(())
        })?;
        return Err(error);
    }
    if enabled && !before.enabled {
        state.engines.gateway.audit(AuditEntry {
            caller: "settings".to_owned(),
            category: "mcp".to_owned(),
            statement: if is_web(state) {
                "MCP server on (/mcp)".to_owned()
            } else {
                format!("MCP server on (127.0.0.1:{})", config.mcp.port)
            },
            outcome: Some(Outcome::Relaxed),
            ..AuditEntry::default()
        });
    }
    Ok(config)
}

/// The MCP bearer token, for the setup snippets. On the web edition it is the MCP-only token, separate from the
/// server's access token: it works for `/mcp` and nowhere else.
pub fn mcp_token(state: &AppState) -> AppResult<String> {
    state.mcp.token()
}

/// A new MCP token; clients set up with the old one stop working, their sessions end and pending approvals are
/// withdrawn.
pub fn rotate_mcp_token(state: &AppState) -> AppResult<String> {
    state.mcp.rotate_token()
}

/// The user's answer to a `run_query` approval: `approve` with the typed name where a typed confirmation is needed.
pub fn mcp_answer(state: &AppState, id: String, approve: bool, typed: Option<String>) -> AppResult<()> {
    state.mcp.answer(&id, approve, typed)
}
