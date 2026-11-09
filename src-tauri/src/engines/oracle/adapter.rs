//! The Oracle engine on the contract. A connection keeps one [`OraEngine`] (one Oracle session) for the tree, describe,
//! browsing and edits, and gives every query tab an Oracle session of its own, which is what makes a transaction span
//! runs and a cancel (OCIBreak) hit only that tab. Oracle schemas (users) are the "databases" of the shared UI; see
//! `nodes.rs`.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;

use super::{
    connect, describe_sync, descriptor, graph, nodes,
    run::{OraCanceller, OraContractSession},
    tabular::{self, app},
    vector, OraEngine, OraOptions,
};
use crate::{
    contract::{
        classify_common, require, tabular::ApplyResult, vector::OPERATIONS as VECTOR_OPERATIONS, BrowseRequest, CallContext, Canceller,
        Capability, CapabilitySet, Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, Engine,
        EngineDescriptor, NodeKind, ObjectDescription, ObjectRef, OpenedSession, Page, ParsedConnection, RowChangeSet, SchemaGraph,
        SchemaGraphSource, ServerInfo, SessionOptions, StatementInfo, TabularData, TlsMode, TreeChildren, TreeRequest,
    },
    engines::common::{
        adapt::{description_to_contract, tls_to_contract},
        classify,
        split::Dialect,
        StatementKind,
    },
    error::{AppError, AppResult},
};

pub struct OracleAdapter;

pub static ORACLE: OracleAdapter = OracleAdapter;

/// Upper-case words of a PL/SQL block, string contents included (`EXECUTE IMMEDIATE 'DROP TABLE t'`).
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '#'))
        .filter(|word| !word.is_empty())
        .map(str::to_uppercase)
        .collect()
}

/// PL/SQL blocks are writes unless their text shows more: dropping or truncating makes them destructive, and user, grant
/// and system changes make them administrative. Plain words are matched, also inside strings (dynamic SQL), so a block can
/// be ranked higher than it needs, never lower.
fn rank_plsql(statement: &mut crate::engines::common::classify::ClassifiedStatement) {
    let words = words(&statement.text);
    if !matches!(words.first().map(String::as_str), Some("BEGIN" | "DECLARE")) {
        return;
    }
    let pair = |a: &str, b: &[&str]| words.windows(2).any(|w| w[0] == a && b.contains(&w[1].as_str()));
    let has = |w: &str| words.iter().any(|x| x == w);
    if pair("ALTER", &["SYSTEM", "USER", "DATABASE"])
        || pair("CREATE", &["USER", "ROLE"])
        || pair("DROP", &["USER", "ROLE"])
        || has("GRANT")
        || has("REVOKE")
    {
        statement.kind = StatementKind::Admin;
        statement.reason = Some("the PL/SQL block changes users, grants or the system".into());
    } else if has("DROP") || has("TRUNCATE") || has("PURGE") {
        statement.kind = StatementKind::Destructive;
        statement.reason = Some("the PL/SQL block drops or truncates".into());
    } else if statement.reason.is_none() {
        statement.reason = Some("PL/SQL block (treated as a write)".into());
    }
}

/// Oracle's statement classification: the shared SQL rules plus [`rank_plsql`] for anonymous blocks.
pub fn classify_sql(text: &str) -> Classification {
    let mut core = classify(Dialect::Oracle, text);
    for statement in &mut core.statements {
        rank_plsql(statement);
    }
    core.into()
}

