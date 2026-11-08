//! The SQL Server engine port. A connection keeps a few idle clients for the tree, browsing and edits (each call takes
//! its own) and opens one dedicated client per query tab.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use super::{
    catalog::{self, bracket, text_rows},
    connect::{self, ClientPool},
    descriptor,
    session::{MssqlCanceller, MssqlSession},
    url,
};
use crate::{
    contract::{
        classify_common, CallContext, Canceller, CapabilitySet, Category, Classification, ClassifyInput, ConnectRequest, Connection,
        ConnectionSpec, DatabaseAdmin, Engine, EngineDescriptor, NodeKind, ObjectDescription, ObjectRef, OpenedSession, ParsedConnection,
        SchemaGraph, SchemaGraphSource, ServerInfo, SessionOptions, SqlDialectId, StatementInfo, TabularData, TreeChildren, TreeRequest,
    },
    engines::common::sqlclassify::classify_sql,
    error::{AppError, AppResult},
};

pub struct MssqlAdapter;

pub static MSSQL: MssqlAdapter = MssqlAdapter;

#[async_trait]
impl Engine for MssqlAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify_sql(SqlDialectId::MsSql, text),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("SQL Server sessions do not run the '{language}' language.")),
            ),
            // SQL Server has no engine operations.
            ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::refused(name, name, "unknown operation")),
            _ => Classification::default(),
        }
    }

    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        url::parse(text)
    }

    fn format_connection_string(&self, spec: &ConnectionSpec) -> Option<String> {
        Some(url::format(spec))
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        connect::check_tls(&request)?;
        let config = connect::build_config(&request);
        let mut client = connect::connect(&config, request.connect_timeout).await?;
        let rows = text_rows(
            &mut client,
            "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)), CAST(SERVERPROPERTY('Edition') AS NVARCHAR(128))",
            &[],
        )
        .await?;
        let row = rows.first();
        let info = connect::server_info(
            &row.map(|row| row.text(0)).unwrap_or_default(),
            &row.map(|row| row.text(1)).unwrap_or_default(),
        );
        Ok(Arc::new(MssqlConnection {
            info,
            capabilities: self.descriptor().capabilities,
            pool: Arc::new(ClientPool::new(config, client)),
        }))
    }
}

/// An open SQL Server.
pub struct MssqlConnection {
    info: ServerInfo,
    capabilities: CapabilitySet,
    pub(super) pool: Arc<ClientPool>,
}

fn database_of_node(parent: &crate::contract::NodeKey) -> AppResult<String> {
    parent
        .object
        .as_ref()
        .map(|object| object.name.clone())
        .or_else(|| parent.id.strip_prefix("db:").map(str::to_owned))
        .ok_or_else(|| AppError::new("This item no longer exists. Refresh the tree."))
}

#[async_trait]
impl Connection for MssqlConnection {
    fn engine_id(&self) -> &str {
        descriptor::ID
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        let mut client = self.pool.take().await?;
        let listed = match &request.parent {
            None => catalog::databases(&mut client)
                .await
                .map(|names| names.iter().map(|name| catalog::database_node(name)).collect()),
            Some(parent) if parent.kind == NodeKind::DATABASE => match database_of_node(parent) {
                Ok(database) => catalog::tables(&mut client, &database)
                    .await
                    .map(|tables| tables.iter().map(|table| catalog::table_node(&database, table)).collect()),
                Err(error) => Err(error),
            },
            Some(_) => Ok(Vec::new()),
        };
        self.pool.release(client, &listed);
        let nodes: Vec<_> = listed?;
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
        let mut client = self.pool.take().await?;
        let described = catalog::describe(&mut client, object).await;
        self.pool.release(client, &described);
        described
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        if let Some(language) = options.language.as_deref().filter(|language| *language != "sql") {
            return Err(AppError::new(format!("SQL Server sessions do not run the '{language}' language.")));
        }
        let (session, signal) = MssqlSession::open(self.pool.config().clone(), Arc::clone(&self.pool), options.database.as_deref()).await?;
        let canceller: Arc<dyn Canceller> = Arc::new(MssqlCanceller {
            pool: Arc::clone(&self.pool),
            signal,
        });
        Ok(OpenedSession {
            session: Arc::new(session),
            canceller: Some(canceller),
        })
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
        let mut client = self.pool.take().await?;
        let pinged = text_rows(&mut client, "SELECT 1", &[]).await.map(|_| ());
        self.pool.release(client, &pinged);
        pinged
    }

    async fn close(&self) {
        self.pool.clear();
    }
}

#[async_trait]
impl SchemaGraphSource for MssqlConnection {
    async fn schema_graph(&self, database: &str, _ctx: &CallContext) -> AppResult<SchemaGraph> {
        let mut client = self.pool.take().await?;
        let graph = catalog::schema_graph(&mut client, database).await;
        self.pool.release(client, &graph);
        graph
    }
}

#[async_trait]
impl DatabaseAdmin for MssqlConnection {
    async fn create_database(&self, name: &str, _first_collection: Option<&str>, _ctx: &CallContext) -> AppResult<()> {
        let mut client = self.pool.take().await?;
        let created = async {
            client
                .simple_query(format!("CREATE DATABASE {}", bracket(name)))
                .await?
                .into_results()
                .await?;
            Ok::<_, AppError>(())
        }
        .await;
        self.pool.release(client, &created);
        created
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Capability;

    fn statement(text: &str) -> Option<Category> {
        MSSQL.classify(&ClassifyInput::Statement { language: "sql", text }).worst()
    }

    #[test]
    fn the_descriptor_is_cached_and_offers_stop() {
        assert_eq!(MSSQL.id(), "mssql");
        assert!(std::ptr::eq(MSSQL.descriptor(), MSSQL.descriptor()));
        let capabilities = MSSQL.descriptor().capabilities;
        assert!(capabilities.contains(Capability::Cancel) && capabilities.contains(Capability::KeysetPaging));
        assert!(!capabilities.contains(Capability::ReadOnlySession));
    }

    #[test]
    fn statements_and_operations_are_classified() {
        assert_eq!(statement("SELECT TOP 5 * FROM t"), Some(Category::Read));
        assert_eq!(statement("TRUNCATE TABLE t"), Some(Category::Destructive));
        assert_eq!(statement("BEGIN TRANSACTION"), Some(Category::Transaction));
        let other = MSSQL.classify(&ClassifyInput::Statement {
            language: "redis",
            text: "GET k",
        });
        assert_eq!(other.worst(), Some(Category::Unknown));
        let unknown = MSSQL.classify(&ClassifyInput::Capability {
            name: "vector.search",
            args: &serde_json::Value::Null,
        });
        assert_eq!(unknown.refusal(), Some("unknown operation"));
    }

    #[test]
    fn connection_strings_parse_and_format() {
        let parsed = MSSQL
            .parse_connection_string("Server=sql,1444;Database=ERP;User Id=sa;Password=pw")
            .unwrap();
        assert_eq!(parsed.spec.engine, "mssql");
        assert_eq!(parsed.password.as_deref(), Some("pw"));
        assert_eq!(
            MSSQL.format_connection_string(&parsed.spec).as_deref(),
            Some("Server=sql,1444;Database=ERP;User Id=sa;Encrypt=True")
        );
    }
}
