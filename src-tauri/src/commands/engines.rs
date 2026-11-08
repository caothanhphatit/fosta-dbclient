//! What engines this build has, and connection strings.

use serde::Serialize;

use crate::{
    contract::{ConnectionSpec, EngineDescriptor},
    error::AppResult,
    runtime::registry,
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
