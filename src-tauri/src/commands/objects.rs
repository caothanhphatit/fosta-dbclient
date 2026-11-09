//! The object tree, descriptions, browsing and editing of an open connection. Reads of metadata go to the engine
//! directly (contained); browsing and edits go through the statement gateway, which applies the workspace's rules.

use super::gateway_context;
use crate::{
    contract::{
        unsupported_capability, ApplyResult, BrowseRequest, CallContext, Capability, ObjectDescription, ObjectRef, Page, RowChangeSet,
        SchemaGraph, TreeChildren, TreeRequest,
    },
    error::{AppError, AppResult},
    gateway::{self, Caller, Confirmation},
    AppState,
};

pub(crate) fn metadata_context(state: &AppState, connection_id: &str) -> CallContext {
    let limits = gateway_context(state, connection_id)
        .map(|context| context.limits())
        .unwrap_or_default();
    CallContext::new(limits)
}

pub async fn tree_children(state: &AppState, connection_id: String, request: TreeRequest) -> AppResult<TreeChildren> {
    let ctx = metadata_context(state, &connection_id);
    state
        .engines
        .handle(&connection_id)?
        .call("listed objects", |conn| async move { conn.children(&request, &ctx).await })
        .await
}

pub async fn describe_object(state: &AppState, connection_id: String, object: ObjectRef) -> AppResult<ObjectDescription> {
    let ctx = metadata_context(state, &connection_id);
    state
        .engines
        .handle(&connection_id)?
        .call("described an object", |conn| async move { conn.describe(&object, &ctx).await })
        .await
}

/// One page of rows. With `request_id`, `cancel_request` stops it (the grid replaced it with a newer page or closed).
pub async fn browse_object(state: &AppState, connection_id: String, request: BrowseRequest, request_id: Option<String>) -> AppResult<Page> {
    let context = gateway_context(state, &connection_id)?;
    gateway::browse_cancellable(&state.engines, &context, &connection_id, &request, request_id.as_deref()).await
}

/// Stops the call started with `request_id` (a page read, an export); unknown ids are remembered briefly, since a
/// cancel may arrive before its request.
pub fn cancel_request(state: &AppState, request_id: String) {
    state.engines.requests.cancel(&request_id);
}

/// Saves grid edits (`caller` "gridSave", the default) or imported rows ("import"; batches of one import share
/// `import_id`, so the run is confirmed once). A change that matches a row someone else changed fails with `CONFLICT`.
pub async fn apply_edits(
    state: &AppState,
    connection_id: String,
    changes: RowChangeSet,
    caller: Option<Caller>,
    import_id: Option<String>,
    confirmation: Option<Confirmation>,
) -> AppResult<ApplyResult> {
    let context = gateway_context(state, &connection_id)?;
    gateway::apply_edits(
        &state.engines,
        &context,
        caller.unwrap_or(Caller::GridSave),
        &changes,
        import_id.as_deref(),
        confirmation.as_ref(),
    )
    .await
}

/// Tables, columns and foreign keys of one database, for the ER diagram.
pub async fn schema_graph(state: &AppState, connection_id: String, database: String) -> AppResult<SchemaGraph> {
    let ctx = metadata_context(state, &connection_id);
    let handle = state.engines.handle(&connection_id)?;
    crate::contract::require(handle.conn.capabilities(), Capability::SchemaGraph)?;
    handle
        .call("read the schema", |conn| async move {
            let source = conn.schema_graph().ok_or_else(|| unsupported_capability("schema diagrams"))?;
            source.schema_graph(&database, &ctx).await
        })
        .await
}

pub async fn create_database(
    state: &AppState,
    connection_id: String,
    name: String,
    first_collection: Option<String>,
    confirmation: Option<Confirmation>,
) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::new("Database name is required"));
    }
    let context = gateway_context(state, &connection_id)?;
    gateway::create_database(&state.engines, &context, name, first_collection.as_deref(), confirmation.as_ref()).await
}
