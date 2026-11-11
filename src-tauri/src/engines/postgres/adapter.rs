//! The PostgreSQL engine on the contract. A connection is bound to one database of the server, so the adapter keeps one
//! [`PgEngine`] per database (opened on first use, for the tree, browsing and edits) and gives every query tab a session
//! of its own, which is what makes `BEGIN … COMMIT` span runs and a cancel hit only that tab.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{
    connect, descriptor,
    explain::{self, OPERATIONS},
    nodes,
    run::{PgCanceller, PgContractSession},
    PgEngine, PgOptions,
};
use crate::{
    contract::{
        classify_common, require, tabular::ApplyResult, vector::OPERATIONS as VECTOR_OPERATIONS, BrowseRequest, CallContext, Canceller,
        Capability, CapabilitySet, Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, DatabaseAdmin,
        Engine, EngineDescriptor, NodeKind, ObjectDescription, ObjectRef, OpenedSession, Page, ParsedConnection, RowChangeSet, SchemaGraph,
        SchemaGraphSource, ServerInfo, SessionOptions, StatementInfo, TabularData, TlsMode, TreeChildren, TreeRequest,
    },
    engines::common::{adapt::tls_to_contract, classify, split::Dialect},
    error::{AppError, AppResult},
};

pub struct PostgresAdapter;

pub static POSTGRES: PostgresAdapter = PostgresAdapter;

/// Percent-encodes a URL component (everything but unreserved characters).
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[async_trait]
impl Engine for PostgresAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify(Dialect::Postgres, text).into(),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("PostgreSQL sessions do not run the '{language}' language.")),
            ),
            // The vector operations only read (each runs in a read-only transaction).
            ClassifyInput::Capability { name, .. } if VECTOR_OPERATIONS.contains(name) => {
                Classification::single(StatementInfo::new(Category::Read, name, name))
            }
            ClassifyInput::Capability { name, args } => explain::classify_operation(name, args),
            _ => Classification::default(),
        }
    }

    /// `postgres://user:pass@host:port/db?sslmode=…` or a libpq key/value string, through [`PgOptions::parse`].
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = PgOptions::parse(text)?;
        let defaults = PgOptions::default();
        let mut options = HashMap::new();
        if parsed.application_name != defaults.application_name {
            options.insert("applicationName".to_owned(), parsed.application_name.clone());
        }
        if parsed.connect_timeout != defaults.connect_timeout {
            options.insert("connectTimeout".to_owned(), parsed.connect_timeout.as_secs().to_string());
        }
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: descriptor::ID.into(),
                host: parsed.host,
                port: parsed.port,
                user: parsed.user,
                database: Some(parsed.database),
                tls: tls_to_contract(&parsed.tls),
                options,
                ..Default::default()
            },
            password: parsed.password,
        })
    }

    /// The URL of a profile, without the password.
    fn format_connection_string(&self, spec: &ConnectionSpec) -> Option<String> {
        let mut url = String::from("postgres://");
        if !spec.user.is_empty() {
            url.push_str(&encode(&spec.user));
            url.push('@');
        }
        url.push_str(if spec.host.contains(':') { "[" } else { "" });
        url.push_str(if spec.host.is_empty() { "localhost" } else { &spec.host });
        url.push_str(if spec.host.contains(':') { "]" } else { "" });
        if spec.port != 0 && spec.port != 5432 {
            url.push_str(&format!(":{}", spec.port));
        }
        if let Some(database) = spec.database.as_deref().filter(|name| !name.is_empty()) {
            url.push_str(&format!("/{}", encode(database)));
        }
        let mut query = Vec::new();
        if let Some(tls) = spec.tls.as_ref().filter(|tls| tls.mode != TlsMode::Disable) {
            let mode = match tls.mode {
                TlsMode::VerifyCa => "verify-ca",
                TlsMode::Require if tls.accept_invalid_certs => "require",
                _ => "verify-full",
            };
            query.push(format!("sslmode={mode}"));
            if let Some(ca) = tls.ca_file.as_deref().filter(|path| !path.is_empty()) {
                query.push(format!("sslrootcert={}", encode(ca)));
            }
        }
        if let Some(name) = spec.option("applicationName") {
            query.push(format!("application_name={}", encode(name)));
        }
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        Some(url)
    }

    fn distinct_values_statement(
        &self,
        object: &crate::contract::ObjectRef,
        column: &crate::contract::ColumnInfo,
        sample: u64,
        limit: u64,
    ) -> Option<String> {
        let object = crate::engines::common::adapt::object_to_common(object).ok()?;
        Some(crate::engines::common::sqlbuild::build_distinct_sample(
            &super::PgDialect,
            &object,
            column,
            sample,
            limit,
        ))
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let options = connect::build_options(&request)?;
        let home = PgEngine::connect(options.clone()).await?;
        let info = connect::server_info(home.server_version());
        let mut capabilities = self.descriptor().capabilities;
        if !home.has_vector_extension().await.unwrap_or(false) {
            capabilities = capabilities.without(Capability::Vector);
        }
        let database = home.database().to_owned();
        let mut engines = HashMap::new();
        engines.insert(database.clone(), Arc::new(home));
        Ok(Arc::new(PostgresConnection {
            info,
            capabilities,
            options,
            home: database,
            engines: Mutex::new(engines),
        }))
    }
}

