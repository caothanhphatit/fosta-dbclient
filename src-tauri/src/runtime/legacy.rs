//! Transitional: the engine contract over the pre-contract `drivers::Driver`s (MongoDB), so the core, the new commands
//! and the contract tests use one path while the native adapters are written. Values are converted at the boundary
//! (text cells become `Value::Text`, `Value::Bytes` for binary columns, numbers keep their text). Removed once every
//! engine has a native adapter.
//!
//! The old commands still call the `Driver` directly; [`connect`] returns it next to the contract connection and the
//! `ConnectionManager` keeps both. MariaDB/MySQL and SQL Server have native adapters; [`open_driver`] opens their legacy
//! driver next to it for the old commands.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock,
};

use async_trait::async_trait;
use futures_util::future::BoxFuture;

use crate::{
    config::ConnectionProfile,
    contract::{
        classify_common, unsupported_capability, ApplyResult, BrowseRequest, CallContext, Canceller, Capability, CapabilitySet,
        Classification, ClassifyInput, ColumnHints, ColumnInfo, ConnectRequest, Connection, DatabaseAdmin, Engine, EngineDescriptor, Field,
        Filter, ForeignKeyInfo, KeyInfo, LanguageSpec, NodeKey, NodeKind, ObjectDescription, ObjectNode, ObjectRef, OpenedSession, Page,
        ReferencingKey, ResultSink, RowChange, RowChangeSet, RunOptions, RunRequest, SchemaGraph, SchemaGraphSource, ServerInfo, Session,
        SessionOptions, StatementInfo, StreamEvent, TabularData, Total, TransactionOp, TransactionState, TreeChildren, TreeRequest, Value,
        ValueKind,
    },
    drivers::{
        self,
        types::{self as old, Cell, CellValue},
        Driver, Limits,
    },
    engines::{mongo, mssql, mysql},
    error::AppResult,
    gateway::classify::{classify_mongo, classify_sql},
    schema,
};

/// An engine served by a legacy driver.
pub struct LegacyEngine {
    kind: drivers::Engine,
    descriptor: fn() -> EngineDescriptor,
    cell: OnceLock<EngineDescriptor>,
}

impl LegacyEngine {
    const fn new(kind: drivers::Engine, descriptor: fn() -> EngineDescriptor) -> Self {
        Self {
            kind,
            descriptor,
            cell: OnceLock::new(),
        }
    }
}

pub static MONGODB: LegacyEngine = LegacyEngine::new(drivers::Engine::MongoDb, mongo::descriptor::descriptor);

/// What the legacy drivers can do through the contract (no structured filters, no keyset, no concurrency checks).
fn legacy_capabilities(kind: drivers::Engine) -> CapabilitySet {
    let mut set = CapabilitySet::of(&[
        Capability::Tree,
        Capability::Describe,
        Capability::MultipleDatabases,
        Capability::BrowseRows,
        Capability::EditRows,
        Capability::RawFilter,
        Capability::ExactCount,
        Capability::Query,
        Capability::SchemaGraph,
        Capability::CreateDatabase,
    ]);
    if kind != drivers::Engine::MongoDb {
        set = set.with(Capability::Transactions).with(Capability::StatementTimeout);
    }
    if matches!(kind, drivers::Engine::MariaDb | drivers::Engine::MySql) {
        set = set.with(Capability::Cancel).with(Capability::ReadOnlySession);
    }
    if kind == drivers::Engine::MongoDb {
        set = set.with(Capability::Documents);
    }
    set
}

#[async_trait]
impl Engine for LegacyEngine {
    fn descriptor(&self) -> &EngineDescriptor {
        self.cell.get_or_init(self.descriptor)
    }

    fn classify(&self, input: &ClassifyInput<'_>) -> Classification {
        if let Some(classification) = classify_common(input) {
            return classification;
        }
        match input {
            ClassifyInput::Statement { text, .. } if self.kind == drivers::Engine::MongoDb => classify_mongo(text),
            ClassifyInput::Statement { text, .. } => classify_sql(
                self.descriptor().sql_dialect.unwrap_or(crate::contract::SqlDialectId::Generic),
                text,
            ),
            ClassifyInput::Capability { name, .. } => Classification::single(StatementInfo::refused(name, name, "unknown operation")),
            _ => Classification::default(),
        }
    }

