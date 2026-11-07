//! The engine-independent surface the UI talks to. Each database kind
//! implements [`Driver`]; [`DbManager`] owns one driver per open connection
//! and maps query-tab sessions to the connection they belong to.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;

use crate::{
    config::ConnectionProfile,
    db::{BrowseRequest, MysqlConnection, QueryOutcome, RowChange, TableDesign, TableInfo, TablePage},
    error::{AppError, AppResult, NOT_CONNECTED},
    lock::lock,
    mongo::MongoConnection,
    mssql::MssqlConnection,
    relations::TableRelations,
    schema::BulkSchema,
};

const NEEDS_MYSQL: &str = "This feature needs a MariaDB or MySQL connection";

#[async_trait]
pub trait Driver: Send + Sync {
    async fn list_databases(&self) -> AppResult<Vec<String>>;
    /// Creates an empty database. MongoDB only materialises a database with
    /// a collection in it, so it needs `first_collection`.
    async fn create_database(&self, name: &str, first_collection: Option<&str>) -> AppResult<()>;
    async fn list_tables(&self, database: &str) -> AppResult<Vec<TableInfo>>;
    async fn describe_table(&self, database: &str, table: &str) -> AppResult<TableDesign>;
    async fn browse_table(&self, request: &BrowseRequest) -> AppResult<TablePage>;
    async fn apply_changes(&self, database: &str, table: &str, changes: Vec<RowChange>) -> AppResult<u64>;
    /// Prepares a dedicated session for one query tab.
    async fn open_session(&self, session_id: &str) -> AppResult<()>;
    fn close_session(&self, session_id: &str);
    /// Runs the editor text in the tab's session. For SQL engines this is SQL;
    /// MongoDB takes a database command as JSON.
    async fn run_query(&self, session_id: &str, database: Option<String>, text: String) -> AppResult<QueryOutcome>;
    async fn cancel_query(&self, session_id: &str) -> AppResult<()>;
    async fn table_relations(&self, _database: &str, _table: &str) -> AppResult<TableRelations> {
        Ok(TableRelations::default())
    }
    /// The whole catalog of a database in a few bulk queries, for the ER diagram.
    /// `None` makes the caller fall back to per-table describe calls.
    async fn schema_bulk(&self, _database: &str) -> AppResult<Option<BulkSchema>> {
        Ok(None)
    }
    /// Releases pools and sockets; the driver is dropped right after.
    async fn close(&self) {}
    /// MariaDB-only tooling (vector search) downcasts through this.
    fn as_mysql(&self) -> Option<&MysqlConnection> {
        None
    }
}

/// Opens the right driver for the profile and returns it with the server version.
pub async fn open(profile: &ConnectionProfile, password: &str) -> AppResult<(Arc<dyn Driver>, String)> {
    match profile.driver.as_str() {
        "mssql" => {
            let (driver, version) = MssqlConnection::open(profile, password).await?;
            Ok((Arc::new(driver), version))
        }
        "mongodb" => {
            let (driver, version) = MongoConnection::open(profile, password).await?;
            Ok((Arc::new(driver), version))
        }
        _ => {
            let (driver, version) = MysqlConnection::open(profile, password).await?;
            Ok((Arc::new(driver), version))
        }
    }
}

/// "Test connection": opens, reads the version and closes again.
pub async fn probe(profile: &ConnectionProfile, password: &str) -> AppResult<String> {
    let (driver, version) = open(profile, password).await?;
    driver.close().await;
    Ok(version)
}

#[derive(Default)]
pub struct DbManager {
    connections: Mutex<HashMap<String, Arc<dyn Driver>>>,
    /// session id → connection id
    sessions: Mutex<HashMap<String, String>>,
}

impl DbManager {
    pub async fn connect(&self, profile: &ConnectionProfile, password: &str) -> AppResult<String> {
        let (driver, version) = open(profile, password).await?;
        let previous = lock(&self.connections).insert(profile.id.clone(), driver);
        if let Some(previous) = previous {
            previous.close().await;
        }
        Ok(version)
    }

    pub async fn disconnect(&self, connection_id: &str) {
        lock(&self.sessions).retain(|_, owner| owner != connection_id);
        let removed = lock(&self.connections).remove(connection_id);
        if let Some(driver) = removed {
            driver.close().await;
        }
    }

    pub fn driver(&self, connection_id: &str) -> AppResult<Arc<dyn Driver>> {
        lock(&self.connections)
            .get(connection_id)
            .cloned()
            .ok_or_else(|| AppError::new(NOT_CONNECTED))
    }

    pub async fn mysql_conn(&self, connection_id: &str) -> AppResult<mysql_async::Conn> {
        let driver = self.driver(connection_id)?;
        let mysql = driver.as_mysql().ok_or_else(|| AppError::new(NEEDS_MYSQL))?;
        mysql.pooled().await
    }

    pub fn is_mysql(&self, connection_id: &str) -> AppResult<bool> {
        Ok(self.driver(connection_id)?.as_mysql().is_some())
    }

    pub async fn open_session(&self, connection_id: &str, session_id: String) -> AppResult<()> {
        let driver = self.driver(connection_id)?;
        driver.open_session(&session_id).await?;
        lock(&self.sessions).insert(session_id, connection_id.to_owned());
        Ok(())
    }

    pub fn close_session(&self, session_id: &str) {
        let owner = lock(&self.sessions).remove(session_id);
        if let Some(driver) = owner.and_then(|id| self.driver(&id).ok()) {
            driver.close_session(session_id);
        }
    }

    pub async fn run_query(&self, session_id: &str, database: Option<String>, text: String) -> AppResult<QueryOutcome> {
        self.session_driver(session_id)?.run_query(session_id, database, text).await
    }

    pub async fn cancel_query(&self, session_id: &str) -> AppResult<()> {
        self.session_driver(session_id)?.cancel_query(session_id).await
    }

    fn session_driver(&self, session_id: &str) -> AppResult<Arc<dyn Driver>> {
        let owner = lock(&self.sessions)
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::new("This query tab lost its session. Close it and open a new query."))?;
        self.driver(&owner)
    }
}
