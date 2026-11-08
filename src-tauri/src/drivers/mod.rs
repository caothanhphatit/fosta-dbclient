//! The engine-independent surface the UI talks to. Each database kind
//! implements [`Driver`]; [`DbManager`] owns one driver per open connection
//! and maps query-tab sessions to the connection they belong to.
//!
//! One file per engine: [`mysql`] (MariaDB and MySQL), [`mssql`] (SQL Server)
//! and [`mongo`] (MongoDB). The data shapes they all return live in [`types`].

pub mod mongo;
pub mod mssql;
mod mssql_batch;
pub mod mysql;
pub mod relations;
pub mod types;

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

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
    error::{AppError, AppResult, NOT_CONNECTED},
    gateway::Gateway,
    lock::lock,
    logging,
    runtime::contain::{contained, crashed, is_crash},
    schema::BulkSchema,
};

const NEEDS_MYSQL: &str = "This feature needs a MariaDB or MySQL connection";
const DEFAULT_MAX_ROWS: usize = 1000;

/// The engine behind a driver: picks the SQL dialect and the engine-level backstops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    MariaDb,
    MySql,
    SqlServer,
    MongoDb,
}

/// Engine-level backstops set by the workspace's safety rules (see `crate::gateway`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Rows kept per result set in query tabs.
    pub max_rows: usize,
    /// Statement timeout in seconds; 0 = none.
    pub timeout_secs: u32,
    /// Put sessions in read-only mode where the engine has one.
    pub read_only: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            timeout_secs: 0,
            read_only: false,
        }
    }
}

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

fn label_of(driver: &str) -> &'static str {
    match driver {
        "mysql" => "MySQL",
        "mssql" => "SQL Server",
        "mongodb" => "MongoDB",
        _ => "MariaDB",
    }
}

struct Entry {
    driver: Arc<dyn Driver>,
    /// Set after a contained panic: the connection refuses calls until the user connects again.
    poisoned: Arc<AtomicBool>,
    version: String,
}

/// An open connection as reported by `diagnostics()`.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenConnection {
    pub engine: &'static str,
    pub version: String,
    pub crashed: bool,
}

#[derive(Default)]
pub struct DbManager {
    connections: Mutex<HashMap<String, Entry>>,
    /// session id → connection id
    sessions: Mutex<HashMap<String, String>>,
    /// Checks every statement against the workspace's rules (see `crate::gateway`).
    pub gateway: Gateway,
}

impl DbManager {
    pub async fn connect(&self, profile: &ConnectionProfile, password: &str) -> AppResult<String> {
        let (owned, secret) = (profile.clone(), password.to_owned());
        let (driver, version) = contained(label_of(&profile.driver), "connected", async move { open(&owned, &secret).await })
            .await
            .map_err(|error| error.with_engine(&profile.driver))?;
        tracing::info!(engine = profile.driver.as_str(), connection = logging::id_hash(&profile.id), %version, "connected");
        let entry = Entry {
            driver,
            poisoned: Arc::default(),
            version: version.clone(),
        };
        let previous = lock(&self.connections).insert(profile.id.clone(), entry);
        if let Some(previous) = previous {
            previous.driver.close().await;
        }
        Ok(version)
    }

    pub async fn disconnect(&self, connection_id: &str) {
        lock(&self.sessions).retain(|_, owner| owner != connection_id);
        let removed = lock(&self.connections).remove(connection_id);
        if let Some(entry) = removed {
            let driver = entry.driver;
            let _ = contained(driver.engine().label(), "disconnected", async move {
                driver.close().await;
                Ok(())
            })
            .await;
        }
    }

    fn entry(&self, connection_id: &str) -> AppResult<(Arc<dyn Driver>, Arc<AtomicBool>)> {
        let connections = lock(&self.connections);
        let entry = connections.get(connection_id).ok_or_else(|| AppError::new(NOT_CONNECTED))?;
        if entry.poisoned.load(Ordering::Relaxed) {
            let engine = entry.driver.engine();
            return Err(
                crashed(engine.label(), "ran an earlier operation", "the connection is closed after a crash").with_engine(engine.id()),
            );
        }
        Ok((Arc::clone(&entry.driver), Arc::clone(&entry.poisoned)))
    }

    /// The driver of an open connection, for callers that are not engine calls themselves (its engine kind).
    pub fn driver(&self, connection_id: &str) -> AppResult<Arc<dyn Driver>> {
        self.entry(connection_id).map(|(driver, _)| driver)
    }