    async fn connect(&self, request: ConnectRequest) -> AppResult<Arc<dyn Connection>> {
        let (connection, _) = connect(&request).await?;
        Ok(connection)
    }
}

/// Whether this engine id is served by a legacy driver.
pub fn handles(engine_id: &str) -> bool {
    matches!(engine_id, "mariadb" | "mysql" | "mssql" | "mongodb")
}

/// Whether the contract connection itself is still [`LegacyConnection`] (no native adapter yet). Engines with a native
/// adapter (MariaDB/MySQL) still get a legacy driver next to it (a second pool) for the old commands, until those are
/// removed.
pub fn wrapped(engine_id: &str) -> bool {
    matches!(engine_id, "mongodb")
}

/// Opens only the legacy driver (next to a native connection).
pub async fn open_driver(request: &ConnectRequest) -> AppResult<Arc<dyn Driver>> {
    let (driver, _) = drivers::open(&profile_of(request), request.secrets.password_or_empty()).await?;
    Ok(driver)
}

/// The profile a legacy driver opens, rebuilt from the spec (the endpoint replaces host and port).
fn profile_of(request: &ConnectRequest) -> ConnectionProfile {
    let spec = &request.spec;
    let mut options = spec.options.clone();
    if let Some(url) = spec.url.as_ref().filter(|url| !url.is_empty()) {
        options.entry("uri".to_owned()).or_insert_with(|| url.clone());
    }
    if let Some(tls) = &spec.tls {
        options
            .entry("encrypt".to_owned())
            .or_insert_with(|| (tls.mode != crate::contract::TlsMode::Disable).to_string());
        options
            .entry("trustServerCertificate".to_owned())
            .or_insert_with(|| tls.accept_invalid_certs.to_string());
    }
    ConnectionProfile {
        id: String::new(),
        name: String::new(),
        engine: spec.engine.clone(),
        host: request.endpoint.host.clone(),
        port: request.endpoint.port,
        user: spec.user.clone(),
        database: spec.database.clone(),
        save_password: false,
        options,
        ..Default::default()
    }
}

/// Opens the legacy driver and wraps it; returns both the contract connection and the driver.
pub async fn connect(request: &ConnectRequest) -> AppResult<(Arc<dyn Connection>, Arc<dyn Driver>)> {
    let profile = profile_of(request);
    let (driver, version) = drivers::open(&profile, request.secrets.password_or_empty()).await?;
    Ok((wrap(Arc::clone(&driver), &version), driver))
}

/// The registered engine per legacy driver kind.
pub fn engine_for(kind: drivers::Engine) -> &'static dyn Engine {
    match kind {
        drivers::Engine::MariaDb => &mysql::MARIADB,
        drivers::Engine::MySql => &mysql::MYSQL,
        drivers::Engine::SqlServer => &mssql::MSSQL,
        drivers::Engine::MongoDb => &MONGODB,
    }
}

/// A contract connection over an open legacy driver.
pub fn wrap(driver: Arc<dyn Driver>, version: &str) -> Arc<dyn Connection> {
    let kind = driver.engine();
    let label = kind.label();
    let trimmed = version.replace("-MariaDB", "").replace("-mariadb", "");
    let short = trimmed.split(['-', ' ']).next().unwrap_or(&trimmed).to_owned();
    Arc::new(LegacyConnection {
        driver,
        kind,
        info: ServerInfo {
            product: label.to_owned(),
            version: version.to_owned(),
            label: format!("{label} {short}"),
            flavor: None,
        },
        capabilities: legacy_capabilities(kind),
        sessions: AtomicU64::new(0),
    })
}

pub struct LegacyConnection {
    driver: Arc<dyn Driver>,
    kind: drivers::Engine,
    info: ServerInfo,
    capabilities: CapabilitySet,
    sessions: AtomicU64,
}

fn database_key(database: &str) -> NodeKey {
    NodeKey {
        id: format!("db:{database}"),
        kind: NodeKind::DATABASE,
        object: Some(ObjectRef::new(Some(database), None, database, NodeKind::DATABASE)),
    }
}

