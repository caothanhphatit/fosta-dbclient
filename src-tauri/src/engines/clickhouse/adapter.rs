//! The ClickHouse engine port: one `Connection` over the HTTP interface (`ChEngine`), a lazy object tree, typed browsing
//! with keyset paging, guarded edits (mutations, see `edit.rs`), query sessions that stream line by line and stop with
//! `KILL QUERY`, and the engine operations behind the ClickHouse views (`ops.rs`).

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use futures_util::future::BoxFuture;

use super::{
    connect, descriptor,
    edit::{engine_of, read_only_reason},
    ops::OPERATIONS,
    session::{ChCancel, ChSession},
    vector::OPERATIONS as VECTOR_OPERATIONS,
    ChDialect, ChEngine, ChOptions,
};
use crate::{
    contract::{
        classify_common, require, ApplyResult, BrowseRequest, CallContext, Canceller, Capability, CapabilitySet, Category, Classification,
        ClassifyInput, ConnectRequest, Connection, ConnectionSpec, DatabaseAdmin, Engine, EngineDescriptor, NodeAction, ObjectDescription,
        ObjectRef, OpenedSession, Page, ParsedConnection, ResultSink, RowChangeSet, RunOptions, RunRequest, ServerInfo, Session,
        SessionOptions, StatementInfo, TabularData, TreeChildren, TreeRequest,
    },
    engines::common::{
        adapt::{self, tls_to_contract},
        classify,
        split::Dialect,
        sqlbuild::SqlDialect,
    },
    error::{AppError, AppResult, ErrorPosition},
};

pub struct ClickHouseAdapter;

pub static CLICKHOUSE: ClickHouseAdapter = ClickHouseAdapter;

/// The classification of an engine-view operation; unknown names are refused.
fn classify_capability(name: &str, args: &serde_json::Value) -> StatementInfo {
    let excerpt = format!("{name} {args}");
    match name {
        "clickhouse.parts" | "clickhouse.mutations" | "clickhouse.processes" | "clickhouse.queryLog" => {
            StatementInfo::new(Category::Read, name, &excerpt)
        }
        "clickhouse.kill" => StatementInfo::new(Category::Admin, name, &excerpt).with_reason("Stops a running query or mutation."),
        // The vector operations only read (every statement runs with readonly = 2).
        _ if VECTOR_OPERATIONS.contains(&name) => StatementInfo::new(Category::Read, name, &excerpt),
        _ => StatementInfo::refused(name, name, "unknown operation"),
    }
}

#[async_trait]
impl Engine for ClickHouseAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify(Dialect::ClickHouse, text).into(),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("ClickHouse sessions do not run the '{language}' language.")),
            ),
            ClassifyInput::Capability { name, args } => Classification::single(classify_capability(name, args)),
            _ => Classification::default(),
        }
    }

    /// `clickhouse://user:pass@host:port/db?secure=true` or `http(s)://host:port/?user=…`, through [`ChOptions::parse`].
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = ChOptions::parse(text)?;
        let mut options = HashMap::new();
        if parsed.connect_timeout != ChOptions::default().connect_timeout {
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

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let prepared = connect::prepare(&request)?;
        let engine = ChEngine::connect_with(prepared.options, prepared.tls).await?;
        let version = engine.server_version().to_owned();
        Ok(Arc::new(ClickHouseConnection {
            info: ServerInfo {
                product: descriptor::LABEL.into(),
                label: format!("{} {version}", descriptor::LABEL),
                version,
                flavor: None,
            },
            capabilities: self.descriptor().capabilities,
            engine,
        }))
    }
}

/// An open ClickHouse server.
pub struct ClickHouseConnection {
    info: ServerInfo,
    capabilities: CapabilitySet,
    engine: ChEngine,
}