    /// Runs one engine call in its own task (see `runtime::contain`). A panic becomes `ENGINE_CRASHED` and poisons the
    /// connection; every error gets the engine id.
    pub async fn call<T, F, Fut>(&self, connection_id: &str, what: &'static str, call: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn Driver>) -> Fut,
        Fut: Future<Output = AppResult<T>> + Send + 'static,
    {
        let (driver, poisoned) = self.entry(connection_id)?;
        let engine = driver.engine();
        let outcome = contained(engine.label(), what, call(driver)).await;
        if outcome.as_ref().err().is_some_and(is_crash) {
            poisoned.store(true, Ordering::Relaxed);
        }
        outcome.map_err(|error| error.with_engine(engine.id()))
    }

    /// [`call`](Self::call) on the connection a query-tab session belongs to.
    pub async fn call_session<T, F, Fut>(&self, session_id: &str, what: &'static str, call: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn Driver>) -> Fut,
        Fut: Future<Output = AppResult<T>> + Send + 'static,
    {
        let connection_id = self.session_connection(session_id)?;
        self.call(&connection_id, what, call).await
    }

    pub async fn mysql_conn(&self, connection_id: &str) -> AppResult<mysql_async::Conn> {
        self.call(connection_id, "opened a connection", |driver| async move {
            let mysql = driver.as_mysql().ok_or_else(|| AppError::new(NEEDS_MYSQL))?;
            mysql.pooled().await
        })
        .await
    }

    pub fn is_mysql(&self, connection_id: &str) -> AppResult<bool> {
        Ok(self.driver(connection_id)?.as_mysql().is_some())
    }

    pub async fn open_session(&self, connection_id: &str, session_id: String) -> AppResult<()> {
        let id = session_id.clone();
        self.call(connection_id, "opened a query session", |driver| async move {
            driver.open_session(&id).await
        })
        .await?;
        lock(&self.sessions).insert(session_id, connection_id.to_owned());
        Ok(())
    }

    pub fn close_session(&self, session_id: &str) {
        let owner = lock(&self.sessions).remove(session_id);
        if let Some(driver) = owner.and_then(|id| self.driver(&id).ok()) {
            driver.close_session(session_id);
        }
    }

    pub async fn cancel_query(&self, session_id: &str) -> AppResult<()> {
        let id = session_id.to_owned();
        self.call_session(
            session_id,
            "cancelled a query",
            |driver| async move { driver.cancel_query(&id).await },
        )
        .await
    }

    /// The connection a query-tab session belongs to.
    pub fn session_connection(&self, session_id: &str) -> AppResult<String> {
        lock(&self.sessions)
            .get(session_id)
            .cloned()
            .ok_or_else(|| AppError::new("This query tab lost its session. Close it and open a new query."))
    }

    pub fn session_driver(&self, session_id: &str) -> AppResult<Arc<dyn Driver>> {
        self.driver(&self.session_connection(session_id)?)
    }

    /// Engine and server version of every open connection, for `diagnostics()`.
    pub fn open_connections(&self) -> Vec<OpenConnection> {
        lock(&self.connections)
            .values()
            .map(|entry| OpenConnection {
                engine: entry.driver.engine().id(),
                version: entry.version.clone(),
                crashed: entry.poisoned.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// Registers a driver directly (tests use fakes that record what they were asked to do).
    #[cfg(test)]
    pub fn insert_driver(&self, connection_id: &str, driver: Arc<dyn Driver>) {
        let entry = Entry {
            driver,
            poisoned: Arc::default(),
            version: "test".into(),
        };
        lock(&self.connections).insert(connection_id.to_owned(), entry);
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

    /// A driver that panics on demand.
    struct Panicky;

    #[async_trait]
    impl Driver for Panicky {
        fn engine(&self) -> Engine {
            Engine::MariaDb
        }
        async fn list_databases(&self) -> AppResult<Vec<String>> {
            Ok(vec!["shop".into()])
        }
        async fn create_database(&self, _: &str, _: Option<&str>) -> AppResult<()> {
            Ok(())
        }
        async fn list_tables(&self, _: &str) -> AppResult<Vec<TableInfo>> {
            let rows: Vec<TableInfo> = Vec::new();
            Ok(vec![rows.into_iter().nth(std::hint::black_box(3)).expect("driver bug")])
        }
        async fn describe_table(&self, _: &str, _: &str) -> AppResult<TableDesign> {
            Err(AppError::new("not here"))
        }
        async fn browse_table(&self, _: &BrowseRequest, _: &Limits) -> AppResult<TablePage> {
            Err(AppError::new("not here"))
        }
        async fn apply_changes(&self, _: &str, _: &str, _: Vec<RowChange>) -> AppResult<u64> {
            Ok(0)
        }
        async fn open_session(&self, _: &str) -> AppResult<()> {
            Ok(())
        }
        fn close_session(&self, _: &str) {}
        async fn run_query(&self, _: &str, _: Option<String>, _: String, _: &Limits) -> AppResult<QueryOutcome> {
            Err(AppError::new("not here"))
        }
        async fn cancel_query(&self, _: &str) -> AppResult<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_driver_panic_is_contained_and_poisons_only_its_connection() {
        let manager = DbManager::default();
        manager.insert_driver("bad", Arc::new(Panicky));
        manager.insert_driver("good", Arc::new(Panicky));
        let list = |id: &'static str| manager.call(id, "listed the tables", |driver| async move { driver.list_tables("shop").await });
        let error = list("bad").await.expect_err("panicked");
        assert!(is_crash(&error), "{error}");
        assert_eq!(error.field("engine").and_then(|v| v.as_str()), Some("mariadb"));
        // Poisoned until the user reconnects, even for calls that would work.
        let again = manager
            .call("bad", "listed the databases", |driver| async move { driver.list_databases().await })
            .await
            .expect_err("poisoned");
        assert!(is_crash(&again));
        assert!(manager.open_connections().iter().any(|c| c.crashed));
        // Other connections keep working.
        let names = manager
            .call(
                "good",
                "listed the databases",
                |driver| async move { driver.list_databases().await },
            )
            .await
            .unwrap();
        assert_eq!(names, vec!["shop".to_owned()]);
        manager.disconnect("bad").await;
        assert_eq!(manager.driver("bad").err().map(|e| e.to_string()), Some(NOT_CONNECTED.to_owned()));
    }
}
