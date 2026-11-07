//! Browsing, editing and querying an open connection. Every command here goes
//! through the connection's [`Driver`](crate::drivers::Driver).

use tauri::State;

use crate::{
    drivers::{
        relations::TableRelations,
        types::{BrowseRequest, QueryOutcome, RowChange, TableDesign, TableInfo, TablePage},
    },
    error::{AppError, AppResult},
    schema, AppState,
};

#[tauri::command]
pub async fn list_databases(state: State<'_, AppState>, connection_id: String) -> AppResult<Vec<String>> {
    state.db.driver(&connection_id)?.list_databases().await
}

/// Tables, columns and foreign keys of one database, for the ER diagram.
#[tauri::command]
pub async fn schema_graph(state: State<'_, AppState>, connection_id: String, database: String) -> AppResult<schema::SchemaGraph> {
    let driver = state.db.driver(&connection_id)?;
    schema::schema_graph(driver.as_ref(), &database).await
}

#[tauri::command]
pub async fn create_database(
    state: State<'_, AppState>,
    connection_id: String,
    name: String,
    first_collection: Option<String>,
) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::new("Database name is required"));
    }
    state
        .db
        .driver(&connection_id)?
        .create_database(name, first_collection.as_deref())
        .await
}

#[tauri::command]
pub async fn list_tables(state: State<'_, AppState>, connection_id: String, database: String) -> AppResult<Vec<TableInfo>> {
    state.db.driver(&connection_id)?.list_tables(&database).await
}

#[tauri::command]
pub async fn describe_table(state: State<'_, AppState>, connection_id: String, database: String, table: String) -> AppResult<TableDesign> {
    state.db.driver(&connection_id)?.describe_table(&database, &table).await
}

#[tauri::command]
pub async fn browse_table(state: State<'_, AppState>, request: BrowseRequest) -> AppResult<TablePage> {
    state.db.driver(&request.connection_id)?.browse_table(&request).await
}

#[tauri::command]
pub async fn apply_changes(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
    changes: Vec<RowChange>,
) -> AppResult<u64> {
    state.db.driver(&connection_id)?.apply_changes(&database, &table, changes).await
}

#[tauri::command]
pub async fn table_relations(
    state: State<'_, AppState>,
    connection_id: String,
    database: String,
    table: String,
) -> AppResult<TableRelations> {
    state.db.driver(&connection_id)?.table_relations(&database, &table).await
}

#[tauri::command]
pub async fn open_session(state: State<'_, AppState>, connection_id: String, session_id: String) -> AppResult<()> {
    state.db.open_session(&connection_id, session_id).await
}

#[tauri::command]
pub fn close_session(state: State<'_, AppState>, session_id: String) {
    state.db.close_session(&session_id);
}

#[tauri::command]
pub async fn run_sql(state: State<'_, AppState>, session_id: String, database: Option<String>, sql: String) -> AppResult<QueryOutcome> {
    state.db.run_query(&session_id, database, sql).await
}

#[tauri::command]
pub async fn cancel_sql(state: State<'_, AppState>, session_id: String) -> AppResult<()> {
    state.db.cancel_query(&session_id).await
}