fn object_kind(kind: drivers::Engine, table_kind: &str) -> NodeKind {
    match (kind, table_kind) {
        (drivers::Engine::MongoDb, _) => NodeKind::COLLECTION,
        (_, "view") => NodeKind::VIEW,
        _ => NodeKind::TABLE,
    }
}

/// A rough neutral kind from a SQL type name, for alignment and editors.
pub fn kind_of_type(type_name: &str) -> ValueKind {
    let t = type_name.to_ascii_lowercase();
    let base = t.split(['(', ' ']).next().unwrap_or(&t);
    match base {
        "tinyint" if t.starts_with("tinyint(1)") => ValueKind::Bool,
        "bit" if t == "bit" || t == "bit(1)" => ValueKind::Bool,
        "bool" | "boolean" => ValueKind::Bool,
        "int" | "integer" | "smallint" | "tinyint" | "mediumint" | "bigint" | "year" => ValueKind::Int,
        "decimal" | "numeric" | "money" | "smallmoney" => ValueKind::Decimal,
        "float" | "double" | "real" => ValueKind::Float,
        "date" => ValueKind::Date,
        "time" => ValueKind::Time,
        "datetime" | "datetime2" | "smalldatetime" | "timestamp" => ValueKind::Timestamp,
        "datetimeoffset" => ValueKind::TimestampTz,
        "json" => ValueKind::Json,
        "uniqueidentifier" | "uuid" => ValueKind::Uuid,
        "binary" | "varbinary" | "blob" | "tinyblob" | "mediumblob" | "longblob" | "image" | "rowversion" => ValueKind::Bytes,
        _ => ValueKind::Text,
    }
}

fn column_of(meta: &old::ColumnMeta) -> ColumnInfo {
    let kind = if meta.binary {
        ValueKind::Bytes
    } else if meta.numeric {
        match kind_of_type(&meta.type_name) {
            kind @ (ValueKind::Int | ValueKind::Float) => kind,
            _ => ValueKind::Decimal,
        }
    } else {
        kind_of_type(&meta.type_name)
    };
    let mut column = ColumnInfo::result(&meta.name, &meta.type_name, kind);
    column.hints = ColumnHints {
        primary: meta.primary,
        binary: meta.binary,
        vector_dims: meta.vector_dims,
        read_only: false,
    };
    column
}

/// A text cell as a typed value of its column.
fn value_of(cell: &Cell, column: &ColumnInfo) -> Value {
    let Some(text) = cell else { return Value::Null };
    match column.kind {
        ValueKind::Bytes => crate::contract::value::unhex(text, false)
            .map(Value::Bytes)
            .unwrap_or_else(|| Value::Text(text.clone())),
        ValueKind::Int => text.parse().map(Value::Int).unwrap_or_else(|_| Value::Decimal(text.clone())),
        ValueKind::Decimal | ValueKind::Float => Value::Decimal(text.clone()),
        _ => Value::Text(text.clone()),
    }
}

fn rows_of(rows: Vec<Vec<Cell>>, columns: &[ColumnInfo]) -> Vec<Vec<Value>> {
    rows.into_iter()
        .map(|row| row.iter().zip(columns).map(|(cell, column)| value_of(cell, column)).collect())
        .collect()
}

/// A typed value as the text cell a legacy driver expects (`0x…` for bytes).
pub fn cell_of(value: &Value) -> Cell {
    match value {
        Value::Null => None,
        Value::Bytes(bytes) => Some(crate::contract::value::hex(bytes)),
        Value::Bool(flag) => Some(if *flag { "1" } else { "0" }.to_owned()),
        other => other.display(),
    }
}

fn cells(fields: &[Field]) -> Vec<CellValue> {
    fields
        .iter()
        .map(|field| CellValue {
            column: field.column.clone(),
            value: cell_of(&field.value),
        })
        .collect()
}

impl LegacyConnection {
    fn database_of(object: &ObjectRef) -> AppResult<String> {
        object
            .database
            .clone()
            .ok_or_else(|| crate::error::AppError::new("The object has no database"))
    }