/// An open PostgreSQL server.
pub struct PostgresConnection {
    info: ServerInfo,
    capabilities: CapabilitySet,
    options: PgOptions,
    /// The database the profile connects to (the tree's engine for the server-level calls).
    home: String,
    engines: Mutex<HashMap<String, Arc<PgEngine>>>,
}

impl PostgresConnection {
    /// The engine of `database` (the profile's when `None`), opened on first use and reopened when the server closed it.
    async fn engine(&self, database: Option<&str>) -> AppResult<Arc<PgEngine>> {
        let name = database.filter(|name| !name.is_empty()).unwrap_or(&self.home);
        let mut engines = self.engines.lock().await;
        if let Some(engine) = engines.get(name) {
            if !engine.is_closed() {
                return Ok(Arc::clone(engine));
            }
        }
        let engine = Arc::new(
            PgEngine::connect(PgOptions {
                database: name.to_owned(),
                ..self.options.clone()
            })
            .await?,
        );
        engines.insert(name.to_owned(), Arc::clone(&engine));
        Ok(engine)
    }
}

#[async_trait]
impl Connection for PostgresConnection {
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
        let filter = request.filter.as_deref();
        match &request.parent {
            None => {
                let home = self.engine(None).await?;
                let mut nodes: Vec<_> = home
                    .list_databases()
                    .await?
                    .iter()
                    .map(|name| nodes::database_node(name, false))
                    .collect();
                if let Some(text) = filter.map(str::to_lowercase).filter(|text| !text.is_empty()) {
                    nodes.retain(|node| node.label.to_lowercase().contains(&text));
                }
                Ok(TreeChildren {
                    total_hint: Some(nodes.len() as u64),
                    nodes,
                    next_cursor: None,
                })
            }
            Some(parent) => {
                let database = nodes::database_of(parent)?;
                let engine = self.engine(Some(&database)).await?;
                let mut root = engine.catalog().await?;
                nodes::prune(&mut root);
                nodes::children(&root, parent, &database, filter)
            }
        }
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        let engine = self.engine(object.database.as_deref()).await?;
        let mut description = engine.describe_contract(object).await?;
        if description.object.kind == NodeKind::TABLE || description.object.kind == NodeKind::MATERIALIZED_VIEW {
            let schema = description.object.schema.clone().unwrap_or_else(|| "public".into());
            description.referenced_by = engine.referencing(&schema, &description.object.name).await?;
        }
        Ok(description)
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        if let Some(language) = options.language.as_deref().filter(|language| *language != "sql") {
            return Err(AppError::new(format!("PostgreSQL sessions do not run the '{language}' language.")));
        }
        let engine = self.engine(options.database.as_deref()).await?;
        let session = engine.open_session().await?;
        let canceller: Arc<dyn Canceller> = Arc::new(PgCanceller(session.canceller()));
        Ok(OpenedSession {
            session: Arc::new(PgContractSession { inner: session }),
            canceller: Some(canceller),
        })
    }

    async fn invoke(&self, name: &str, args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        let database = args.get("database").and_then(|value| value.as_str()).map(str::to_owned);
        if VECTOR_OPERATIONS.contains(&name) {
            require(self.capabilities, Capability::Vector)?;
            return self.engine(database.as_deref()).await?.vector(name, args).await;
        }
        if !OPERATIONS.contains(&name) {
            return Err(crate::contract::unsupported_capability(name));
        }
        require(self.capabilities, Capability::Explain)?;
        self.engine(database.as_deref()).await?.explain(&args).await
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
        self.engine(None).await?.ping().await?;
        Ok(())
    }

    async fn close(&self) {
        self.engines.lock().await.clear();
    }
}

#[async_trait]
impl TabularData for PostgresConnection {
    async fn browse(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        self.engine(request.object.database.as_deref())
            .await?
            .browse_page(request, ctx)
            .await
    }

    async fn apply(&self, changes: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        if changes.changes.is_empty() {
            return Ok(ApplyResult::default());
        }
        self.engine(changes.object.database.as_deref()).await?.apply_set(changes).await
    }
}

#[async_trait]
impl SchemaGraphSource for PostgresConnection {
    async fn schema_graph(&self, database: &str, _ctx: &CallContext) -> AppResult<SchemaGraph> {
        self.engine(Some(database)).await?.graph().await
    }
}

