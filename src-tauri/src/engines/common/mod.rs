//! Shapes shared by the engine modules: errors, catalog nodes and descriptions, browse requests and pages, row changes, result
//! streaming, transaction state. They are deliberately free of any engine, Tauri or `drivers` type so the core's ports can adopt
//! them as they are.

pub mod classify;
pub mod split;
pub mod sqlbuild;
pub mod value;

use serde::Serialize;
use std::fmt;
use std::time::Duration;

pub use classify::{classify, Classification, StatementKind};
pub use value::{parse_input, Value, ValueKind};

// ---------------------------------------------------------------------------------------------------------------------------
// Errors

/// An error shown to the user: a sentence that says what went wrong and, where possible, what to do. `code` carries the engine's
/// own code (SQLSTATE, ORA-xxxxx, ClickHouse error code) when there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineError {
    pub message: String,
    pub code: Option<String>,
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// Bad options, a malformed connection string, a value that does not fit its column.
    Input,
    /// Could not reach or log in to the server, or the client library is missing.
    Connect,
    /// The server rejected a statement.
    Server,
    /// The statement was cancelled by the user.
    Cancelled,
    /// The statement or the connection attempt ran out of time.
    Timeout,
    /// Anything else (protocol errors, unexpected shapes).
    Internal,
}

impl EngineError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            kind,
        }
    }
    pub fn input(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Input, message)
    }
    pub fn connect(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Connect, message)
    }
    pub fn server(message: impl Into<String>, code: Option<String>) -> Self {
        Self {
            message: message.into(),
            code,
            kind: ErrorKind::Server,
        }
    }
    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "The statement was cancelled.")
    }
    pub fn timeout(what: &str, limit: Duration) -> Self {
        Self::new(
            ErrorKind::Timeout,
            format!("{what} did not finish within {} ms.", limit.as_millis()),
        )
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{} ({code})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for EngineError {}

pub type Result<T> = std::result::Result<T, EngineError>;

// ---------------------------------------------------------------------------------------------------------------------------
// TLS

/// How to secure the connection, for engines whose client supports TLS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum TlsMode {
    /// Plain connection.
    #[default]
    Disable,
    /// TLS with certificate and host name verification against the bundled web roots, plus `ca_file` when given.
    Verify { ca_file: Option<String> },
    /// TLS without certificate verification (self-signed test servers). Traffic is encrypted but the server is not authenticated.
    SkipVerify,
}

// ---------------------------------------------------------------------------------------------------------------------------
// Catalog

/// What a catalog node is. The UI picks views and actions per kind (ADR 0001: object tree described by the engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    Database,
    Schema,
    /// A grouping node such as "Tables" or "Functions"; its `name` is the label.
    Folder,
    Table,
    View,
    MaterializedView,
    ForeignTable,
    Dictionary,
    Function,
    Procedure,
    Package,
    Sequence,
    Type,
    Extension,
    Index,
    Trigger,
    Synonym,
}

impl NodeKind {
    /// Whether rows can be browsed with [`BrowseRequest`].
    pub fn is_relation(self) -> bool {
        matches!(
            self,
            NodeKind::Table | NodeKind::View | NodeKind::MaterializedView | NodeKind::ForeignTable | NodeKind::Dictionary
        )
    }
}

/// A structured reference to a catalog object; never a dotted string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    pub database: Option<String>,
    pub schema: Option<String>,
    pub name: String,
    pub kind: NodeKind,
}

impl ObjectRef {
    pub fn new(database: Option<&str>, schema: Option<&str>, name: &str, kind: NodeKind) -> Self {
        Self {
            database: database.map(str::to_owned),
            schema: schema.map(str::to_owned),
            name: name.to_owned(),
            kind,
        }
    }
    pub fn table(schema: Option<&str>, name: &str) -> Self {
        Self::new(None, schema, name, NodeKind::Table)
    }
}

/// One node of the catalog tree. `children` is filled as deep as the call promised; `detail` is a short engine-specific note
/// (a table engine, a function signature, an extension version).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogNode {
    pub kind: NodeKind,
    pub name: String,
    pub object: Option<ObjectRef>,
    pub detail: Option<String>,
    pub children: Vec<CatalogNode>,
}

