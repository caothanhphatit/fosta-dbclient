//! The SQLite engine on the contract. The adapter keeps one connection to the file for the tree, browsing and edits, and
//! gives every query tab a connection of its own (so `BEGIN … COMMIT` spans runs and a cancel interrupts only that tab).
//! Rows are edited by primary key or `rowid`; views are read-only.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;

use super::{
    connect, descriptor,
    explain::{self, OPERATIONS},
    nodes,
    run::{SqliteCanceller, SqliteContractSession},
    SqliteEngine, SqliteOptions,
};
use crate::{
    contract::{
        classify_common, require, tabular::ApplyResult, BrowseRequest, CallContext, Canceller, Capability, CapabilitySet, Category,
        Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, Engine, EngineDescriptor, ObjectDescription, ObjectRef,
        OpenedSession, Page, ParsedConnection, RowChangeSet, SchemaGraph, SchemaGraphSource, ServerInfo, SessionOptions, StatementInfo,
        TabularData, TreeChildren, TreeRequest,
    },
    engines::common::{classify, split::Dialect},
    error::{AppError, AppResult},
};

pub struct SqliteAdapter;

pub static SQLITE: SqliteAdapter = SqliteAdapter;

/// Percent-encodes the characters that would end a path in a URL.
fn encode_path(path: &str) -> String {
    path.replace('\\', "/")
        .replace('%', "%25")
        .replace('?', "%3F")
        .replace('#', "%23")
        .replace(' ', "%20")
}

#[async_trait]
impl Engine for SqliteAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify(Dialect::Sqlite, text).into(),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("SQLite sessions do not run the '{language}' language.")),
            ),
            ClassifyInput::Capability { name, args } => explain::classify_operation(name, args),
            _ => Classification::default(),
        }
    }

    /// `sqlite:///path.db`, `file:path.db?mode=ro` or a plain path, through [`SqliteOptions::from_uri`].
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = SqliteOptions::from_uri(text)?;
        let defaults = SqliteOptions::file(&parsed.path);
        let mut options = HashMap::new();
        if parsed.read_only {
            options.insert("readOnly".to_owned(), "true".to_owned());
        }
        if parsed.create {
            options.insert("create".to_owned(), "true".to_owned());
        }
        if parsed.busy_timeout != defaults.busy_timeout {
            options.insert("busyTimeout".to_owned(), parsed.busy_timeout.as_millis().to_string());
        }
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: descriptor::ID.into(),
                file: Some(parsed.path),
                options,
                ..Default::default()
            },
            password: None,
        })
    }

    fn format_connection_string(&self, spec: &ConnectionSpec) -> Option<String> {
        let path = encode_path(spec.file.as_deref().filter(|file| !file.is_empty())?);
        let mut url = if path.starts_with('/') {
            format!("sqlite://{path}")
        } else if path.as_bytes().get(1) == Some(&b':') {
            format!("sqlite:///{path}")
        } else {
            format!("sqlite://{path}")
        };
        let mut query = Vec::new();
        if spec.option("readOnly") == Some("true") {
            query.push("mode=ro".to_owned());
        } else if spec.option("create") == Some("true") {
            query.push("mode=rwc".to_owned());
        }
        if let Some(millis) = spec.option("busyTimeout") {
            query.push(format!("busy_timeout={millis}"));
        }
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        Some(url)
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let options = connect::build_options(&request)?;
        let read_only = options.read_only;
        let engine = SqliteEngine::connect(options.clone()).await?;
        let mut capabilities = self.descriptor().capabilities;
        if read_only {
            capabilities = capabilities.without(Capability::EditRows);
        }
        Ok(Arc::new(SqliteConnection {
            info: connect::server_info(engine.server_version(), &options),
            capabilities,
            engine: Arc::new(engine),
        }))
    }
}

/// An open SQLite database file.
pub struct SqliteConnection {
    info: ServerInfo,
    capabilities: CapabilitySet,
    engine: Arc<SqliteEngine>,
}

