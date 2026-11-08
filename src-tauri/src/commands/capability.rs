//! Engine-specific operations used by engine views (`redis.scan`, `es.clusterHealth`, `vector.search`, …). Every call
//! is classified by the engine and checked by the gateway; unknown names are refused.

use serde_json::Value;

use super::gateway_context;
use crate::{
    error::AppResult,
    gateway::{self, Caller, Confirmation},
    AppState,
};

pub async fn invoke_capability(
    state: &AppState,
    connection_id: String,
    name: String,
    args: Option<Value>,
    confirmation: Option<Confirmation>,
) -> AppResult<Value> {
    let context = gateway_context(state, &connection_id)?;
    gateway::invoke(
        &state.engines,
        &context,
        Caller::EngineView,
        &name,
        args.unwrap_or(Value::Null),
        confirmation.as_ref(),
    )
    .await
}