impl CatalogNode {
    pub fn object(object: ObjectRef, detail: Option<String>) -> Self {
        Self {
            kind: object.kind,
            name: object.name.clone(),
            object: Some(object),
            detail,
            children: Vec::new(),
        }
    }
    pub fn container(kind: NodeKind, name: &str, children: Vec<CatalogNode>) -> Self {
        Self {
            kind,
            name: name.to_owned(),
            object: None,
            detail: None,
            children,
        }
    }
    /// Depth-first search by kind and name, for tests and lookups.
    pub fn find(&self, kind: NodeKind, name: &str) -> Option<&CatalogNode> {
        if self.kind == kind && self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(kind, name))
    }
    /// Number of nodes of `kind` in this subtree.
    pub fn count(&self, kind: NodeKind) -> usize {
        usize::from(self.kind == kind) + self.children.iter().map(|c| c.count(kind)).sum::<usize>()
    }
}

/// A column of a table or of a result set.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    /// The engine's own spelling of the type, usable in casts (`numeric(12,2)`, `Nullable(DateTime64(3))`, `VARCHAR2(50)`).
    pub type_name: String,
    pub kind: ValueKind,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
    /// `ALWAYS` / `BY DEFAULT` identity, or `AUTOINCREMENT` / `serial`.
    pub identity: Option<String>,
    /// The expression of a generated (computed) column.
    pub generated: Option<String>,
}

impl ColumnInfo {
    /// A result-set column: only the name and type are known.
    pub fn result(name: &str, type_name: &str, kind: ValueKind) -> Self {
        Self {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            kind,
            nullable: true,
            default: None,
            comment: None,
            identity: None,
            generated: None,
        }
    }
    /// Whether the column can be written (generated columns cannot).
    pub fn is_writable(&self) -> bool {
        self.generated.is_none() && self.identity.as_deref() != Some("ALWAYS")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    pub name: Option<String>,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyInfo {
    pub name: Option<String>,
    pub columns: Vec<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: Option<String>,
    pub on_delete: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    /// Column names, or expressions for expression indexes.
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    /// Access method or index type (`btree`, `gin`, `minmax`, `NORMAL`, ...).
    pub method: Option<String>,
    pub definition: Option<String>,
}

/// Everything the design view needs about one object.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDescription {
    pub object: ObjectRef,
    pub comment: Option<String>,
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Option<KeyInfo>,
    pub unique_keys: Vec<KeyInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    pub indexes: Vec<IndexInfo>,
    /// Approximate row count from the engine's statistics; never a `COUNT(*)`.
    pub row_estimate: Option<i64>,
    pub ddl: Option<String>,
    /// Engine-specific facts (ClickHouse table engine, SQLite `WITHOUT ROWID`, Oracle tablespace).
    pub extra: Vec<(String, String)>,
}

impl ObjectDescription {
    pub fn new(object: ObjectRef) -> Self {
        Self {
            object,
            comment: None,
            columns: Vec::new(),
            primary_key: None,
            unique_keys: Vec::new(),
            foreign_keys: Vec::new(),
            indexes: Vec::new(),
            row_estimate: None,
            ddl: None,
            extra: Vec::new(),
        }
    }
    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// Browse

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Case-insensitive substring match on the column's text form.
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    /// A raw `LIKE` pattern (`%`, `_`), case-sensitive where the engine is.
    Like,
    NotLike,
    In,
    NotIn,
    Between,
    IsNull,
    IsNotNull,
}

impl FilterOp {
    /// How many values the operator takes (`None` = one or more).
    pub fn arity(self) -> Option<usize> {
        match self {
            FilterOp::IsNull | FilterOp::IsNotNull => Some(0),
            FilterOp::Between => Some(2),
            FilterOp::In | FilterOp::NotIn => None,
            _ => Some(1),
        }
    }
}

/// One condition: `column op values`. Values are what the user typed; the engine parses them to the column's type and binds them.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterCond {
    pub column: String,
    pub op: FilterOp,
    pub values: Vec<String>,
}

