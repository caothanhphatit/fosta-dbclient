//! Redis engine core (key-value capability in ADR 0001).
//!
//! - [`connection`]: options (standalone, TLS, ACL user, RESP2/RESP3), [`RedisEngine::connect`], server info.
//! - [`scan`] and [`tree`]: SCAN-based key browsing (never `KEYS`) and an incremental prefix tree for the object tree.
//! - [`values`]: typed, paged reads for every data type; big collections are always read in windows.
//! - [`edit`]: typed write operations, each with its classification.
//! - [`console`]: a redis-cli style command console with typed RESP2/RESP3 replies and timeouts.
//! - [`classify`]: command classification for the statement gateway.
//!
//! Deferred (options are designed in [`connection::RedisTopology`], connecting returns a clear error):
//! - **Sentinel**: resolve the master through the sentinels (`SENTINEL get-master-addr-by-name`), then connect as standalone.
//!   The `redis` crate's `sentinel` feature can do this; it needs a sentinel container in `dev/` to be tested.
//! - **Cluster**: reads need SCAN on every primary (route by node address) and slot-aware key commands (`cluster-async`).
//!   Key browsing across nodes changes the cursor shape (one cursor per node), so it lands together with a cluster container.
//! - **Pub/Sub and MONITOR**: need a streaming channel to the UI (event sink port). The console refuses `SUBSCRIBE`,
//!   `PSUBSCRIBE`, `SSUBSCRIBE` and `MONITOR` with a message instead of hanging a connection.

pub mod adapter;
pub mod classify;
pub mod connection;
pub mod console;
pub mod descriptor;
pub mod edit;
pub mod scan;
pub mod tree;
pub mod types;
pub mod values;

#[cfg(test)]
mod live_tests;

pub use adapter::{RedisAdapter, REDIS};

use crate::error::AppError;

impl From<::redis::RedisError> for AppError {
    fn from(error: ::redis::RedisError) -> Self {
        AppError::new(redis_error_message(&error))
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
        format!("The Redis ACL user has no permission for this command: {text}")
    } else if error.is_timeout() {
        "Redis did not answer in time.".into()
    } else if error.is_connection_refusal() {
        format!("Could not connect to Redis (connection refused). Check the host and port. ({text})")
    } else {
        text
    }
}
