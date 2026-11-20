//! Redis engine (key-value capability in ADR 0001; also Valkey and other RESP servers).
//!
//! The core:
//! - [`connection`]: options (standalone or Redis Cluster, TLS, ACL user, RESP2/RESP3), [`RedisEngine::connect`], server
//!   info.
//! - [`cluster`]: the Redis Cluster shard map, commands sent to one node, the composite SCAN cursor, the cluster overview.
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
//! A Redis Cluster connection has database 0 only, no vector search, and a console without MULTI/EXEC, WATCH or SCAN.
//! Out of scope (connecting says so instead of guessing): the **Sentinel** topology (see [`connection::RedisTopology`]),
//! Redis Cluster through SSH, and Pub/Sub and `MONITOR`, which the console refuses with a message instead of hanging a
//! connection.

pub mod adapter;
pub mod catalog;
pub mod classify;
pub mod cluster;
pub mod connect;
pub mod connection;
pub mod console;
pub mod descriptor;
pub mod edit;
pub mod guard;
pub mod ops;
mod profile;
pub mod scan;
pub mod session;
pub mod tree;
pub mod types;
pub mod values;
pub mod vector;

#[cfg(test)]
mod cluster_contract_tests;
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
        } else if login
            || error.is_connection_refusal()
            || error.is_connection_dropped()
            || error.is_io_error()
            || is_cluster_unreachable(&error)
        {
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

/// No cluster node could be reached: none of the seeds answered, or the node owning a slot has no connection.
fn is_cluster_unreachable(error: &::redis::RedisError) -> bool {
    matches!(error.kind(), ::redis::ErrorKind::ClusterConnectionNotFound)
        || error
            .to_string()
            .to_ascii_lowercase()
            .contains("failed to create initial connections")
}

/// Turns a client error into a sentence for the user.
pub(crate) fn redis_error_message(error: &::redis::RedisError) -> String {
    let text = error.to_string();
    let lower = text.to_ascii_lowercase();
    let detail = error.detail().unwrap_or(&text);
    match error.code() {
        Some("CROSSSLOT") => {
            return "The keys live in different hash slots of the cluster, so one command cannot touch them together. Give \
                    them a common {hashtag} (user:{42}:name, user:{42}:email) or run one command per key."
                .into()
        }
        Some("MOVED" | "ASK") => {
            return format!(
                "This key lives on another node of a Redis Cluster ({detail}). Connect with the Cluster topology to follow \
                 the cluster's redirects."
            )
        }
        Some("CLUSTERDOWN") => return format!("The Redis Cluster is down or does not cover every hash slot: {detail}"),
        Some("READONLY") => {
            return "This server is a read-only replica, so it refuses writes. Connect to the master (or through Sentinel) \
                    to change data."
                .into()
        }
        Some("MASTERDOWN") => {
            return "The replica lost its link with the master and refuses to answer until it is back (failover in \
                    progress?)."
                .into()
        }
        _ => {}
    }
    if is_cluster_unreachable(error) {
        return format!("Could not reach any node of the Redis Cluster. Check the seed nodes and their ports. ({text})");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn server_error(code: &str, detail: &str) -> AppError {
        ::redis::make_extension_error(code.into(), Some(detail.into())).into()
    }

    #[test]
    fn cluster_errors_read_as_sentences() {
        let cross = server_error("CROSSSLOT", "Keys in request don't hash to the same slot");
        assert!(cross.message().contains("hashtag"), "{}", cross.message());
        assert_eq!(cross.payload().unwrap()["vendorCode"], "CROSSSLOT");
        assert!(server_error("MOVED", "3999 127.0.0.1:7002").message().contains("127.0.0.1:7002"));
        assert!(server_error("READONLY", "You can't write against a read only replica.")
            .message()
            .contains("replica"));
        assert!(server_error("CLUSTERDOWN", "Hash slot not served")
            .message()
            .contains("Hash slot not served"));
        assert!(server_error("MASTERDOWN", "Link with MASTER is down").message().contains("master"));
        let unreachable: AppError = ::redis::RedisError::from((::redis::ErrorKind::ClusterConnectionNotFound, "no node")).into();
        assert_eq!(unreachable.code(), Some("CONNECTION_ERROR"));
    }
}