impl FilterCond {
    pub fn new(column: &str, op: FilterOp, values: &[&str]) -> Self {
        Self {
            column: column.to_owned(),
            op,
            values: values.iter().map(|v| (*v).to_owned()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SortKey {
    pub column: String,
    pub descending: bool,
}

impl SortKey {
    pub fn asc(column: &str) -> Self {
        Self {
            column: column.to_owned(),
            descending: false,
        }
    }
    pub fn desc(column: &str) -> Self {
        Self {
            column: column.to_owned(),
            descending: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseRequest {
    pub object: ObjectRef,
    /// Conditions joined with AND (or OR when `match_any`).
    pub filter: Vec<FilterCond>,
    pub match_any: bool,
    pub sort: Vec<SortKey>,
    pub offset: u64,
    pub limit: u64,
    /// Keyset cursor: the `next_keyset` of the previous page. When set, `offset` is ignored.
    pub after: Option<Vec<Value>>,
    /// Ask for an estimated total (statistics for unfiltered tables, the planner's estimate otherwise).
    pub estimate_total: bool,
}

impl BrowseRequest {
    pub fn new(object: ObjectRef, limit: u64) -> Self {
        Self {
            object,
            filter: Vec::new(),
            match_any: false,
            sort: Vec::new(),
            offset: 0,
            limit,
            after: None,
            estimate_total: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<Value>>,
    pub has_more: bool,
    /// Columns that identify a row for edits (primary key, or the engine's row id).
    pub key_columns: Vec<String>,
    /// Pass back as `after` to get the next page by keyset; `None` when keyset paging is not possible for this order.
    pub next_keyset: Option<Vec<Value>>,
    pub estimated_total: Option<i64>,
    /// The SQL that was run (with placeholders), for the UI's "show query".
    pub sql: String,
}

// ---------------------------------------------------------------------------------------------------------------------------
// Row changes

/// One row edit. Keys identify exactly one row; values are typed (use [`parse_input`] for text the user typed).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum RowChange {
    Insert {
        values: Vec<(String, Value)>,
    },
    Update {
        key: Vec<(String, Value)>,
        values: Vec<(String, Value)>,
    },
    Delete {
        key: Vec<(String, Value)>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSummary {
    pub inserted: u64,
    pub updated: u64,
    pub deleted: u64,
}

// ---------------------------------------------------------------------------------------------------------------------------
// Running statements

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    /// Stop reading each result set after this many rows (the rest is cancelled or discarded on the server).
    pub row_limit: usize,
    /// Rows per streamed batch.
    pub batch_size: usize,
    /// Server-side (or client-enforced) limit per statement.
    pub timeout: Option<Duration>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            row_limit: 1000,
            batch_size: 200,
            timeout: None,
        }
    }
}

/// What a session streams while it runs a script.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum StreamEvent {
    /// A statement started; `index` counts statements in the script.
    Statement {
        index: usize,
        sql: String,
    },
    /// A result set starts.
    Columns {
        index: usize,
        columns: Vec<ColumnInfo>,
    },
    Rows {
        index: usize,
        rows: Vec<Vec<Value>>,
    },
    /// A statement finished. `affected` is set for statements that change rows; `truncated` when rows were left unread.
    Done {
        index: usize,
        affected: Option<u64>,
        rows: usize,
        truncated: bool,
        elapsed_ms: u64,
    },
}

/// Whether the consumer wants more rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// Receives a run's events in order. Returning [`Flow::Stop`] from a `Rows` event stops reading that result set early.
pub trait ResultSink: Send {
    fn event(&mut self, event: StreamEvent) -> Flow;
}

impl<F: FnMut(StreamEvent) -> Flow + Send> ResultSink for F {
    fn event(&mut self, event: StreamEvent) -> Flow {
        self(event)
    }
}

/// A finished result set, for callers that want everything in memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSet {
    pub sql: String,
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<Value>>,
    pub affected: Option<u64>,
    pub truncated: bool,
}

/// A sink that keeps every result set in memory.
#[derive(Debug, Default)]
pub struct CollectSink {
    pub sets: Vec<ResultSet>,
    pub batches: usize,
}

impl ResultSink for CollectSink {
    fn event(&mut self, event: StreamEvent) -> Flow {
        match event {
            StreamEvent::Statement { sql, .. } => self.sets.push(ResultSet { sql, ..Default::default() }),
            StreamEvent::Columns { columns, .. } => {
                if let Some(set) = self.sets.last_mut() {
                    set.columns = columns;
                }
            }
            StreamEvent::Rows { rows, .. } => {
                self.batches += 1;
                if let Some(set) = self.sets.last_mut() {
                    set.rows.extend(rows);
                }
            }
            StreamEvent::Done { affected, truncated, .. } => {
                if let Some(set) = self.sets.last_mut() {
                    set.affected = affected;
                    set.truncated = truncated;
                }
            }
        }
        Flow::Continue
    }
}

/// Transaction state of a session, reported after each run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TxState {
    /// Autocommit: every statement commits on its own.
    Idle,
    /// Inside an explicit transaction; changes are pending until COMMIT.
    Active,
    /// Inside a transaction that failed; only ROLLBACK (or ROLLBACK TO SAVEPOINT) works.
    Failed,
    /// The engine has no transactions (ClickHouse).
    Unsupported,
}

/// Fills `StreamEvent::Rows` batches from a row source and enforces the row limit. Returns `(rows_kept, truncated)` once the
/// source is exhausted or the limit or the consumer stops it; `next` is called until it returns `None` or the batch loop ends.
pub(crate) struct Batcher {
    pub index: usize,
    pub limit: usize,
    pub batch_size: usize,
    pub kept: usize,
    pub stopped: bool,
    buffer: Vec<Vec<Value>>,
}

impl Batcher {
    pub fn new(index: usize, opts: &RunOptions) -> Self {
        Self {
            index,
            limit: opts.row_limit,
            batch_size: opts.batch_size.max(1),
            kept: 0,
            stopped: false,
            buffer: Vec::new(),
        }
    }

    /// Adds a row. Returns false once no more rows are wanted (limit reached or the sink said stop).
    pub fn push(&mut self, row: Vec<Value>, sink: &mut dyn ResultSink) -> bool {
        if self.stopped || self.kept >= self.limit {
            self.stopped = true;
            return false;
        }
        self.buffer.push(row);
        self.kept += 1;
        if self.buffer.len() >= self.batch_size || self.kept >= self.limit {
            self.flush(sink);
        }
        !self.stopped && self.kept < self.limit
    }

    pub fn flush(&mut self, sink: &mut dyn ResultSink) {
        if self.buffer.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.buffer);
        if sink.event(StreamEvent::Rows { index: self.index, rows }) == Flow::Stop {
            self.stopped = true;
        }
    }

    /// True when the reader stopped before the source ended.
    pub fn wants_more(&self) -> bool {
        !self.stopped && self.kept < self.limit
    }
}

/// Runs a blocking statement loop (SQLite, Oracle) on Tokio's blocking pool and forwards its events to `sink` with
/// backpressure. When `sink` answers [`Flow::Stop`], the worker's next event returns `Stop` so it stops reading.
pub(crate) async fn run_blocking<T, F>(sink: &mut dyn ResultSink, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut dyn ResultSink) -> Result<T> + Send + 'static,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    struct ChannelSink {
        tx: tokio::sync::mpsc::Sender<StreamEvent>,
        stop: Arc<AtomicBool>,
    }
    impl ResultSink for ChannelSink {
        fn event(&mut self, event: StreamEvent) -> Flow {
            if self.tx.blocking_send(event).is_err() || self.stop.load(Ordering::Relaxed) {
                Flow::Stop
            } else {
                Flow::Continue
            }
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let stop = Arc::new(AtomicBool::new(false));
    let mut worker_sink = ChannelSink { tx, stop: stop.clone() };
    let handle = tokio::task::spawn_blocking(move || work(&mut worker_sink));
    while let Some(ev) = rx.recv().await {
        if sink.event(ev) == Flow::Stop {
            stop.store(true, Ordering::Relaxed);
        }
    }
    handle
        .await
        .map_err(|e| EngineError::internal(format!("The worker thread failed: {e}")))?
}

/// Runs blocking work (catalog queries, row changes) on Tokio's blocking pool.
pub(crate) async fn blocking<T, F>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| EngineError::internal(format!("The worker thread failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batcher_stops_at_the_limit_and_flushes_in_batches() {
        let opts = RunOptions {
            row_limit: 5,
            batch_size: 2,
            timeout: None,
        };
        let mut b = Batcher::new(0, &opts);
        let mut sink = CollectSink::default();
        sink.event(StreamEvent::Statement { index: 0, sql: "x".into() });
        let mut pushed = 0;
        for i in 0..10 {
            pushed += 1;
            if !b.push(vec![Value::Int(i)], &mut sink) {
                break;
            }
        }
        b.flush(&mut sink);
        assert_eq!(pushed, 5);
        assert_eq!(sink.sets[0].rows.len(), 5);
        assert_eq!(sink.batches, 3);
        assert!(!b.wants_more());
    }

    #[test]
    fn catalog_find_and_count() {
        let t = CatalogNode::object(ObjectRef::table(Some("public"), "orders"), None);
        let root = CatalogNode::container(
            NodeKind::Schema,
            "public",
            vec![CatalogNode::container(NodeKind::Folder, "Tables", vec![t])],
        );
        assert!(root.find(NodeKind::Table, "orders").is_some());
        assert_eq!(root.count(NodeKind::Table), 1);
    }
}