#[async_trait]
impl Connection for ClickHouseConnection {
    fn engine_id(&self) -> &str {
        descriptor::ID
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    fn connection_actions(&self) -> Vec<NodeAction> {
        let mut action = NodeAction::view("server", "Server: running queries and query log", "server");
        action.icon = Some("server".into());
        vec![action]
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        self.engine.tree_children(request).await
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        let mut core = adapt::object_to_common(object)?;
        if core.database.is_none() {
            core.database = Some(self.engine.default_database().to_owned());
        }
        let description = self.engine.describe(&core).await?;
        let reason = read_only_reason(&description);
        let engine = engine_of(&description).map(str::to_owned);
        let database = core.database.clone();
        let mut described = adapt::description_to_contract(description, database.as_deref());
        match (&reason, engine) {
            (Some(reason), _) if described.object.kind == crate::contract::NodeKind::TABLE => {
                for column in &mut described.columns {
                    column.hints.read_only = true;
                }
                described.extra.push(("edits".into(), format!("read-only: {reason}")));
            }
            (None, _) => described.extra.push((
                "edits".into(),
                "asynchronous mutations: ALTER TABLE … UPDATE and DELETE FROM, one after another, no transaction".into(),
            )),
            _ => {}
        }
        Ok(described)
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        if let Some(language) = options.language.as_deref().filter(|language| *language != "sql") {
            return Err(AppError::new(format!("ClickHouse sessions do not run the '{language}' language.")));
        }
        let session = self.engine.open_session()?;
        let canceller: Arc<dyn Canceller> = Arc::new(ChCanceller(session.canceller()));
        Ok(OpenedSession {
            session: Arc::new(ChContractSession {
                inner: session,
                database: options.database.clone(),
            }),
            canceller: Some(canceller),
        })
    }

    async fn invoke(&self, name: &str, args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        if VECTOR_OPERATIONS.contains(&name) {
            require(self.capabilities, Capability::Vector)?;
            return self.engine.vector(name, args).await;
        }
        if !OPERATIONS.contains(&name) {
            return Err(crate::contract::unsupported_capability(name));
        }
        require(self.capabilities, Capability::ServerOverview)?;
        self.engine.invoke_operation(name, &args).await
    }

    fn tabular(&self) -> Option<&dyn TabularData> {
        Some(self)
    }

    fn admin(&self) -> Option<&dyn DatabaseAdmin> {
        Some(self)
    }

    async fn ping(&self) -> AppResult<()> {
        self.engine.http().query("SELECT 1", &[]).await?;
        Ok(())
    }

    async fn close(&self) {}
}

#[async_trait]
impl TabularData for ClickHouseConnection {
    async fn browse(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        self.engine.browse_page(request, ctx.limits.timeout_secs).await
    }

    async fn apply(&self, changes: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        require(self.capabilities, Capability::EditRows)?;
        self.engine.apply(changes).await
    }
}

#[async_trait]
impl DatabaseAdmin for ClickHouseConnection {
    async fn create_database(&self, name: &str, _first_collection: Option<&str>, _ctx: &CallContext) -> AppResult<()> {
        let sql = format!("CREATE DATABASE {}", ChDialect.quote_ident(name));
        self.engine.http().execute(&sql, &[]).await?;
        Ok(())
    }
}

/// A query tab: statements run one request at a time, in the tab's HTTP session.
struct ChContractSession {
    inner: ChSession,
    /// The database the tab was opened on; a run's own database wins.
    database: Option<String>,
}

#[async_trait]
impl Session for ChContractSession {
    fn language(&self) -> &str {
        "sql"
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        let database = request.database.as_deref().or(self.database.as_deref());
        self.inner
            .run_script(&request.text, database, options, sink)
            .await
            .map_err(|failed| {
                let mut error = failed.error;
                // The server counts characters from the start of the statement; the editor from the start of the text.
                error.position = error.position.map(|offset| offset + (failed.start + request.offset) as u32);
                let position = error.position;
                let mut mapped = AppError::from(error).with_statement(failed.index, &failed.text);
                if let Some(offset) = position {
                    mapped = mapped.with_position(ErrorPosition {
                        offset: Some(offset),
                        ..ErrorPosition::default()
                    });
                }
                mapped
            })
    }
}

/// `KILL QUERY WHERE query_id = …` on a request of its own, so it does not wait for the session.
struct ChCanceller(ChCancel);

impl Canceller for ChCanceller {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>> {
        let cancel = self.0.clone();
        Box::pin(async move { Ok(cancel.cancel().await?) })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::contract::{ConnectSecrets, Endpoint, EngineStatus, TlsMode};

    fn statement(text: &str) -> Classification {
        CLICKHOUSE.classify(&ClassifyInput::Statement { language: "sql", text })
    }

    fn capability(name: &str) -> Classification {
        let args = json!({ "queryId": "q1" });
        CLICKHOUSE.classify(&ClassifyInput::Capability { name, args: &args })
    }

    #[test]
    fn descriptor_is_ready_and_consistent() {
        let descriptor = CLICKHOUSE.descriptor();
        assert_eq!(CLICKHOUSE.id(), "clickhouse");
        assert_eq!(descriptor.status, EngineStatus::Ready);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("sql"));
        assert!(!descriptor.capabilities.contains(Capability::Transactions));
        assert!(descriptor.capabilities.contains(Capability::Cancel) && descriptor.capabilities.contains(Capability::StreamingResults));
    }

