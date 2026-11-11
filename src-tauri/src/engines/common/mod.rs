//! Shapes shared by the SQL engine cores (PostgreSQL, SQLite, ClickHouse, Oracle): catalog nodes and descriptions, browse
//! requests and pages, row changes. The neutral types the contract owns (typed values, columns, filters, streaming,
//! errors, transaction state) live in `crate::contract` and are re-exported here, so the cores use one set of types;
//! [`adapt`] converts the core-only shapes to their contract counterparts at the adapter boundary.

pub mod adapt;
pub mod classify;
pub mod serverjs;
pub mod split;
pub mod sqlbuild;
pub mod sqlclassify;
pub mod value;

use serde::Serialize;

pub use crate::contract::{
    describe::{ColumnHints, ColumnInfo, ForeignKeyInfo, IndexInfo, KeyInfo},
    errors::{EngineError, ErrorKind},
    session::TxState,
    stream::{blocking, run_blocking, Batcher, CollectSink, Flow, ResultSet, ResultSink, RunOptions, StreamEvent},
    tabular::{FilterCond, FilterOp, SortKey},
};
pub use classify::{classify, Classification, StatementKind};
#[allow(unused_imports)] // part of the API; the app uses it once edits are wired in
pub use value::{parse_input, Value, ValueKind};

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
    /// Read from the end: the order reversed, the rows returned in the normal order (see the contract's `reverse`).
    pub reverse: bool,
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
            reverse: false,
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

#[cfg(test)]
mod tests {
    use super::*;

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
