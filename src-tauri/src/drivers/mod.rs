//! The pre-contract drivers (MariaDB/MySQL, SQL Server, MongoDB). The runtime serves them through the engine contract
//! (`runtime::legacy`) until each has a native adapter; the old commands still call them directly.
//!
//! One file per engine: [`mysql`] (MariaDB and MySQL), [`mssql`] (SQL Server)
//! and [`mongo`] (MongoDB). The data shapes they all return live in [`types`].

pub mod mongo;
pub mod mssql;
mod mssql_batch;
pub mod mysql;
pub mod relations;
pub mod types;

use std::sync::Arc;

use async_trait::async_trait;

use self::{
    mongo::MongoConnection,
    mssql::MssqlConnection,
    mysql::MysqlConnection,
    relations::TableRelations,
    types::{BrowseRequest, QueryOutcome, RowChange, TableDesign, TableInfo, TablePage, TransactionOp, TransactionState},
};
use crate::{
    config::ConnectionProfile,
    error::{AppError, AppResult},
    schema::BulkSchema,
};

pub const NEEDS_MYSQL: &str = "This feature needs a MariaDB or MySQL connection";

/// The engine behind a driver: picks the SQL dialect and the engine-level backstops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    MariaDb,
    MySql,
    SqlServer,
    MongoDb,
}

pub use crate::contract::limits::Limits;

/// Statements the app sends to a database go through `crate::gateway` first, which checks
/// them against the workspace's rules; drivers only execute.
#[async_trait]
pub trait Driver: Send + Sync {
    fn engine(&self) -> Engine;
    async fn list_databases(&self) -> AppResult<Vec<String>>;
    /// Creates an empty database. MongoDB only materialises a database with
    /// a collection in it, so it needs `first_collection`.
    async fn create_database(&self, name: &str, first_collection: Option<&str>) -> AppResult<()>;
    async fn list_tables(&self, database: &str) -> AppResult<Vec<TableInfo>>;
    async fn describe_table(&self, database: &str, table: &str) -> AppResult<TableDesign>;
    /// Reads one page. The filter has been validated by the gateway as a single condition.
    async fn browse_table(&self, request: &BrowseRequest, limits: &Limits) -> AppResult<TablePage>;
    async fn apply_changes(&self, database: &str, table: &str, changes: Vec<RowChange>) -> AppResult<u64>;
    /// Prepares a dedicated session for one query tab.
    async fn open_session(&self, session_id: &str) -> AppResult<()>;
    fn close_session(&self, session_id: &str);
    /// Runs the editor text in the tab's session. For SQL engines this is SQL;
    /// MongoDB takes a database command as JSON.
    async fn run_query(&self, session_id: &str, database: Option<String>, text: String, limits: &Limits) -> AppResult<QueryOutcome>;
    async fn cancel_query(&self, session_id: &str) -> AppResult<()>;
    /// Begin / Commit / Rollback / auto-commit in a query-tab session, returning the new state.
    async fn transaction(&self, _session_id: &str, _op: TransactionOp, _limits: &Limits) -> AppResult<TransactionState> {
        Ok(TransactionState::unsupported())
    }
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
        "mariadb" | "mysql" => {
            let (driver, version) = MysqlConnection::open(profile, password).await?;
            Ok((Arc::new(driver), version))
        }
        other => Err(unknown_driver(other)),
    }
}

/// A profile whose driver this build does not know (a typo in fosta-studio.json, or a newer app's engine).
fn unknown_driver(driver: &str) -> AppError {
    AppError::new(format!(
        "Unknown database type '{driver}'. This version supports mariadb, mysql, mssql and mongodb; edit the connection to pick one."
    ))
}

/// "Test connection": opens, reads the version and closes again.
pub async fn probe(profile: &ConnectionProfile, password: &str) -> AppResult<String> {
    let (driver, version) = open(profile, password).await?;
    driver.close().await;
    Ok(version)
}

impl Engine {
    /// The profile's engine id, as in `fosta-studio.json`.
    pub fn id(self) -> &'static str {
        match self {
            Engine::MariaDb => "mariadb",
            Engine::MySql => "mysql",
            Engine::SqlServer => "mssql",
            Engine::MongoDb => "mongodb",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Engine::MariaDb => "MariaDB",
            Engine::MySql => "MySQL",
            Engine::SqlServer => "SQL Server",
            Engine::MongoDb => "MongoDB",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_driver_ids_are_refused_instead_of_opening_mysql() {
        let profile = ConnectionProfile {
            id: "x".into(),
            name: "x".into(),
            driver: "postgress".into(),
            host: "127.0.0.1".into(),
            port: 1,
            user: String::new(),
            database: None,
            save_password: false,
            options: Default::default(),
            safety: None,
        };
        let error = open(&profile, "").await.err().expect("refused");
        assert!(error.to_string().starts_with("Unknown database type 'postgress'"), "{error}");
    }
}
