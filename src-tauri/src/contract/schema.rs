//! What the ER diagram needs for one database: tables with their columns and the foreign keys between them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaColumn {
    pub name: String,
    pub column_type: String,
    pub nullable: bool,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaTable {
    pub name: String,
    pub kind: String,
    pub rows: Option<u64>,
    pub comment: String,
    pub columns: Vec<SchemaColumn>,
}

/// One foreign key, possibly over several columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub constraint: String,
    pub database: String,
    pub table: String,
    pub columns: Vec<String>,
    pub referenced_database: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaGraph {
    pub tables: Vec<SchemaTable>,
    /// Foreign keys whose both ends are in `tables`.
    pub relations: Vec<Relation>,
    /// Total tables in the database; more than `tables.len()` when capped.
    pub total_tables: usize,
}