    #[test]
    fn statements_are_classified_with_the_clickhouse_dialect() {
        let reads = [
            "SELECT count() FROM events",
            "SHOW TABLES FROM shop",
            "SHOW CREATE TABLE shop.orders",
            "DESCRIBE TABLE shop.orders",
            "DESC shop.orders",
            "EXPLAIN SELECT 1",
            "EXPLAIN PIPELINE SELECT 1",
            "EXISTS TABLE shop.orders",
            "WITH 1 AS x SELECT x",
        ];
        for sql in reads {
            assert_eq!(statement(sql).worst(), Some(Category::Read), "{sql}");
        }
        let writes = [
            "INSERT INTO events VALUES (1)",
            "INSERT INTO events (id) SELECT 1",
            "ALTER TABLE events DELETE WHERE id = 1",
            "ALTER TABLE events UPDATE a = 1 WHERE id = 1",
            "DELETE FROM events WHERE id = 1",
        ];
        for sql in writes {
            assert_eq!(statement(sql).worst(), Some(Category::Write), "{sql}");
        }
        let ddl = [
            "CREATE TABLE t (id UInt64) ENGINE = MergeTree ORDER BY id",
            "CREATE VIEW v AS SELECT 1",
            "ALTER TABLE t ADD COLUMN c String",
        ];
        for sql in ddl {
            assert_eq!(statement(sql).worst(), Some(Category::Ddl), "{sql}");
        }
        let destructive = [
            "DROP TABLE events",
            "DROP DATABASE shop",
            "TRUNCATE TABLE events",
            "ALTER TABLE events DROP PARTITION 202501",
            "ALTER TABLE events DETACH PARTITION 202501",
            "ALTER TABLE events DROP COLUMN c",
            "DELETE FROM events",
        ];
        for sql in destructive {
            assert_eq!(statement(sql).worst(), Some(Category::Destructive), "{sql}");
        }
        // SYSTEM, KILL and OPTIMIZE are server operations: the gateway's own "administrative" rule asks for a typed
        // confirmation under the default rules, like for destructive statements.
        for sql in [
            "SYSTEM RELOAD DICTIONARIES",
            "SYSTEM DROP DNS CACHE",
            "KILL QUERY WHERE query_id = 'x'",
            "OPTIMIZE TABLE t FINAL",
        ] {
            assert_eq!(statement(sql).worst(), Some(Category::Admin), "{sql}");
        }
        assert_eq!(
            statement("SELECT 1; SYSTEM FLUSH LOGS; DROP TABLE t").worst(),
            Some(Category::Admin),
            "the worst statement decides"
        );
        assert_eq!(statement("SELECT 1; DROP TABLE t").worst(), Some(Category::Destructive));
        assert_eq!(
            statement("SELECT 1; ALTER TABLE t UPDATE a = 1 WHERE 1").worst(),
            Some(Category::Write)
        );
    }

    #[test]
    fn engine_view_operations_are_classified_and_unknown_ones_refused() {
        let readers = OPERATIONS.iter().chain(VECTOR_OPERATIONS.iter());
        for name in readers.filter(|name| **name != "clickhouse.kill") {
            assert_eq!(capability(name).worst(), Some(Category::Read), "{name}");
            assert_eq!(capability(name).refusal(), None);
        }
        assert_eq!(capability("clickhouse.kill").worst(), Some(Category::Admin));
        assert!(OPERATIONS.contains(&"clickhouse.kill"));
        assert_eq!(capability("clickhouse.dropEverything").refusal(), Some("unknown operation"));
    }

    #[test]
    fn urls_split_into_a_spec_and_a_password() {
        let parsed = CLICKHOUSE
            .parse_connection_string("clickhouse://analyst:p%40ss@ch.local/metrics?secure=true&connect_timeout=3")
            .unwrap();
        assert_eq!(parsed.password.as_deref(), Some("p@ss"));
        assert_eq!(parsed.spec.engine, "clickhouse");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("ch.local", 8443));
        assert_eq!(parsed.spec.user, "analyst");
        assert_eq!(parsed.spec.database.as_deref(), Some("metrics"));
        assert_eq!(parsed.spec.option("connectTimeout"), Some("3"));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::VerifyFull));
        assert!(parsed.spec.options.values().all(|value| !value.contains("p@ss")));
        assert!(parsed.spec.url.is_none());
        let http = CLICKHOUSE.parse_connection_string("http://localhost:8123/?password=x").unwrap();
        assert_eq!(http.password.as_deref(), Some("x"));
        assert!(http.spec.options.is_empty() && http.spec.tls.is_none());
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_connect_error() {
        let request = ConnectRequest {
            spec: ConnectionSpec {
                engine: "clickhouse".into(),
                host: "127.0.0.1".into(),
                port: 1,
                ..ConnectionSpec::default()
            },
            secrets: ConnectSecrets::default(),
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: 1,
            },
            connect_timeout: Duration::from_secs(1),
        };
        let Err(error) = CLICKHOUSE.connect(request).await else {
            panic!("nothing listens on port 1");
        };
        assert_eq!(error.code(), Some("CONNECTION_ERROR"), "{error:?}");
    }
}