    fn limits(options: &RunOptions) -> Limits {
        Limits {
            max_rows: options.row_limit,
            timeout_secs: options.timeout.map(|t| t.as_secs() as u32).unwrap_or(0),
            read_only: options.read_only,
        }
    }
}

#[async_trait]
impl Connection for LegacyConnection {
    fn engine_id(&self) -> &str {
        self.kind.id()
    }

    fn info(&self) -> &ServerInfo {
        &self.info
    }

    fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    fn languages(&self) -> Vec<LanguageSpec> {
        Vec::new()
    }

    async fn children(&self, request: &TreeRequest, _ctx: &CallContext) -> AppResult<TreeChildren> {
        let nodes = match &request.parent {
            None => self
                .driver
                .list_databases()
                .await?
                .into_iter()
                .map(|name| ObjectNode {
                    label: name.clone(),
                    kind: NodeKind::DATABASE,
                    icon: "database".into(),
                    object: database_key(&name).object,
                    key: database_key(&name),
                    detail: None,
                    badges: Vec::new(),
                    expandable: true,
                    default_view: None,
                    actions: Vec::new(),
                })
                .collect(),
            Some(parent) if parent.kind == NodeKind::DATABASE => {
                let database = parent.id.strip_prefix("db:").unwrap_or(&parent.id).to_owned();
                self.driver
                    .list_tables(&database)
                    .await?
                    .into_iter()
                    .map(|table| {
                        let kind = object_kind(self.kind, &table.kind);
                        let object = ObjectRef::new(Some(&database), None, &table.name, kind.clone());
                        ObjectNode {
                            key: NodeKey {
                                id: format!("db:{database}/{}:{}", kind.as_str(), table.name),
                                kind: kind.clone(),
                                object: Some(object.clone()),
                            },
                            label: table.name.clone(),
                            icon: if kind == NodeKind::VIEW { "view" } else { "table" }.into(),
                            object: Some(object),
                            detail: table.rows.map(|rows| format!("~{rows} rows")),
                            badges: Vec::new(),
                            expandable: false,
                            default_view: Some("data".into()),
                            actions: Vec::new(),
                            kind,
                        }
                    })
                    .collect()
            }
            Some(_) => Vec::new(),
        };
        Ok(TreeChildren {
            nodes,
            next_cursor: None,
            total_hint: None,
        })
    }

    async fn describe(&self, object: &ObjectRef, _ctx: &CallContext) -> AppResult<ObjectDescription> {
        let database = Self::database_of(object)?;
        let design = self.driver.describe_table(&database, &object.name).await?;
        let relations = self.driver.table_relations(&database, &object.name).await.unwrap_or_default();
        let mut description = ObjectDescription::new(object.clone());
        let primary: Vec<String> = design
            .columns
            .iter()
            .filter(|column| column.key.eq_ignore_ascii_case("PRI"))
            .map(|column| column.name.clone())
            .collect();
        description.columns = design
            .columns
            .iter()
            .map(|column| {
                let mut info = ColumnInfo::result(&column.name, &column.column_type, kind_of_type(&column.column_type));
                info.nullable = column.nullable;
                info.default = column.default_value.clone();
                info.comment = (!column.comment.is_empty()).then(|| column.comment.clone());
                info.identity = column.extra.contains("auto_increment").then(|| "auto_increment".to_owned());
                info.hints.primary = primary.contains(&column.name);
                info.hints.binary = matches!(info.kind, ValueKind::Bytes);
                info
            })
            .collect();
        description.primary_key = (!primary.is_empty()).then_some(KeyInfo {
            name: None,
            columns: primary,
        });
        description.foreign_keys = relations
            .outgoing
            .iter()
            .map(|relation| ForeignKeyInfo {
                name: Some(relation.constraint.clone()),
                columns: relation.columns.clone(),
                ref_database: (relation.referenced_database != database).then(|| relation.referenced_database.clone()),
                ref_schema: None,
                ref_table: relation.referenced_table.clone(),
                ref_columns: relation.referenced_columns.clone(),
                on_update: None,
                on_delete: None,
            })
            .collect();
        description.referenced_by = relations
            .incoming
            .iter()
            .map(|relation| ReferencingKey {
                name: Some(relation.constraint.clone()),
                database: Some(relation.database.clone()),
                schema: None,
                table: relation.table.clone(),
                columns: relation.columns.clone(),
                ref_columns: relation.referenced_columns.clone(),
            })
            .collect();
        description.ddl = (!design.ddl.is_empty()).then_some(design.ddl);
        Ok(description)
    }