#[async_trait]
impl Engine for OracleAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: "sql", text } => classify_sql(text),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("Oracle sessions do not run the '{language}' language.")),
            ),
            // The vector operations only read (searches run in a read-only transaction).
            ClassifyInput::Capability { name, .. } if VECTOR_OPERATIONS.contains(name) => {
                Classification::single(StatementInfo::new(Category::Read, name, name))
            }
            ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::refused(name, name, "unknown operation")),
            _ => Classification::default(),
        }
    }

    /// `oracle://user:pass@host:1521/SERVICE`, `user/pass@//host:1521/SERVICE`, `user/pass@alias` or EZConnect, through
    /// [`OraOptions::parse`]. A descriptor or alias lands in `options.connectString`.
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = OraOptions::parse(text)?;
        let defaults = OraOptions::default();
        let mut options = HashMap::new();
        if parsed.connect_timeout != defaults.connect_timeout {
            options.insert("connectTimeout".to_owned(), parsed.connect_timeout.as_secs().to_string());
        }
        let database = match parsed.connect_string {
            Some(connect_string) => {
                options.insert("connectString".to_owned(), connect_string);
                None
            }
            None => Some(parsed.service),
        };
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: descriptor::ID.into(),
                host: parsed.host,
                port: parsed.port,
                user: parsed.user,
                database,
                tls: tls_to_contract(&parsed.tls),
                options,
                ..Default::default()
            },
            password: parsed.password,
        })
    }

    /// The URL of a profile, without the password (`None` for a connect descriptor or TNS alias, which has no URL form).
    fn format_connection_string(&self, spec: &ConnectionSpec) -> Option<String> {
        if spec.option("connectString").is_some() {
            return None;
        }
        let mut url = String::from("oracle://");
        if !spec.user.is_empty() {
            url.push_str(&encode(&spec.user));
            url.push('@');
        }
        url.push_str(if spec.host.is_empty() { "localhost" } else { &spec.host });
        url.push_str(&format!(":{}", if spec.port == 0 { 1521 } else { spec.port }));
        url.push('/');
        url.push_str(spec.database.as_deref().filter(|name| !name.is_empty()).unwrap_or("FREEPDB1"));
        let mut query = Vec::new();
        if spec.tls.as_ref().is_some_and(|tls| tls.mode != TlsMode::Disable) {
            query.push("ssl=true".to_owned());
        }
        if let Some(seconds) = spec.option("connectTimeout") {
            query.push(format!("connect_timeout={seconds}"));
        }
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }
        Some(url)
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let options = connect::build_options(&request)?;
        connect::ensure_client(request.spec.option(connect::CLIENT_DIR)).await?;
        let engine = OraEngine::connect(options).await.map_err(connect::connect_error)?;
        let mut capabilities = self.descriptor().capabilities;
        if !vector::vector_ready(engine.server_version()) {
            capabilities = capabilities.without(Capability::Vector);
        }
        Ok(Arc::new(OracleConnection {
            info: connect::server_info(engine.server_version()),
            capabilities,
            engine: Arc::new(engine),
        }))
    }
}

/// Percent-encodes a URL component (everything but unreserved characters).
fn encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// An open Oracle database.
pub struct OracleConnection {
    info: ServerInfo,
    capabilities: CapabilitySet,
    engine: Arc<OraEngine>,
}

impl OracleConnection {
    fn login(&self) -> String {
        self.engine.current_schema().to_owned()
    }
}

#[async_trait]
impl Connection for OracleConnection {
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
        let (request, login) = (request.clone(), self.login());
        self.engine.on_conn(move |c| nodes::children(c, &request, &login)).await
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        let login = self.login();
        let core = nodes::to_core(object, &login)?;
        self.engine
            .on_conn(move |c| {
                let described = describe_sync(c, &core, &login).map_err(app)?;
                let owner = described.object.schema.clone().unwrap_or_else(|| login.clone());
                let object = nodes::to_contract(&described.object, &login);
                let mut description = description_to_contract(described, None);
                description.object = object;
                for key in &mut description.foreign_keys {
                    key.ref_database = key.ref_schema.take();
                }
                if description.object.kind == NodeKind::TABLE || description.object.kind == NodeKind::MATERIALIZED_VIEW {
                    description.referenced_by = graph::referencing(c, &owner, &description.object.name)?;
                }
                Ok(description)
            })
            .await
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        if let Some(language) = options.language.as_deref().filter(|language| *language != "sql") {
            return Err(AppError::new(format!("Oracle sessions do not run the '{language}' language.")));
        }
        let session = self.engine.open_session().await.map_err(app)?;
        let canceller: Arc<dyn Canceller> = Arc::new(OraCanceller(session.canceller()));
        let session = OraContractSession::new(session);
        session.use_schema(options.database.as_deref()).await?;
        Ok(OpenedSession {
            session: Arc::new(session),
            canceller: Some(canceller),
        })
    }

    async fn invoke(&self, name: &str, args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        if !VECTOR_OPERATIONS.contains(&name) {
            return Err(crate::contract::unsupported_capability(name));
        }
        require(self.capabilities, Capability::Vector)?;
        vector::invoke(self.engine.clone(), self.login(), name, args).await
    }

    fn tabular(&self) -> Option<&dyn TabularData> {
        Some(self)
    }

    fn schema_graph(&self) -> Option<&dyn SchemaGraphSource> {
        Some(self)
    }

    async fn ping(&self) -> AppResult<()> {
        self.engine.on_conn(|c| c.ping().map_err(|error| app(super::map_err(error)))).await
    }

    async fn close(&self) {}
}

