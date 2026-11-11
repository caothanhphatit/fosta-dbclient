//! The Redis engine port: descriptor, classification, connection URLs and the connection itself.
//!
//! One [`RedisEngine`] (a multiplexed connection) serves each logical database: the one from the profile is opened at connect,
//! the others on first use, so browsing `db3` never `SELECT`s on a connection somebody else is using. A console tab is a
//! session with a connection of its own.
//!
//! Engine views call these `invoke` names: `redis.info`, `redis.slowlog`, `redis.scan`, `redis.keyInfo`, `redis.readValue`
//! (reads) and `redis.edit` (a [`RedisEdit`] with an optional `expect`, classified by [`RedisEdit::class`]). Everything but
//! `redis.info` and `redis.slowlog` takes the database in `db`.

use std::{
    collections::HashMap,
    sync::{Arc, OnceLock},
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{
    catalog,
    classify::{self, CommandClass},
    connect,
    connection::{RedisConnectOptions, RedisEngine, ServerFlavor, DEFAULT_PORT},
    console::{split_lines, tokenize},
    descriptor,
    edit::RedisEdit,
    ops, profile,
    session::{RedisSession, LANGUAGE},
    vector::{self, RedisVectors, OPERATIONS as VECTOR_OPERATIONS, SEARCH_MODULE},
};
use crate::{
    contract::{
        classify_common, require, unsupported_capability, vector as shared_vector, AuthMethodId, CallContext, Capability, CapabilitySet,
        Category, Classification, ClassifyInput, ConnectRequest, Connection, ConnectionSpec, Engine, EngineDescriptor, EngineError,
        ExtrasRead, NodeAction, NodeKind, ObjectDescription, ObjectRef, OpenedSession, ParsedConnection, ProfileEnrichment, ProfileExtras,
        ServerInfo, SessionOptions, StatementInfo, TlsConfig, TlsMode, TreeChildren, TreeRequest,
    },
    error::{AppError, AppResult},
};

pub struct RedisAdapter;

pub static REDIS: RedisAdapter = RedisAdapter;

/// A core command class as a gateway category; `None` for commands the console cannot run (Pub/Sub, MONITOR).
/// Session commands (SELECT, MULTI/EXEC, CLIENT SETNAME) only change the console's connection, so they count as reads.
fn category(class: CommandClass) -> Option<Category> {
    match class {
        CommandClass::Session | CommandClass::Read => Some(Category::Read),
        CommandClass::Write => Some(Category::Write),
        CommandClass::Unknown => Some(Category::Unknown),
        CommandClass::Admin => Some(Category::Admin),
        CommandClass::Destructive => Some(Category::Destructive),
        CommandClass::Unsupported => None,
    }
}

/// Console text: one command per line (comments and blank lines skipped), each tokenized like redis-cli.
fn classify_console(text: &str) -> Classification {
    let statements = split_lines(text)
        .into_iter()
        .map(|(line_number, line)| match tokenize(line) {
            Ok(args) => {
                let command = classify::classify(&args);
                match category(command.class) {
                    Some(category) => {
                        let mut info = StatementInfo::new(category, &command.command, line);
                        info.reason = command.reason;
                        info
                    }
                    None => StatementInfo::refused(
                        &command.command,
                        line,
                        command.reason.as_deref().unwrap_or("The console cannot run this command."),
                    ),
                }
            }
            Err(error) => StatementInfo::new(Category::Unknown, "command", line).with_reason(&format!("Line {line_number}: {error}")),
        })
        .collect();
    Classification { statements }
}

/// A `redis.edit` payload, classified by the edit itself; malformed payloads are refused.
fn classify_edit(name: &str, args: &serde_json::Value) -> StatementInfo {
    let edit: RedisEdit = match serde_json::from_value(args.clone()) {
        Ok(edit) => edit,
        Err(error) => return StatementInfo::refused(name, name, &format!("invalid arguments: {error}")),
    };
    let op = args.get("op").and_then(|op| op.as_str()).unwrap_or_default();
    let keys: Vec<String> = match &edit {
        RedisEdit::Delete { keys } => keys.iter().map(|key| key.display()).collect(),
        other => other.key().map(|key| key.display()).into_iter().collect(),
    };
    let reason = match &edit {
        RedisEdit::Delete { .. } => Some("Deletes whole keys."),
        RedisEdit::Rename { overwrite: true, .. } => Some("Replaces the target key if it exists."),
        _ => None,
    };
    let category = category(edit.class()).unwrap_or(Category::Unknown);
    let mut info = StatementInfo::new(category, &format!("{name} {op}"), &format!("{name} {op} {}", keys.join(" "))).with_tables(keys);
    info.reason = reason.map(str::to_owned);
    info
}

fn classify_capability(name: &str, args: &serde_json::Value) -> StatementInfo {
    match name {
        "redis.info" | "redis.slowlog" | "redis.scan" | "redis.keyInfo" | "redis.readValue" => {
            StatementInfo::new(Category::Read, name, &format!("{name} {args}"))
        }
        "redis.edit" => classify_edit(name, args),
        // The vector operations only read (FT.INFO, FT.SEARCH, FT.EXPLAINCLI, HGET, JSON.GET).
        _ if VECTOR_OPERATIONS.contains(&name) => StatementInfo::new(Category::Read, name, &format!("{name} {args}")),
        _ => StatementInfo::refused(name, name, "unknown operation"),
    }
}

#[async_trait]
impl Engine for RedisAdapter {
    fn descriptor(&self) -> &EngineDescriptor {
        static DESCRIPTOR: OnceLock<EngineDescriptor> = OnceLock::new();
        DESCRIPTOR.get_or_init(descriptor::descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { language: LANGUAGE, text } => classify_console(text),
            ClassifyInput::Statement { language, text } => Classification::single(
                StatementInfo::new(Category::Unknown, language, text)
                    .with_reason(&format!("Redis consoles do not run the '{language}' language.")),
            ),
            ClassifyInput::Capability { name, args } => Classification::single(classify_capability(name, args)),
            _ => Classification::default(),
        }
    }

    /// `redis://[user[:password]@]host[:port][/db]` or `rediss://…`, through [`RedisConnectOptions::from_url`].
    fn parse_connection_string(&self, text: &str) -> AppResult<ParsedConnection> {
        let parsed = RedisConnectOptions::from_url(text)?;
        let mut options = HashMap::new();
        if parsed.db != 0 {
            options.insert("db".to_owned(), parsed.db.to_string());
        }
        let auth = match (&parsed.username, &parsed.password) {
            (Some(_), _) => Some(AuthMethodId::AclUser),
            (None, Some(_)) => Some(AuthMethodId::Password),
            (None, None) => None,
        };
        Ok(ParsedConnection {
            spec: ConnectionSpec {
                engine: descriptor::ID.into(),
                host: parsed.host,
                port: parsed.port.unwrap_or(DEFAULT_PORT),
                user: parsed.username.unwrap_or_default(),
                options,
                tls: parsed.tls.map(|_| TlsConfig {
                    mode: TlsMode::VerifyFull,
                    ..Default::default()
                }),
                auth,
                ..Default::default()
            },
            password: parsed.password,
        })
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let options = connect::options(&request)?;
        let base_db = options.db;
        let (host, port, wait) = (request.endpoint.host.clone(), request.endpoint.port, request.connect_timeout);
        let base = RedisEngine::connect(options)
            .await
            .map_err(|error| connect_failure(error, &host, port, wait))?;
        let server = base.server_info().await?;
        let product = match server.flavor {
            ServerFlavor::Valkey => "Valkey",
            ServerFlavor::Dragonfly => "Dragonfly",
            ServerFlavor::Redis | ServerFlavor::Unknown => descriptor::LABEL,
        };
        let version = if server.version.is_empty() {
            "unknown".to_owned()
        } else {
            server.version.clone()
        };
        let info = ServerInfo {
            product: product.to_owned(),
            label: format!("{product} {version}"),
            version,
            flavor: Some(server.mode.clone()),
        };
        // The Query Engine (Redis 8, or the search module): MODULE LIST, or FT._LIST where MODULE is not allowed.
        let search = base.has_module(SEARCH_MODULE) || base.query(&redis::cmd("FT._LIST")).await.is_ok();
        let mut capabilities = self.descriptor().capabilities;
        if !search {
            capabilities = capabilities.without(Capability::Vector);
        }
        Ok(Arc::new(RedisConnection {
            databases: server.databases.filter(|count| *count > 0).unwrap_or(catalog::DEFAULT_DATABASES),
            base,
            base_db,
            others: Mutex::new(HashMap::new()),
            vectors: Mutex::new(None),
            info,
            capabilities,
        }))
    }
}

/// A connect attempt that timed out is a connection problem (the host, port or firewall), not a slow command: Windows reports a
/// refused port only after its own retries, so the client's timeout often fires first.
fn connect_failure(error: AppError, host: &str, port: u16, wait: std::time::Duration) -> AppError {
    if error.code() != Some("TIMEOUT") {
        return error;
    }
    let seconds = wait.as_secs().max(1);
    AppError::from(EngineError::connect(format!(
        "Could not connect to Redis at {host}:{port}: no answer within {seconds} s. Check the host, the port and any firewall or tunnel."
    )))
    .with_engine(descriptor::ID)
}

/// An open Redis server: the engine of the profile's database, the engines of the databases browsed since.
pub struct RedisConnection {
    base: RedisEngine,
    base_db: i64,
    databases: u32,
    others: Mutex<HashMap<i64, RedisEngine>>,
    /// The RESP2 connection to database 0 the vector operations use (search indexes live there).
    vectors: Mutex<Option<RedisEngine>>,
    info: ServerInfo,
    capabilities: CapabilitySet,
}

impl RedisConnection {
    /// The engine for the vector operations: database 0 over RESP2, connecting on first use.
    async fn vector_engine(&self) -> AppResult<RedisEngine> {
        if self.base_db == 0 && !self.base.options().resp3 {
            return Ok(self.base.clone());
        }
        let mut vectors = self.vectors.lock().await;
        if let Some(engine) = vectors.as_ref() {
            return Ok(engine.clone());
        }
        let engine = RedisEngine::connect(RedisConnectOptions {
            db: 0,
            resp3: false,
            ..self.base.options().clone()
        })
        .await?;
        *vectors = Some(engine.clone());
        Ok(engine)
    }

    /// The engine of logical database `db`, connecting on first use.
    async fn engine_for(&self, db: i64) -> AppResult<RedisEngine> {
        if db == self.base_db {
            return Ok(self.base.clone());
        }
        if db < 0 || db >= i64::from(self.databases) {
            return Err(AppError::new(format!(
                "Database {db} does not exist: this server has databases 0 to {}.",
                self.databases - 1
            )));
        }
        let mut others = self.others.lock().await;
        if let Some(engine) = others.get(&db) {
            return Ok(engine.clone());
        }
        let engine = RedisEngine::connect(RedisConnectOptions {
            db,
            ..self.base.options().clone()
        })
        .await?;
        others.insert(db, engine.clone());
        Ok(engine)
    }

    /// `db3`, `3` or nothing (the profile's database).
    fn database_of_options(&self, database: Option<&str>) -> AppResult<i64> {
        match database.map(str::trim).filter(|text| !text.is_empty()) {
            None => Ok(self.base_db),
            Some(text) => text
                .trim_start_matches("db")
                .parse()
                .map_err(|_| AppError::new(format!("The Redis database is a number such as 0 or 3, not `{text}`."))),
        }
    }
}

#[async_trait]
impl Connection for RedisConnection {
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
        let mut action = NodeAction::view("server", "Server info and slow log", "server");
        action.icon = Some("server".into());
        vec![action]
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        match &request.parent {
            None => {
                let nodes = catalog::databases(&self.base, self.databases).await;
                Ok(TreeChildren {
                    total_hint: Some(nodes.len() as u64),
                    nodes,
                    next_cursor: None,
                })
            }
            Some(parent) if parent.kind == NodeKind::DATABASE => {
                let db = catalog::db_of_parent(parent)?;
                let mut page = catalog::keys_page(&self.engine_for(db).await?, db, request).await?;
                // Search indexes with vector fields (database 0 only) come first, on the unfiltered first page.
                let first_page = request.cursor.is_none() && request.filter.as_deref().is_none_or(|filter| filter.trim().is_empty());
                if db == 0 && first_page && self.capabilities.contains(Capability::Vector) {
                    let mut nodes = vector::index_nodes(&self.vector_engine().await?).await;
                    nodes.append(&mut page.nodes);
                    page.nodes = nodes;
                }
                Ok(page)
            }
            Some(_) => Ok(TreeChildren::default()),
        }
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        let db = catalog::db_of_object(object)?;
        catalog::describe(&self.engine_for(db).await?, object).await
    }

    async fn open_session(&self, options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        if let Some(language) = options.language.as_deref().filter(|language| *language != LANGUAGE) {
            return Err(AppError::new(format!("Redis consoles do not run the '{language}' language.")));
        }
        let db = self.database_of_options(options.database.as_deref())?;
        let (session, canceller) = RedisSession::open(self.engine_for(db).await?, db).await?;
        Ok(OpenedSession {
            session: Arc::new(session),
            canceller: Some(canceller),
        })
    }

    async fn invoke(&self, name: &str, args: serde_json::Value, _ctx: &CallContext) -> AppResult<serde_json::Value> {
        match name {
            "redis.info" => ops::info(&self.base).await,
            "redis.slowlog" => ops::slowlog(&self.base, &args).await,
            "redis.scan" | "redis.keyInfo" | "redis.readValue" | "redis.edit" => {
                let engine = self.engine_for(ops::db_of(&args)).await?;
                match name {
                    "redis.scan" => ops::scan(&engine, &args).await,
                    "redis.keyInfo" => Ok(serde_json::to_value(ops::key_info(&engine, &args).await?)?),
                    "redis.readValue" => Ok(serde_json::to_value(ops::read_value(&engine, &args).await?)?),
                    _ => ops::edit(&engine, &args).await,
                }
            }
            _ if VECTOR_OPERATIONS.contains(&name) => {
                require(self.capabilities, Capability::Vector)?;
                let mut store = RedisVectors::new(self.vector_engine().await?, true);
                shared_vector::invoke(&mut store, name, args).await
            }
            other => Err(unsupported_capability(other)),
        }
    }

    fn profile_extras(&self) -> Option<&dyn ProfileExtras> {
        Some(self)
    }

    async fn ping(&self) -> AppResult<()> {
        self.base.ping().await
    }

    async fn close(&self) {
        self.others.lock().await.clear();
        self.vectors.lock().await.take();
    }
}

