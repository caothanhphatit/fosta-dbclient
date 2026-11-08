//! Redis engine (key-value capability in ADR 0001; also Valkey and other RESP servers).
//!
//! The core:
//! - [`connection`]: options (standalone, TLS, ACL user, RESP2/RESP3), [`RedisEngine::connect`], server info.
//! - [`scan`] and [`tree`]: SCAN-based key browsing (never `KEYS`) and an incremental prefix tree.
//! - [`values`]: typed, paged reads for every data type; big collections are always read in windows.
//! - [`edit`]: typed write operations, each with its classification.
//! - [`console`]: a redis-cli style command console with typed RESP2/RESP3 replies and timeouts.
//! - [`classify`]: command classification for the statement gateway.
//!
//! The contract port over it:
//! - [`adapter`]: the engine and its connection (one connection per logical database, opened on first use).
//! - [`connect`]: connection options from a [`crate::contract::ConnectRequest`] (endpoint, secrets, TLS files).
//! - [`catalog`]: the tree (`db0`..`db15`, then `SCAN` pages of keys) and key descriptions.
//! - [`ops`]: the `redis.*` operations engine views call through `invoke`.
//! - [`guard`]: edits that fail with `CONFLICT` when the value changed since the user read it (WATCH/MULTI).
//! - [`session`]: console sessions with a `CLIENT UNBLOCK` canceller.
//!
//! Out of scope (connecting says so instead of guessing): the **Cluster** and **Sentinel** topologies (see
//! [`connection::RedisTopology`]), and Pub/Sub and `MONITOR`, which the console refuses with a message instead of
//! hanging a connection.

pub mod adapter;
pub mod catalog;
pub mod classify;
pub mod connect;
pub mod connection;
pub mod console;
pub mod descriptor;
pub mod edit;
pub mod guard;
pub mod ops;
pub mod scan;
pub mod session;
pub mod tree;
pub mod types;
pub mod values;

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod live_tests;

pub use adapter::{RedisAdapter, REDIS};

use crate::{
    contract::{EngineError, ErrorKind},
    error::AppError,
};

/// The structured form of a client error: `ENGINE_ERROR` with the server's error name (`WRONGTYPE`, `ERR`, `NOPERM`) as the
/// vendor code, `CONNECTION_ERROR` for unreachable servers and failed logins, `TIMEOUT` for slow ones.
impl From<::redis::RedisError> for AppError {
    fn from(error: ::redis::RedisError) -> Self {
        let message = redis_error_message(&error);
        let code = error.code().map(str::to_owned);
        let login = matches!(code.as_deref(), Some("NOAUTH" | "WRONGPASS"));
        let kind = if error.is_timeout() {
            ErrorKind::Timeout
        } else if login || error.is_connection_refusal() || error.is_connection_dropped() || error.is_io_error() {
            ErrorKind::Connect
        } else if code.is_some() {
            ErrorKind::Server
        } else {
            ErrorKind::Internal
        };
        let mut engine_error = EngineError::new(kind, message);
        engine_error.code = code;
        AppError::from(engine_error).with_engine(descriptor::ID)
    }
}

/// Turns a client error into a sentence for the user.
pub(crate) fn redis_error_message(error: &::redis::RedisError) -> String {
    let text = error.to_string();
    let lower = text.to_ascii_lowercase();
    if lower.contains("noauth") || lower.contains("authentication required") {
        "Redis asks for a password. Enter the password (and the ACL user name if there is one).".into()
    } else if lower.contains("wrongpass") || lower.contains("invalid username-password") || lower.contains("authentication failed") {
        "Redis rejected the user name or password.".into()
    } else if lower.contains("noperm") {
        format!(
            "The Redis ACL user has no permission for this command: {}",
            error.detail().unwrap_or(&text)
        )
    } else if error.is_timeout() {
        "Redis did not answer in time.".into()
    } else if error.is_connection_refusal() {
        format!("Could not connect to Redis (connection refused). Check the host and port. ({text})")
    } else if let Some(detail) = error.detail() {
        detail.to_owned()
    } else {
        text
    }
}
