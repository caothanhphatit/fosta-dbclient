//! The MariaDB/MySQL engine port: one adapter for both engine ids. A connection keeps a small pool for the tree,
//! browsing and Stop, a second pool with `CLIENT_FOUND_ROWS` for grid saves, and one dedicated connection per query tab.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use mysql_async::{prelude::*, Conn, Opts, Pool};

use super::{
    catalog,
    connect::{self, within, Server, CHECKOUT_TIMEOUT},
    descriptor::{self, Flavor},
    session::{MySqlCanceller, MySqlSession},
    url, vector,
};
use crate::{
    contract::{
        classify_common, require, CallContext, Canceller, Capability, CapabilitySet, Category, Classification, ClassifyInput,
        ConnectRequest, Connection, ConnectionSpec, DatabaseAdmin, Engine, EngineDescriptor, NodeKind, ObjectDescription, ObjectRef,
        OpenedSession, ParsedConnection, SchemaGraph, SchemaGraphSource, ServerInfo, SessionOptions, StatementInfo, TabularData,
        TreeChildren, TreeRequest,
    },
    engines::common::sqlclassify::classify_sql,
    error::{AppError, AppResult},
};

pub struct MySqlAdapter {
    flavor: Flavor,
}

pub static MARIADB: MySqlAdapter = MySqlAdapter { flavor: Flavor::MariaDb };
pub static MYSQL: MySqlAdapter = MySqlAdapter { flavor: Flavor::MySql };

impl MySqlAdapter {
    pub fn flavor(&self) -> Flavor {
        self.flavor
    }
}

/// How the gateway treats the engine operations (all read-only).
fn classify_operation(name: &str) -> Classification {
    if vector::OPERATIONS.contains(&name) {
        Classification::single(StatementInfo::new(Category::Read, name, name))
    } else {
        Classification::single(StatementInfo::refused(name, name, "unknown operation"))
    }
}

#[async_trait]
impl Engine for MySqlAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static MARIADB_DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        static MYSQL_DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        let cell = match self.flavor {
            Flavor::MariaDb => &MARIADB_DESCRIPTOR,
            Flavor::MySql => &MYSQL_DESCRIPTOR,
        };
        cell.get_or_init(|| descriptor::descriptor(self.flavor))
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify_sql(self.flavor.dialect(), text),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("{} sessions do not run the '{language}' language.", self.flavor.label())),
            ),
            ClassifyInput::Capability { name, .. } => classify_operation(name),
            _ => Classification::default(),
        }
    }

    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        url::parse(self.flavor, text)
    }

    fn format_connection_string(&self, spec: &ConnectionSpec) -> Option<String> {
        Some(url::format(self.flavor, spec))
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let (options, pool, edit_pool, version) = connect::open(self.flavor, &request).await?;
        let server = Server::from_version(&version);
        let mut capabilities = self.descriptor().capabilities;
        if !server.vector {
            capabilities = capabilities.without(Capability::Vector);
        }
        Ok(Arc::new(MySqlConnection {
            engine_id: self.flavor.id(),
            info: server.info(&version),
            capabilities,
            opts: options.opts,
            pool,
            edit_pool,
            server,
        }))
    }
}

/// An open MariaDB/MySQL server.
pub struct MySqlConnection {
    engine_id: &'static str,
    info: ServerInfo,
    capabilities: CapabilitySet,
    /// For query sessions (without `CLIENT_FOUND_ROWS`, so affected rows read like the mysql client's).
    opts: Opts,
    pub(super) pool: Pool,
    pub(super) edit_pool: Pool,
    pub(super) server: Server,
}

impl MySqlConnection {
    pub(super) async fn pooled(&self) -> AppResult<Conn> {
        within(CHECKOUT_TIMEOUT, "Opening a connection", self.pool.get_conn()).await
    }
}

