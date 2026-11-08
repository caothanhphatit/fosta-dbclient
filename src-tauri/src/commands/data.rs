//! Browsing, editing and querying an open connection. Catalog reads go straight to the
//! connection's [`Driver`](crate::drivers::Driver); everything that runs a statement on the
//! user's behalf (filters, saves, imports, queries, new databases, transactions) goes through
//! the statement gateway (`crate::gateway`), which applies the workspace's safety rules.

use crate::{
    drivers::{
        relations::TableRelations,
        types::{BrowseRequest, QueryOutcome, RowChange, TableDesign, TableInfo, TablePage, TransactionOp, TransactionState},
    },
    error::{AppError, AppResult},
    gateway::{self, Caller, Confirmation, Context},
    schema, AppState,
};

/// The gateway context (workspace, connection, rules) of a saved connection.
pub(crate) fn gateway_context(state: &AppState, connection_id: &str) -> AppResult<Context> {
    state.config.read(|config| Context::for_connection(config, connection_id))
}

pub async fn list_databases(state: &AppState, connection_id: String) -> AppResult<Vec<String>> {
    state
        .engines
        .call_legacy(&connection_id, "listed the databases", |driver| async move {
            driver.list_databases().await
        })
        .await
}

/// Tables, columns and foreign keys of one database, for the ER diagram.
pub async fn schema_graph(state: &AppState, connection_id: String, database: String) -> AppResult<schema::SchemaGraph> {
    state
        .engines
        .call_legacy(&connection_id, "read the schema", |driver| async move {
            schema::schema_graph(driver.as_ref(), &database).await
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

pub async fn list_tables(state: &AppState, connection_id: String, database: String) -> AppResult<Vec<TableInfo>> {
    state
        .engines
        .call_legacy(&connection_id, "listed the tables", |driver| async move {
            driver.list_tables(&database).await
        })
        .await
}

pub async fn describe_table(state: &AppState, connection_id: String, database: String, table: String) -> AppResult<TableDesign> {
    state
        .engines
        .call_legacy(&connection_id, "described a table", |driver| async move {
            driver.describe_table(&database, &table).await
        })
        .await
}

pub async fn browse_table(state: &AppState, request: BrowseRequest) -> AppResult<TablePage> {
    let context = gateway_context(state, &request.connection_id)?;
    gateway::browse(&state.engines, &context, &request).await
}

/// Saves grid edits (`caller` "gridSave", the default) or imported rows ("import"; batches of
/// one import share `import_id`, so the run is confirmed once).
#[allow(clippy::too_many_arguments)]
pub async fn apply_changes(
    state: &AppState,
    connection_id: String,
    database: String,
    table: String,
    changes: Vec<RowChange>,
    caller: Option<Caller>,
    import_id: Option<String>,
    confirmation: Option<Confirmation>,
) -> AppResult<u64> {
    let context = gateway_context(state, &connection_id)?;
    let caller = caller.unwrap_or(Caller::GridSave);
    gateway::apply_changes(
        &state.engines,
        &context,
        caller,
        &database,
        &table,
        changes,
        import_id.as_deref(),
        confirmation.as_ref(),
    )
    .await
}

pub async fn table_relations(state: &AppState, connection_id: String, database: String, table: String) -> AppResult<TableRelations> {
    state
        .engines
        .call_legacy(&connection_id, "read the relations of a table", |driver| async move {
            driver.table_relations(&database, &table).await
        })
        .await
}

pub async fn open_session(state: &AppState, connection_id: String, session_id: String) -> AppResult<()> {
    state.engines.open_legacy_session(&connection_id, session_id).await
}

pub async fn close_session(state: &AppState, session_id: String) -> AppResult<()> {
    state.engines.close_session(&session_id).await;
    Ok(())
}

pub async fn run_sql(
    state: &AppState,
    session_id: String,
    database: Option<String>,
    sql: String,
    confirmation: Option<Confirmation>,
) -> AppResult<QueryOutcome> {
    let context = gateway_context(state, &state.engines.session_connection(&session_id)?)?;
    gateway::run_query(&state.engines, &context, &session_id, database, sql, confirmation.as_ref()).await
}

pub async fn cancel_sql(state: &AppState, session_id: String) -> AppResult<()> {
    state.engines.cancel(&session_id).await
}

/// Begin / Commit / Rollback / auto-commit on / off / status for a query tab's session.
pub async fn sql_transaction(state: &AppState, session_id: String, op: TransactionOp) -> AppResult<TransactionState> {
    let context = gateway_context(state, &state.engines.session_connection(&session_id)?)?;
    gateway::transaction(&state.engines, &context, &session_id, op).await
}