    async fn open_session(&self, _options: &SessionOptions, _ctx: &CallContext) -> AppResult<OpenedSession> {
        let id = format!("legacy-{}", self.sessions.fetch_add(1, Ordering::Relaxed));
        self.driver.open_session(&id).await?;
        let session = Arc::new(LegacySession {
            driver: Arc::clone(&self.driver),
            id: id.clone(),
            language: if self.kind == drivers::Engine::MongoDb {
                "mongo-command"
            } else {
                "sql"
            },
        });
        let canceller: Option<Arc<dyn Canceller>> = self.capabilities.contains(Capability::Cancel).then(|| {
            Arc::new(LegacyCanceller {
                driver: Arc::clone(&self.driver),
                id,
            }) as Arc<dyn Canceller>
        });
        Ok(OpenedSession { session, canceller })
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

    async fn close(&self) {
        self.driver.close().await;
    }
}

#[async_trait]
impl TabularData for LegacyConnection {
    async fn browse(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        let filter = match &request.filter {
            Filter::None => None,
            Filter::Raw { text, .. } => Some(text.clone()),
            Filter::Conditions { conditions, .. } if conditions.is_empty() => None,
            Filter::Conditions { .. } => return Err(unsupported_capability("column filters on this connection")),
        };
        let sort = request.sort.first();
        let old_request = old::BrowseRequest {
            connection_id: String::new(),
            database: Self::database_of(&request.object)?,
            table: request.object.name.clone(),
            offset: request.offset,
            limit: request.limit,
            order_by: sort.map(|key| key.column.clone()),
            descending: sort.is_some_and(|key| key.descending),
            filter,
        };
        let page = self.driver.browse_table(&old_request, &ctx.limits).await?;
        let columns: Vec<ColumnInfo> = page.columns.iter().map(column_of).collect();
        let count = page.rows.len() as u64;
        Ok(Page {
            rows: rows_of(page.rows, &columns),
            columns,
            has_more: request.offset + count < page.total,
            key_columns: page.primary_key,
            next_keyset: None,
            total: Some(Total {
                value: page.total,
                exact: true,
            }),
            statement: None,
            elapsed_ms: page.elapsed_ms,
            row_versions: None,
        })
    }

    async fn apply(&self, set: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        let database = Self::database_of(&set.object)?;
        let mut result = ApplyResult::default();
        let changes = set
            .changes
            .iter()
            .map(|change| match change {
                RowChange::Insert { values } => {
                    result.inserted += 1;
                    old::RowChange::Insert { values: cells(values) }
                }
                RowChange::Update { key, values, .. } => {
                    result.updated += 1;
                    old::RowChange::Update {
                        key: cells(key),
                        values: cells(values),
                    }
                }
                RowChange::Delete { key, .. } => {
                    result.deleted += 1;
                    old::RowChange::Delete { key: cells(key) }
                }
            })
            .collect();
        self.driver.apply_changes(&database, &set.object.name, changes).await?;
        Ok(result)
    }
}

#[async_trait]
impl SchemaGraphSource for LegacyConnection {
    async fn schema_graph(&self, database: &str, _ctx: &CallContext) -> AppResult<SchemaGraph> {
        schema::schema_graph(self.driver.as_ref(), database).await
    }
}

#[async_trait]
impl DatabaseAdmin for LegacyConnection {
    async fn create_database(&self, name: &str, first_collection: Option<&str>, _ctx: &CallContext) -> AppResult<()> {
        self.driver.create_database(name, first_collection).await
    }
}

struct LegacySession {
    driver: Arc<dyn Driver>,
    id: String,
    language: &'static str,
}

#[async_trait]
impl Session for LegacySession {
    fn language(&self) -> &str {
        self.language
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        let outcome = self
            .driver
            .run_query(
                &self.id,
                request.database.clone(),
                request.text.clone(),
                &LegacyConnection::limits(options),
            )
            .await?;
        let elapsed = outcome.elapsed_ms;
        for (index, set) in outcome.sets.into_iter().enumerate() {
            sink.event(StreamEvent::Statement { index, sql: String::new() });
            let columns: Vec<ColumnInfo> = set.columns.iter().map(column_of).collect();
            let rows = set.rows.len();
            if !columns.is_empty() {
                sink.event(StreamEvent::Columns {
                    index,
                    columns: columns.clone(),
                });
                let mut values = rows_of(set.rows, &columns);
                while !values.is_empty() {
                    let rest = values.split_off(values.len().min(options.batch_size.max(1)));
                    let batch = std::mem::replace(&mut values, rest);
                    if sink.event(StreamEvent::Rows { index, rows: batch }) == crate::contract::Flow::Stop {
                        return Ok(());
                    }
                }
            }
            sink.event(StreamEvent::Done {
                index,
                affected: columns.is_empty().then_some(set.affected_rows),
                rows,
                truncated: set.truncated,
                elapsed_ms: elapsed,
            });
        }
        Ok(())
    }