#[async_trait]
impl Connection for MySqlConnection {
    fn engine_id(&self) -> &str {
        self.engine_id
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        let mut conn = self.pooled().await?;
        let nodes = match &request.parent {
            None => catalog::databases(&mut conn)
                .await?
                .iter()
                .map(|name| catalog::database_node(name))
                .collect(),
            Some(parent) if parent.kind == NodeKind::DATABASE => {
                let database = parent
                    .object
                    .as_ref()
                    .map(|object| object.name.clone())
                    .or_else(|| parent.id.strip_prefix("db:").map(str::to_owned))
                    .ok_or_else(|| AppError::new("This item no longer exists. Refresh the tree."))?;
                catalog::tables(&mut conn, &database)
                    .await?
                    .iter()
                    .map(|table| catalog::table_node(&database, table))
                    .collect()
            }
            Some(_) => Vec::new(),
        };
        let nodes: Vec<_> = match request.filter.as_deref().map(str::to_lowercase).filter(|text| !text.is_empty()) {
            Some(filter) => nodes
                .into_iter()
                .filter(|node| node.label.to_lowercase().contains(&filter))
                .collect(),
            None => nodes,
        };
        Ok(TreeChildren {
            total_hint: Some(nodes.len() as u64),
            nodes,
            next_cursor: None,
        })
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        let mut conn = self.pooled().await?;
        catalog::describe(&mut conn, self.server.mariadb, object).await
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        if let Some(language) = options.language.as_deref().filter(|language| *language != "sql") {
            return Err(AppError::new(format!(
                "{} sessions do not run the '{language}' language.",
                self.info.product
            )));
        }
        let (session, thread_id) = MySqlSession::open(self.opts.clone(), self.server.mariadb, options.database.as_deref()).await?;
        let canceller: Arc<dyn Canceller> = Arc::new(MySqlCanceller {
            opts: self.opts.clone(),
            thread_id,
        });
        Ok(OpenedSession {
            session: Arc::new(session),
            canceller: Some(canceller),
        })
    }

    async fn invoke(&self, name: &str, args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        if !vector::OPERATIONS.contains(&name) {
            return Err(crate::contract::unsupported_capability(name));
        }
        require(self.capabilities, Capability::Vector)?;
        let mut conn = self.pooled().await?;
        vector::invoke(&mut conn, name, args).await
    }

    fn tabular(&self) -> Option<&dyn TabularData> {
        Some(self)
    }

    fn schema_graph(&self) -> Option<&dyn SchemaGraphSource> {
        Some(self)
    }

    fn admin(&self) -> Option<&dyn DatabaseAdmin> {
        Some(self)
    }

    async fn ping(&self) -> AppResult<()> {
        let mut conn = self.pooled().await?;
        conn.ping().await?;
        Ok(())
    }

    async fn close(&self) {
        let _ = self.pool.clone().disconnect().await;
        let _ = self.edit_pool.clone().disconnect().await;
    }
}

#[async_trait]
impl SchemaGraphSource for MySqlConnection {
    async fn schema_graph(&self, database: &str, _ctx: &CallContext) -> AppResult<SchemaGraph> {
        let mut conn = self.pooled().await?;
        catalog::schema_graph(&mut conn, database).await
    }
}

#[async_trait]
impl DatabaseAdmin for MySqlConnection {
    async fn create_database(&self, name: &str, _first_collection: Option<&str>, _ctx: &CallContext) -> AppResult<()> {
        let mut conn = self.pooled().await?;
        let sql = format!(
            "CREATE DATABASE {} CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci",
            catalog::quote_ident(name)
        );
        conn.query_drop(sql).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statement(adapter: &MySqlAdapter, text: &str) -> Option<Category> {
        adapter.classify(&ClassifyInput::Statement { language: "sql", text }).worst()
    }

    #[test]
    fn descriptors_are_cached_per_flavor() {
        assert_eq!(MARIADB.id(), "mariadb");
        assert_eq!(MYSQL.id(), "mysql");
        assert!(MARIADB.descriptor().capabilities.contains(Capability::Vector));
        assert!(!MYSQL.descriptor().capabilities.contains(Capability::Vector));
        assert!(std::ptr::eq(MARIADB.descriptor(), MARIADB.descriptor()));
    }

    #[test]
    fn statements_and_operations_are_classified() {
        assert_eq!(statement(&MARIADB, "SELECT * FROM t"), Some(Category::Read));
        assert_eq!(statement(&MYSQL, "DELETE FROM t"), Some(Category::Destructive));
        assert_eq!(statement(&MARIADB, "SELECT 1; DROP TABLE t"), Some(Category::Destructive));
        let other = MARIADB.classify(&ClassifyInput::Statement {
            language: "redis",
            text: "GET k",
        });
        assert_eq!(other.worst(), Some(Category::Unknown));
        let args = serde_json::Value::Null;
        for name in vector::OPERATIONS {
            let classification = MARIADB.classify(&ClassifyInput::Capability { name, args: &args });
            assert_eq!(classification.worst(), Some(Category::Read), "{name}");
            assert!(classification.refusal().is_none());
        }
        let unknown = MYSQL.classify(&ClassifyInput::Capability {
            name: "vector.drop",
            args: &args,
        });
        assert_eq!(unknown.refusal(), Some("unknown operation"));
    }

    #[test]
    fn connection_strings_parse_and_format_per_flavor() {
        let parsed = MYSQL.parse_connection_string("mysql://app:pw@db:3307/shop").unwrap();
        assert_eq!(parsed.spec.engine, "mysql");
        assert_eq!(parsed.password.as_deref(), Some("pw"));
        assert_eq!(
            MARIADB.format_connection_string(&parsed.spec).as_deref(),
            Some("mariadb://app@db:3307/shop")
        );
    }
}