#[async_trait]
impl Connection for SqliteConnection {
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
        let catalog = self.engine.catalog().await?;
        nodes::children(&catalog, request.parent.as_ref(), request.filter.as_deref())
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        self.engine.describe_contract(object).await
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        if let Some(language) = options.language.as_deref().filter(|language| *language != "sql") {
            return Err(AppError::new(format!("SQLite sessions do not run the '{language}' language.")));
        }
        let session = self.engine.open_session().await?;
        let canceller: Arc<dyn Canceller> = Arc::new(SqliteCanceller(session.canceller()));
        Ok(OpenedSession {
            session: Arc::new(SqliteContractSession { inner: session }),
            canceller: Some(canceller),
        })
    }

    async fn invoke(&self, name: &str, args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        if !OPERATIONS.contains(&name) {
            return Err(crate::contract::unsupported_capability(name));
        }
        require(self.capabilities, Capability::Explain)?;
        self.engine.explain(&args).await
    }

    fn tabular(&self) -> Option<&dyn TabularData> {
        Some(self)
    }

    fn schema_graph(&self) -> Option<&dyn SchemaGraphSource> {
        Some(self)
    }

    async fn ping(&self) -> AppResult<()> {
        self.engine.ping().await
    }

    async fn close(&self) {}
}

#[async_trait]
impl TabularData for SqliteConnection {
    async fn browse(&self, request: &BrowseRequest, _ctx: &CallContext) -> AppResult<Page> {
        self.engine.browse_page(request).await
    }

    async fn apply(&self, changes: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        require(self.capabilities, Capability::EditRows)?;
        if changes.changes.is_empty() {
            return Ok(ApplyResult::default());
        }
        self.engine.apply_set(changes).await
    }
}