#[async_trait]
impl TabularData for OracleConnection {
    async fn browse(&self, request: &BrowseRequest, _ctx: &CallContext) -> AppResult<Page> {
        let (request, login) = (request.clone(), self.login());
        self.engine.on_conn(move |c| tabular::browse_sync(c, &request, &login)).await
    }

    async fn apply(&self, changes: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        require(self.capabilities, Capability::EditRows)?;
        if changes.changes.is_empty() {
            return Ok(ApplyResult::default());
        }
        let (changes, login) = (changes.clone(), self.login());
        self.engine.on_conn(move |c| tabular::apply_sync(c, &changes, &login)).await
    }
}

#[async_trait]
impl SchemaGraphSource for OracleConnection {
    async fn schema_graph(&self, database: &str, _ctx: &CallContext) -> AppResult<SchemaGraph> {
        let owner = if database.is_empty() { self.login() } else { database.to_owned() };
        self.engine.on_conn(move |c| graph::schema_graph(c, &owner)).await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::contract::{ConnectSecrets, Endpoint, EngineStatus};

    fn statement(text: &str) -> Classification {
        ORACLE.classify(&ClassifyInput::Statement { language: "sql", text })
    }

    #[test]
    fn descriptor_is_ready_and_consistent() {
        let descriptor = ORACLE.descriptor();
        assert_eq!(ORACLE.id(), "oracle");
        assert_eq!(descriptor.status, EngineStatus::Ready);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("sql"));
        for capability in [
            Capability::MultipleDatabases,
            Capability::EditRows,
            Capability::Cancel,
            Capability::StreamingResults,
            Capability::SchemaGraph,
        ] {
            assert!(descriptor.capabilities.contains(capability), "{capability:?}");
        }
        assert!(
            !descriptor.capabilities.contains(Capability::Schemas),
            "schemas are the databases of the tree"
        );
        assert!(descriptor
            .connection
            .fields
            .iter()
            .any(|field| field.key == format!("options.{}", connect::CLIENT_DIR)));
    }

    #[test]
    fn statements_are_classified_with_the_oracle_rules() {
        assert_eq!(statement("SELECT * FROM dual").worst(), Some(Category::Read));
        assert_eq!(
            statement("UPDATE orders SET status = 'PAID' WHERE id = 1").worst(),
            Some(Category::Write)
        );
        assert_eq!(statement("CREATE TABLE t (id NUMBER)").worst(), Some(Category::Ddl));
        assert_eq!(statement("TRUNCATE TABLE orders").worst(), Some(Category::Destructive));
        assert_eq!(statement("DROP TABLE orders PURGE").worst(), Some(Category::Destructive));
        assert_eq!(statement("ALTER SYSTEM SET open_cursors = 500").worst(), Some(Category::Admin));
        assert_eq!(statement("ALTER SESSION SET CURRENT_SCHEMA = shop").worst(), Some(Category::Read));
        assert_eq!(
            statement("CREATE OR REPLACE PROCEDURE p IS BEGIN NULL; END;").worst(),
            Some(Category::Ddl)
        );
    }

    #[test]
    fn plsql_blocks_are_writes_unless_they_show_more() {
        let block = statement("BEGIN\n  DBMS_OUTPUT.PUT_LINE('x');\nEND;");
        assert_eq!(block.worst(), Some(Category::Write));
        assert!(block.statements[0].reason.as_deref().unwrap().contains("PL/SQL"));
        assert_eq!(statement("DECLARE n NUMBER; BEGIN n := 1; END;").worst(), Some(Category::Write));
        assert_eq!(
            statement("BEGIN EXECUTE IMMEDIATE 'DROP TABLE t PURGE'; END;").worst(),
            Some(Category::Destructive)
        );
        assert_eq!(
            statement("BEGIN EXECUTE IMMEDIATE 'truncate table t'; END;").worst(),
            Some(Category::Destructive)
        );
        assert_eq!(
            statement("BEGIN EXECUTE IMMEDIATE 'ALTER SYSTEM FLUSH SHARED_POOL'; END;").worst(),
            Some(Category::Admin)
        );
        assert_eq!(
            statement("BEGIN EXECUTE IMMEDIATE 'GRANT DBA TO bob'; END;").worst(),
            Some(Category::Admin)
        );
        // A column called DROPPED is not a DROP.
        assert_eq!(
            statement("BEGIN UPDATE t SET dropped = 1 WHERE id = 1; END;").worst(),
            Some(Category::Write)
        );
        let script = statement("BEGIN NULL; END;\n/\nSELECT 1 FROM dual");
        assert_eq!(script.statements.len(), 2);
        assert_eq!(script.statements[1].category, Category::Read);
    }

