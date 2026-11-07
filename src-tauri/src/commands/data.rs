//! Browsing, editing and querying an open connection. Every command here goes
//! through the connection's [`Driver`](crate::drivers::Driver).

use crate::{
    drivers::{
        relations::TableRelations,
        types::{BrowseRequest, QueryOutcome, RowChange, TableDesign, TableInfo, TablePage},
    },
    error::{AppError, AppResult},
    schema, AppState,
};

pub async fn list_databases(state: &AppState, connection_id: String) -> AppResult<Vec<String>> {
    state.db.driver(&connection_id)?.list_databases().await
}

/// Tables, columns and foreign keys of one database, for the ER diagram.
pub async fn schema_graph(state: &AppState, connection_id: String, database: String) -> AppResult<schema::SchemaGraph> {
    let driver = state.db.driver(&connection_id)?;
    schema::schema_graph(driver.as_ref(), &database).await
}

pub async fn create_database(state: &AppState, connection_id: String, name: String, first_collection: Option<String>) -> AppResult<()> {
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

pub async fn list_tables(state: &AppState, connection_id: String, database: String) -> AppResult<Vec<TableInfo>> {
    state.db.driver(&connection_id)?.list_tables(&database).await
}

pub async fn describe_table(state: &AppState, connection_id: String, database: String, table: String) -> AppResult<TableDesign> {
    state.db.driver(&connection_id)?.describe_table(&database, &table).await
}

pub async fn browse_table(state: &AppState, request: BrowseRequest) -> AppResult<TablePage> {
    state.db.driver(&request.connection_id)?.browse_table(&request).await
}

pub async fn apply_changes(
    state: &AppState,
    connection_id: String,
    database: String,
    table: String,
    changes: Vec<RowChange>,
) -> AppResult<u64> {
    state.db.driver(&connection_id)?.apply_changes(&database, &table, changes).await
}

pub async fn table_relations(state: &AppState, connection_id: String, database: String, table: String) -> AppResult<TableRelations> {
    state.db.driver(&connection_id)?.table_relations(&database, &table).await
}

pub async fn open_session(state: &AppState, connection_id: String, session_id: String) -> AppResult<()> {
    state.db.open_session(&connection_id, session_id).await
}

pub fn close_session(state: &AppState, session_id: String) {
    state.db.close_session(&session_id);
}

pub async fn run_sql(state: &AppState, session_id: String, database: Option<String>, sql: String) -> AppResult<QueryOutcome> {
    state.db.run_query(&session_id, database, sql).await
}

pub async fn cancel_sql(state: &AppState, session_id: String) -> AppResult<()> {
    state.db.cancel_query(&session_id).await
}