impl ProfileExtras for RedisConnection {
    fn reads(&self, database: &str, _objects: &[ObjectRef]) -> Vec<ExtrasRead> {
        profile::reads(database)
    }

    fn interpret(&self, database: &str, answers: &[(ExtrasRead, serde_json::Value)]) -> ProfileEnrichment {
        profile::interpret(database, answers)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::contract::{ConnectSecrets, Endpoint, EngineStatus};

    fn console(text: &str) -> Classification {
        REDIS.classify(&ClassifyInput::Statement { language: "redis", text })
    }

    fn capability(name: &str, args: serde_json::Value) -> Classification {
        REDIS.classify(&ClassifyInput::Capability { name, args: &args })
    }

    #[test]
    fn descriptor_is_ready_and_consistent() {
        let descriptor = REDIS.descriptor();
        assert_eq!(REDIS.id(), "redis");
        assert_eq!(descriptor.status, EngineStatus::Ready);
        assert_eq!(descriptor.default_language().map(|language| language.id.as_str()), Some("redis"));
        assert!(descriptor.filter.raw.is_none());
        use crate::contract::Capability as C;
        for capability in [
            C::Tree,
            C::MultipleDatabases,
            C::KeyValue,
            C::Console,
            C::Query,
            C::Cancel,
            C::ServerOverview,
        ] {
            assert!(descriptor.capabilities.contains(capability), "{capability:?}");
        }
        // Keys are not rows: no grid browsing or editing, no transactions toolbar.
        for capability in [C::BrowseRows, C::EditRows, C::Transactions, C::SchemaGraph, C::CreateDatabase] {
            assert!(!descriptor.capabilities.contains(capability), "{capability:?}");
        }
    }

    #[test]
    fn console_lines_are_classified_one_by_one() {
        assert_eq!(console("GET user:1").worst(), Some(Category::Read));
        assert_eq!(console("SELECT 2").worst(), Some(Category::Read));
        assert_eq!(console("HSET user:1 name \"Ada Lovelace\"").worst(), Some(Category::Write));
        assert_eq!(console("FLUSHALL").worst(), Some(Category::Destructive));
        assert_eq!(console("CONFIG SET maxmemory 1gb").worst(), Some(Category::Admin));
        let script = console("# warm up\nGET a\n\nDEL a b\n");
        assert_eq!(script.statements.len(), 2);
        assert_eq!(script.statements[1].kind, "DEL");
        assert_eq!(console("SET k \"unclosed").worst(), Some(Category::Unknown));
    }

    #[test]
    fn dangerous_commands_need_the_strict_categories_and_writes_are_writes() {
        for dangerous in [
            "FLUSHALL",
            "FLUSHDB",
            "FLUSHDB ASYNC",
            "CONFIG SET maxmemory 1",
            "KEYS *",
            "DEBUG SLEEP 1",
            "SHUTDOWN NOSAVE",
        ] {
            let worst = console(dangerous).worst();
            assert!(
                matches!(worst, Some(Category::Destructive | Category::Admin)),
                "{dangerous}: {worst:?}"
            );
        }
        for write in [
            "SET k v",
            "DEL k",
            "HSET h f v",
            "LPUSH l a",
            "SADD s a",
            "ZADD z 1 a",
            "XADD s * f v",
            "EXPIRE k 10",
            "RENAME a b",
        ] {
            assert_eq!(console(write).worst(), Some(Category::Write), "{write}");
        }
        for read in ["GET k", "KEYS user:*", "SCAN 0 MATCH user:*", "HGETALL h", "TTL k"] {
            assert_eq!(console(read).worst(), Some(Category::Read), "{read}");
        }
        // Reads that expose secrets or other clients' traffic are administration.
        for admin in [
            "CONFIG GET requirepass",
            "ACL LIST",
            "ACL GETUSER default",
            "CLIENT LIST",
            "SLOWLOG GET 10",
        ] {
            assert_eq!(console(admin).worst(), Some(Category::Admin), "{admin}");
        }
    }

    #[test]
    fn streaming_commands_are_refused() {
        let classification = console("GET a\nSUBSCRIBE news");
        assert!(classification.refusal().is_some());
        assert_eq!(classification.statements[1].kind, "SUBSCRIBE");
        assert!(console("MONITOR").refusal().is_some());
    }

    #[test]
    fn engine_view_operations_are_classified_and_unknown_ones_refused() {
        assert_eq!(
            capability("redis.scan", json!({ "cursor": "0", "pattern": "user:*" })).worst(),
            Some(Category::Read)
        );
        let set = capability(
            "redis.edit",
            json!({ "op": "setString", "key": { "text": "k" }, "value": { "text": "v" }, "ttlMs": null, "keepTtl": false, "condition": "always",
                    "db": 2, "expect": { "kind": "string", "value": { "text": "old" } } }),
        );
        assert_eq!(set.worst(), Some(Category::Write));
        assert_eq!(set.tables(), vec!["k".to_owned()]);
        let ttl = capability("redis.edit", json!({ "op": "expire", "key": { "text": "k" }, "ttlMs": 5000 }));
        assert_eq!(ttl.worst(), Some(Category::Write));
        let delete = capability("redis.edit", json!({ "op": "delete", "keys": [{ "text": "a" }, { "text": "b" }] }));
        assert_eq!(delete.worst(), Some(Category::Destructive));
        assert_eq!(delete.tables().len(), 2);
        assert!(capability("redis.edit", json!({ "op": "teleport" })).refusal().is_some());
        assert_eq!(capability("redis.flush", json!({})).refusal(), Some("unknown operation"));
        for name in VECTOR_OPERATIONS {
            assert_eq!(capability(name, json!({ "table": "idx" })).worst(), Some(Category::Read), "{name}");
        }
    }

    #[test]
    fn urls_split_into_a_spec_and_a_password() {
        let parsed = REDIS.parse_connection_string("rediss://app:s3cret@cache.local:6380/3").unwrap();
        assert_eq!(parsed.password.as_deref(), Some("s3cret"));
        assert_eq!(parsed.spec.engine, "redis");
        assert_eq!((parsed.spec.host.as_str(), parsed.spec.port), ("cache.local", 6380));
        assert_eq!(parsed.spec.user, "app");
        assert_eq!(parsed.spec.option("db"), Some("3"));
        assert_eq!(parsed.spec.auth, Some(AuthMethodId::AclUser));
        assert_eq!(parsed.spec.tls.as_ref().map(|tls| tls.mode), Some(TlsMode::VerifyFull));
        assert!(parsed.spec.options.values().all(|value| !value.contains("s3cret")));
        assert!(parsed.spec.url.is_none());
        let plain = REDIS.parse_connection_string("redis://localhost").unwrap();
        assert_eq!(plain.spec.port, 6379);
        assert!(plain.spec.options.is_empty() && plain.spec.tls.is_none() && plain.password.is_none());
        assert!(REDIS.parse_connection_string("http://localhost").is_err());
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_connection_error_naming_the_engine() {
        // A port nobody listens on: bind to get a free one, then close it so the connection is refused at once.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let request = ConnectRequest {
            spec: ConnectionSpec::default(),
            secrets: ConnectSecrets::default(),
            endpoint: Endpoint {
                host: "127.0.0.1".into(),
                port: free,
            },
            connect_timeout: Duration::from_secs(2),
        };
        let Err(error) = REDIS.connect(request).await else {
            panic!("nothing listens on the port");
        };
        assert_eq!(error.code(), Some("CONNECTION_ERROR"), "{error:?}");
        assert_eq!(error.payload().unwrap()["retryable"], true);
        assert_eq!(error.field("engine").and_then(|engine| engine.as_str()), Some("redis"));
        assert_ne!(error.code(), Some(crate::contract::errors::ENGINE_NOT_WIRED));
    }
}
