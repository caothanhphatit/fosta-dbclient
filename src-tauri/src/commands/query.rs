//! Query-tab sessions: open with a language, run (streaming), cancel, transactions, close.

use super::gateway_context;
use crate::{
    contract::{Flow, RunRequest, RunSummary, SessionInfo, SessionOptions, StreamEvent, TransactionOp, TransactionState},
    error::AppResult,
    events::EventSink,
    gateway::{self, Confirmation},
    AppState,
};

/// Opens a session for a query tab (its own server connection where the engine has one). `options` is optional so
/// older callers keep working.
pub async fn open_session(
    state: &AppState,
    connection_id: String,
    session_id: String,
    options: Option<SessionOptions>,
) -> AppResult<SessionInfo> {
    state
        .engines
        .open_session(&connection_id, session_id, options.unwrap_or_default())
        .await
}

pub async fn close_session(state: &AppState, session_id: String) -> AppResult<()> {
    state.engines.close_session(&session_id).await;
    Ok(())
}

/// Runs the editor text through the gateway; events stream through `events` while it runs. The run stops when the
/// receiver goes away (the tab closed, the HTTP response was dropped).
pub async fn run(
    state: &AppState,
    session_id: String,
    request: RunRequest,
    confirmation: Option<Confirmation>,
    events: EventSink<StreamEvent>,
) -> AppResult<RunSummary> {
    let context = gateway_context(state, &state.engines.session_connection(&session_id)?)?;
    let mut sink = move |event: StreamEvent| if events(event) { Flow::Continue } else { Flow::Stop };
    gateway::run(&state.engines, &context, &session_id, &request, confirmation.as_ref(), &mut sink).await
}

pub async fn cancel_run(state: &AppState, session_id: String) -> AppResult<()> {
    state.engines.cancel(&session_id).await
}

/// Begin / Commit / Rollback / auto-commit on / off / status.
pub async fn transaction(state: &AppState, session_id: String, op: TransactionOp) -> AppResult<TransactionState> {
    let context = gateway_context(state, &state.engines.session_connection(&session_id)?)?;
    gateway::transaction(&state.engines, &context, &session_id, op).await
}