    async fn transaction(&self, op: TransactionOp, options: &RunOptions) -> AppResult<TransactionState> {
        self.driver.transaction(&self.id, op, &LegacyConnection::limits(options)).await
    }

    async fn transaction_state(&self) -> TransactionState {
        self.driver
            .transaction(&self.id, TransactionOp::Status, &Limits::default())
            .await
            .unwrap_or_else(|_| TransactionState::unsupported())
    }

    async fn close(&self) {
        self.driver.close_session(&self.id);
    }
}

struct LegacyCanceller {
    driver: Arc<dyn Driver>,
    id: String,
}

impl Canceller for LegacyCanceller {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>> {
        let (driver, id) = (Arc::clone(&self.driver), self.id.clone());
        Box::pin(async move { driver.cancel_query(&id).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Category;

    #[test]
    fn type_names_map_to_neutral_kinds() {
        assert_eq!(kind_of_type("int(11)"), ValueKind::Int);
        assert_eq!(kind_of_type("tinyint(1)"), ValueKind::Bool);
        assert_eq!(kind_of_type("decimal(12,2)"), ValueKind::Decimal);
        assert_eq!(kind_of_type("varbinary(16)"), ValueKind::Bytes);
        assert_eq!(kind_of_type("datetime2"), ValueKind::Timestamp);
        assert_eq!(kind_of_type("varchar(20)"), ValueKind::Text);
    }

    #[test]
    fn cells_round_trip_through_values() {
        let bytes = ColumnInfo::result("b", "varbinary(2)", ValueKind::Bytes);
        assert_eq!(value_of(&Some("0xdead".into()), &bytes), Value::Bytes(vec![0xde, 0xad]));
        assert_eq!(cell_of(&Value::Bytes(vec![0xde, 0xad])).as_deref(), Some("0xdead"));
        let int = ColumnInfo::result("i", "int", ValueKind::Int);
        assert_eq!(value_of(&Some("7".into()), &int), Value::Int(7));
        assert_eq!(value_of(&None, &int), Value::Null);
    }

    #[test]
    fn legacy_engines_classify_with_the_gateway_classifiers() {
        let mongo = MONGODB.classify(&ClassifyInput::Statement {
            language: "mongo-command",
            text: r#"{ "find": "t" }"#,
        });
        assert_eq!(mongo.worst(), Some(Category::Read));
        let unknown = MONGODB.classify(&ClassifyInput::Capability {
            name: "x.y",
            args: &serde_json::Value::Null,
        });
        assert!(unknown.refusal().is_some());
        assert_eq!(MONGODB.descriptor().id, "mongodb");
        assert_eq!(engine_for(drivers::Engine::MariaDb).id(), "mariadb");
        assert_eq!(engine_for(drivers::Engine::SqlServer).id(), "mssql");
        assert!(!handles("postgres") && handles("mysql") && !wrapped("mysql"));
        assert!(handles("mssql") && !wrapped("mssql") && wrapped("mongodb"));
    }
}
