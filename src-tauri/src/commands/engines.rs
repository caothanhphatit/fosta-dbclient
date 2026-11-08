//! What engines this build has, and connection strings.

use serde::Serialize;
use serde_json::Value;

use crate::{
    contract::{ConnectionSpec, EngineDescriptor},
    error::AppResult,
    events::EventSink,
    runtime::{oracle_client, registry},
    AppState,
};

/// Every registered engine, in connection-dialog order (planned ones included; the UI decides what to show).
pub fn engine_descriptors(_state: &AppState) -> Vec<EngineDescriptor> {
    registry().descriptors()
}

/// A connection string split into the profile fields it sets and its password.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedConnectionString {
    pub spec: ConnectionSpec,
    pub password: Option<String>,
}

pub fn parse_connection_string(_state: &AppState, engine: String, text: String) -> AppResult<ParsedConnectionString> {
    let parsed = registry().get(&engine)?.parse_connection_string(text.trim())?;
    Ok(ParsedConnectionString {
        spec: parsed.spec,
        password: parsed.password,
    })
}

/// `oracle_client_status`: whether Oracle Instant Client is available and loaded, and what a download would fetch.
pub fn oracle_client_status(_state: &AppState) -> AppResult<Value> {
    oracle_client::status()
}

/// `oracle_client_download`: downloads, installs and loads Instant Client, streaming `{phase, downloaded, total}` progress
/// events; resolves with the new status.
pub async fn oracle_client_download(_state: &AppState, events: EventSink<Value>) -> AppResult<Value> {
    let mut progress = move |event: Value| {
        events(event);
    };
    oracle_client::download(&mut progress).await
}

/// `oracle_client_cancel`: stops a running download.
pub fn oracle_client_cancel(_state: &AppState) {
    oracle_client::cancel();
}