#[async_trait]
impl DatabaseAdmin for PostgresConnection {
    async fn create_database(&self, name: &str, _first_collection: Option<&str>, _ctx: &CallContext) -> AppResult<()> {
        let sql = format!("CREATE DATABASE {}", super::catalog::ident(name));
        self.engine(None).await?.batch(&sql).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::contract::{ConnectSecrets, Endpoint, EngineStatus, TlsConfig};

    fn statement(text: &str) -> Classification {
        POSTGRES.classify(&ClassifyInput::Statement { language: "sql", text })
    }

    #[test]
    fn descriptor_is_ready_and_consistent() {
        let descriptor = POSTGRES.descriptor();
        assert_eq!(POSTGRES.id(), "postgres");
        assert_eq!(descriptor.status, EngineStatus::Ready);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("sql"));
        for capability in [
            Capability::Schemas,
            Capability::MultipleDatabases,
            Capability::SchemaGraph,
            Capability::Explain,
        ] {
            assert!(descriptor.capabilities.contains(capability), "{capability:?}");
        }
    }

    #[test]
    fn statements_are_classified_with_the_postgres_dialect() {
        assert_eq!(statement("SELECT * FROM orders").worst(), Some(Category::Read));
        assert_eq!(
            statement("UPDATE orders SET paid = true WHERE id = 1").worst(),
            Some(Category::Write)
        );
        assert_eq!(statement("CREATE TABLE t (id int)").worst(), Some(Category::Ddl));
        assert_eq!(statement("DELETE FROM orders").worst(), Some(Category::Destructive));
        assert_eq!(statement("SELECT 1; DROP TABLE t").statements.len(), 2);
        assert_eq!(statement("VACUUM ANALYZE orders").worst(), Some(Category::Admin));
        assert_eq!(statement("REFRESH MATERIALIZED VIEW mv").worst(), Some(Category::Ddl));
        assert_eq!(
            statement("EXPLAIN ANALYZE DELETE FROM t WHERE id = 1").worst(),
            Some(Category::Write)
        );
        assert_eq!(
            statement("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d").worst(),
            Some(Category::Write)
        );
        let other = POSTGRES.classify(&ClassifyInput::Statement {
            language: "redis",
            text: "GET k",
        });
        assert_eq!(other.worst(), Some(Category::Unknown));
    }

    #[test]
    fn operations_are_classified_and_unknown_names_refused() {
        let plain = serde_json::json!({ "sql": "DELETE FROM t" });
        let explain = POSTGRES.classify(&ClassifyInput::Capability {
            name: "postgres.explain",
            args: &plain,
        });
        assert_eq!(explain.worst(), Some(Category::Read));
        let args = serde_json::Value::Null;
        for name in VECTOR_OPERATIONS {
            let vector = POSTGRES.classify(&ClassifyInput::Capability { name, args: &args });
            assert_eq!(vector.worst(), Some(Category::Read), "{name}");
            assert!(vector.refusal().is_none());
        }
        let unknown = POSTGRES.classify(&ClassifyInput::Capability {
            name: "pg.vacuum",
            args: &args,
        });
        assert_eq!(unknown.refusal(), Some("unknown operation"));
    }

    #[test]
    fn urls_split_into_a_spec_and_a_password() {
        let parsed = POSTGRES
            .parse_connection_string("postgres://app:s3cret@db.local:5433/shop?sslmode=require&application_name=etl")
            .unwrap();
        assert_eq!(parsed.password.as_deref(), Some("s3cret"));
        assert_eq!(parsed.spec.engine, "postgres");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("db.local", 5433));
        assert_eq!(parsed.spec.user, "app");
        assert_eq!(parsed.spec.database.as_deref(), Some("shop"));
        assert_eq!(parsed.spec.option("applicationName"), Some("etl"));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::Require));
        assert!(parsed.spec.options.values().all(|value| !value.contains("s3cret")));
        assert!(parsed.spec.url.is_none());
        assert!(POSTGRES.parse_connection_string("postgres://h/db?bogus=1").is_err());
    }

    #[test]
    fn a_profile_formats_back_into_a_url_without_the_password() {
        let parsed = POSTGRES
            .parse_connection_string("postgres://app:s3cret@db.local:5433/my%20shop?sslmode=verify-full&application_name=etl")
            .unwrap();
        let url = POSTGRES.format_connection_string(&parsed.spec).unwrap();
        assert_eq!(
            url,
            "postgres://app@db.local:5433/my%20shop?sslmode=verify-full&application_name=etl"
        );
        assert!(!url.contains("s3cret"));
        let again = POSTGRES.parse_connection_string(&url).unwrap();
        assert_eq!(again.spec, parsed.spec);
        let plain = ConnectionSpec {
            host: "localhost".into(),
            port: 5432,
            user: "postgres".into(),
            tls: Some(TlsConfig {
                mode: TlsMode::Require,
                accept_invalid_certs: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            POSTGRES.format_connection_string(&plain).as_deref(),
            Some("postgres://postgres@localhost?sslmode=require")
        );
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_connection_error() {
        let request = ConnectRequest {
            spec: ConnectionSpec::default(),
            secrets: ConnectSecrets::default(),
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: 1,
            },
            connect_timeout: Duration::from_secs(2),
        };
        let Err(error) = POSTGRES.connect(request).await else {
            panic!("nothing listens on port 1");
        };
        assert_eq!(error.code(), Some("CONNECTION_ERROR"), "{error:?}");
    }
}