    #[test]
    fn unknown_operations_are_refused_and_vector_operations_read() {
        let args = serde_json::Value::Null;
        let classification = ORACLE.classify(&ClassifyInput::Capability {
            name: "oracle.awr",
            args: &args,
        });
        assert_eq!(classification.refusal(), Some("unknown operation"));
        for name in VECTOR_OPERATIONS {
            let vector = ORACLE.classify(&ClassifyInput::Capability { name, args: &args });
            assert_eq!(vector.worst(), Some(Category::Read), "{name}");
            assert!(vector.refusal().is_none());
        }
    }

    #[test]
    fn connect_strings_split_into_a_spec_and_a_password() {
        let parsed = ORACLE
            .parse_connection_string("oracle://scott:tiger@db.local:1522/ORCLPDB1?ssl=true")
            .unwrap();
        assert_eq!(parsed.password.as_deref(), Some("tiger"));
        assert_eq!(parsed.spec.engine, "oracle");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("db.local", 1522));
        assert_eq!(parsed.spec.user, "scott");
        assert_eq!(parsed.spec.database.as_deref(), Some("ORCLPDB1"));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::VerifyFull));
        assert!(parsed.spec.options.values().all(|value| !value.contains("tiger")));
        assert!(parsed.spec.url.is_none());

        let alias = ORACLE.parse_connection_string("scott/tiger@PRODDB").unwrap();
        assert_eq!(alias.spec.option("connectString"), Some("PRODDB"));
        assert!(alias.spec.database.is_none());
        assert_eq!(alias.password.as_deref(), Some("tiger"));
        assert!(ORACLE.parse_connection_string("db.local:1521/FREEPDB1").is_err());
    }

    #[test]
    fn a_profile_formats_back_into_a_url_without_the_password() {
        let parsed = ORACLE
            .parse_connection_string("oracle://shop:secret@db.local:1522/ORCLPDB1?ssl=true&connect_timeout=5")
            .unwrap();
        let url = ORACLE.format_connection_string(&parsed.spec).unwrap();
        assert_eq!(url, "oracle://shop@db.local:1522/ORCLPDB1?ssl=true&connect_timeout=5");
        assert_eq!(ORACLE.parse_connection_string(&url).unwrap().spec, parsed.spec);
        let alias = ORACLE.parse_connection_string("scott/tiger@PRODDB").unwrap();
        assert!(ORACLE.format_connection_string(&alias.spec).is_none());
    }

    #[tokio::test]
    async fn a_client_folder_without_the_library_is_a_structured_error() {
        if oracle::InitParams::is_initialized() {
            return; // another test in this process loaded a client
        }
        // An empty folder: connect refuses before loading anything, so no client gets loaded into this test process.
        let empty = std::env::temp_dir().join("fosta-oracle-adapter-no-client");
        std::fs::create_dir_all(&empty).unwrap();
        let mut spec = ConnectionSpec {
            engine: "oracle".into(),
            user: "shop".into(),
            ..ConnectionSpec::default()
        };
        spec.options
            .insert(connect::CLIENT_DIR.into(), empty.to_string_lossy().into_owned());
        let request = ConnectRequest {
            spec,
            secrets: ConnectSecrets::default(),
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: 1,
            },
            connect_timeout: Duration::from_secs(2),
        };
        let Err(error) = ORACLE.connect(request).await else {
            panic!("the folder has no client");
        };
        assert_eq!(error.code(), Some("ORACLE_CLIENT"), "{error:?}");
        assert_eq!(error.field("kind").and_then(|v| v.as_str()), Some("notInstalled"));
        assert_eq!(error.field("engine").and_then(|v| v.as_str()), Some("oracle"));
    }
}
