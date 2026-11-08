//! Errors engines return and the structured codes the UI acts on.
//!
//! Engine cores return [`EngineError`] (a sentence plus the engine's own code); at the contract boundary it becomes an
//! [`AppError`] with `{ code: "ENGINE_ERROR", engineCode, kind }` so the UI can tell a server rejection from a timeout.

use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{AppError, ErrorPosition};

/// An engine-side failure: the server rejected a statement, the client timed out, …
pub const ENGINE_ERROR: &str = "ENGINE_ERROR";
/// The profile names an engine this build does not know.
pub const UNKNOWN_ENGINE: &str = "UNKNOWN_ENGINE";
/// The engine is registered (descriptor, classifier, URL parser) but its adapter is not implemented yet.
pub const ENGINE_NOT_WIRED: &str = "ENGINE_NOT_WIRED";
/// The connection (or this build) does not offer the requested capability.
pub const UNSUPPORTED_CAPABILITY: &str = "UNSUPPORTED_CAPABILITY";
/// Row edits matched rows that changed (or disappeared) since they were read; nothing was applied.
pub const CONFLICT: &str = "CONFLICT";

/// An error shown to the user: a sentence that says what went wrong and, where possible, what to do. `code` carries the engine's
/// own code (SQLSTATE, ORA-xxxxx, ClickHouse error code) when there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineError {
    pub message: String,
    pub code: Option<String>,
    pub kind: ErrorKind,
    /// More text from the server (PostgreSQL DETAIL, CONTEXT / WHERE).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The server's suggestion (PostgreSQL HINT).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// 0-based character offset of the error in the statement, when the server reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// Bad options, a malformed connection string, a value that does not fit its column.
    Input,
    /// Could not reach or log in to the server, or the client library is missing.
    Connect,
    /// The server rejected a statement.
    Server,
    /// The statement was cancelled by the user.
    Cancelled,
    /// The statement or the connection attempt ran out of time.
    Timeout,
    /// Anything else (protocol errors, unexpected shapes).
    Internal,
}

impl EngineError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            kind,
            detail: None,
            hint: None,
            position: None,
        }
    }
    pub fn input(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Input, message)
    }
    pub fn connect(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Connect, message)
    }
    pub fn server(message: impl Into<String>, code: Option<String>) -> Self {
        Self {
            code,
            ..Self::new(ErrorKind::Server, message)
        }
    }
    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "The statement was cancelled.")
    }
    pub fn timeout(what: &str, limit: Duration) -> Self {
        Self::new(
            ErrorKind::Timeout,
            format!("{what} did not finish within {} ms.", limit.as_millis()),
        )
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{} ({code})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for EngineError {}

pub type EngineResult<T> = std::result::Result<T, EngineError>;

/// Keeps the engine's code, the error kind, detail, hint and position in the structured error; a five-character
/// code is also reported as the SQLSTATE.
impl From<EngineError> for AppError {
    fn from(error: EngineError) -> Self {
        let text = error.to_string();
        let retryable = matches!(error.kind, ErrorKind::Timeout | ErrorKind::Connect);
        let code = match error.kind {
            ErrorKind::Connect => "CONNECTION_ERROR",
            ErrorKind::Cancelled => "CANCELLED",
            ErrorKind::Timeout => "TIMEOUT",
            _ => ENGINE_ERROR,
        };
        let mut out = AppError::structured(text, json!({ "code": code, "kind": error.kind })).retryable(retryable);
        if let Some(engine_code) = &error.code {
            out = out.with_vendor_code(engine_code);
            if engine_code.len() == 5 && engine_code.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_uppercase()) {
                out = out.with_sqlstate(engine_code);
            }
        }
        if let Some(detail) = error.detail {
            out = out.with_detail(detail);
        }
        if let Some(hint) = error.hint {
            out = out.with("hint", hint);
        }
        if let Some(offset) = error.position {
            out = out.with_position(ErrorPosition {
                offset: Some(offset),
                ..ErrorPosition::default()
            });
        }
        out
    }
}

/// `UNSUPPORTED_CAPABILITY` for an operation the engine (or this connection) does not offer.
pub fn unsupported_capability(what: &str) -> AppError {
    AppError::structured(
        format!("This connection does not support {what}."),
        json!({ "code": UNSUPPORTED_CAPABILITY, "capability": what }),
    )
}

/// `ENGINE_NOT_WIRED` for a registered engine whose adapter is not implemented in this build yet.
pub fn not_wired(engine: &str, label: &str) -> AppError {
    AppError::structured(
        format!("{label} support is planned but not available in this build of Fosta Studio yet."),
        json!({ "code": ENGINE_NOT_WIRED, "engine": engine }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_errors_keep_their_code_and_kind() {
        let error = AppError::from(EngineError::server("relation \"x\" does not exist", Some("42P01".into())));
        assert_eq!(error.code(), Some(ENGINE_ERROR));
        let detail = error.payload().unwrap();
        assert_eq!(detail["vendorCode"], "42P01");
        assert_eq!(detail["sqlstate"], "42P01");
        assert_eq!(detail["kind"], "server");
        assert_eq!(error.to_string(), "relation \"x\" does not exist (42P01)");
    }

    #[test]
    fn capability_and_wiring_errors_are_structured() {
        assert_eq!(unsupported_capability("transactions").code(), Some(UNSUPPORTED_CAPABILITY));
        let error = not_wired("postgres", "PostgreSQL");
        assert_eq!(error.code(), Some(ENGINE_NOT_WIRED));
        assert_eq!(error.payload().unwrap()["engine"], "postgres");
    }
}
