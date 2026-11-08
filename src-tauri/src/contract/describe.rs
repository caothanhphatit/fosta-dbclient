//! What the design view, the grid header and the assistant know about one object: columns with typed kinds, keys,
//! foreign keys in both directions, indexes and DDL.

use serde::{Deserialize, Serialize};

use super::{object::ObjectRef, value::ValueKind};

/// UI hints the engine knows about a column.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnHints {
    /// Part of the primary key (or the engine's row identity).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub primary: bool,
    /// Binary data shown as hex.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub binary: bool,
    /// A vector column (MariaDB `VECTOR(n)`) with its dimensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_dims: Option<u32>,
    /// Cannot be written from the grid (generated, identity, row version).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
}

/// A column of a table or of a result set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    /// The engine's own spelling of the type, usable in casts (`numeric(12,2)`, `Nullable(DateTime64(3))`, `VARCHAR2(50)`).
    pub type_name: String,
    pub kind: ValueKind,
    pub nullable: bool,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    /// `ALWAYS` / `BY DEFAULT` identity, or `AUTOINCREMENT` / `serial` / `auto_increment`.
    #[serde(default)]
    pub identity: Option<String>,
    /// The expression of a generated (computed) column.
    #[serde(default)]
    pub generated: Option<String>,
    #[serde(default)]
    pub hints: ColumnHints,
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
            hints: ColumnHints::default(),
        }
    }

    /// Whether the column can be written (generated columns cannot).
    pub fn is_writable(&self) -> bool {
        self.generated.is_none() && self.identity.as_deref() != Some("ALWAYS") && !self.hints.read_only
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    pub name: Option<String>,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyInfo {
    pub name: Option<String>,
    pub columns: Vec<String>,
    /// The referenced database when the engine allows cross-database keys (MySQL); `None` = same database.
    #[serde(default)]
    pub ref_database: Option<String>,
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_update: Option<String>,
    pub on_delete: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// A foreign key pointing at the described object, seen from the referencing side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferencingKey {
    pub name: Option<String>,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub table: String,
    pub columns: Vec<String>,
    /// Columns of the described object.
    pub ref_columns: Vec<String>,
}

/// Everything the design view needs about one object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDescription {
    pub object: ObjectRef,
    pub comment: Option<String>,
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Option<KeyInfo>,
    pub unique_keys: Vec<KeyInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// Foreign keys of other tables that point at this one.
    #[serde(default)]
    pub referenced_by: Vec<ReferencingKey>,
    pub indexes: Vec<IndexInfo>,
    /// Approximate row count from the engine's statistics; never a `COUNT(*)`.
    pub row_estimate: Option<i64>,
    pub ddl: Option<String>,
    /// Engine-specific facts (ClickHouse table engine, SQLite `WITHOUT ROWID`, Oracle tablespace, MySQL column extras).
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
            referenced_by: Vec::new(),
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