#[async_trait]
impl SchemaGraphSource for SqliteConnection {
    async fn schema_graph(&self, database: &str, _ctx: &CallContext) -> AppResult<SchemaGraph> {
        self.engine.graph(database).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::contract::{ConnectSecrets, Endpoint, EngineStatus};

    fn statement(text: &str) -> Classification {
        SQLITE.classify(&ClassifyInput::Statement { language: "sql", text })
    }

    fn request(file: Option<&str>, options: &[(&str, &str)]) -> ConnectRequest {
        let mut spec = ConnectionSpec {
            engine: "sqlite".into(),
            file: file.map(str::to_owned),
            ..Default::default()
        };
        for (key, value) in options {
            spec.options.insert((*key).to_owned(), (*value).to_owned());
        }
        ConnectRequest {
            spec,
            secrets: ConnectSecrets::default(),
            endpoint: Endpoint {
                host: String::new(),
                port: 0,
            },
            connect_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn descriptor_is_ready_and_consistent() {
        let descriptor = SQLITE.descriptor();
        assert_eq!(SQLITE.id(), "sqlite");
        assert_eq!(descriptor.status, EngineStatus::Ready);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("sql"));
        assert!(descriptor.connection.uses_file && !descriptor.connection.uses_host);
        assert!(descriptor
            .connection
            .fields
            .iter()
            .any(|field| field.key == "file" && field.required));
    }

    #[test]
    fn statements_are_classified_with_the_sqlite_dialect() {
        assert_eq!(statement("SELECT * FROM orders").worst(), Some(Category::Read));
        assert_eq!(statement("INSERT INTO orders (id) VALUES (1)").worst(), Some(Category::Write));
        assert_eq!(statement("CREATE INDEX i ON orders (id)").worst(), Some(Category::Ddl));
        assert_eq!(statement("DROP TABLE orders").worst(), Some(Category::Destructive));
        assert_eq!(statement("DELETE FROM orders").worst(), Some(Category::Destructive));
        assert_eq!(statement("PRAGMA table_info(orders)").worst(), Some(Category::Read));
        assert_eq!(statement("ATTACH DATABASE 'x.db' AS x").worst(), Some(Category::Admin));
        assert_eq!(statement("VACUUM").worst(), Some(Category::Admin));
        assert_eq!(statement("SELECT 1; DROP TABLE t").statements.len(), 2);
        let other = SQLITE.classify(&ClassifyInput::Statement {
            language: "es-console",
            text: "GET /",
        });
        assert_eq!(other.worst(), Some(Category::Unknown));
    }

    #[test]
    fn operations_are_classified_and_unknown_names_refused() {
        let args = serde_json::json!({ "sql": "DELETE FROM t" });
        let explain = SQLITE.classify(&ClassifyInput::Capability {
            name: "sqlite.explain",
            args: &args,
        });
        assert_eq!(explain.worst(), Some(Category::Read));
        let null = serde_json::Value::Null;
        let unknown = SQLITE.classify(&ClassifyInput::Capability {
            name: "sqlite.vacuum",
            args: &null,
        });
        assert_eq!(unknown.refusal(), Some("unknown operation"));
    }

    #[test]
    fn uris_become_a_file_and_options_and_back() {
        let parsed = SQLITE
            .parse_connection_string("file:///C:/data/shop.db?mode=ro&busy_timeout=250")
            .unwrap();
        assert_eq!(parsed.spec.engine, "sqlite");
        assert_eq!(parsed.spec.file.as_deref(), Some("C:/data/shop.db"));
        assert_eq!(parsed.spec.option("readOnly"), Some("true"));
        assert_eq!(parsed.spec.option("busyTimeout"), Some("250"));
        assert!(parsed.password.is_none() && parsed.spec.url.is_none());
        assert_eq!(SQLITE.parse_connection_string("shop.db").unwrap().spec.options.len(), 0);
        assert!(SQLITE.parse_connection_string("   ").is_err());
        let url = SQLITE.format_connection_string(&parsed.spec).unwrap();
        assert_eq!(url, "sqlite:///C:/data/shop.db?mode=ro&busy_timeout=250");
        assert_eq!(SQLITE.parse_connection_string(&url).unwrap().spec, parsed.spec);
        let unix = ConnectionSpec {
            file: Some("/var/db/my shop.db".into()),
            ..Default::default()
        };
        assert_eq!(
            SQLITE.format_connection_string(&unix).as_deref(),
            Some("sqlite:///var/db/my%20shop.db")
        );
        assert!(SQLITE.format_connection_string(&ConnectionSpec::default()).is_none());
    }

    #[tokio::test]
    async fn a_missing_file_is_a_connection_error_and_a_file_is_required() {
        let Err(error) = SQLITE.connect(request(Some("Z:/definitely/missing/file.db"), &[])).await else {
            panic!("the file does not exist");
        };
        assert_eq!(error.code(), Some("CONNECTION_ERROR"), "{error:?}");
        let Err(empty) = SQLITE.connect(request(None, &[])).await else {
            panic!("a file is required");
        };
        assert!(empty.to_string().contains("Choose"));
    }

    #[tokio::test]
    async fn a_memory_database_is_one_database_for_the_grid_and_the_tabs() {
        use crate::contract::{CollectSink, RunOptions, RunRequest};
        let connection = SQLITE.connect(request(Some(":memory:"), &[])).await.unwrap();
        let ctx = CallContext::default();
        let tab = connection.open_session(&SessionOptions::default(), &ctx).await.unwrap().session;
        let mut sink = CollectSink::default();
        let script = RunRequest {
            text: "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t (name) VALUES ('a'), ('b')".into(),
            database: None,
            offset: 0,
        };
        tab.run(&script, &RunOptions::default(), &mut sink).await.unwrap();
        let page = connection
            .tabular()
            .unwrap()
            .browse(
                &BrowseRequest::new(ObjectRef::new(Some("main"), None, "t", crate::contract::NodeKind::TABLE), 10),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 2);
        let tree = connection.children(&TreeRequest::default(), &ctx).await.unwrap();
        assert_eq!(tree.nodes[0].label, "main");
    }
}
