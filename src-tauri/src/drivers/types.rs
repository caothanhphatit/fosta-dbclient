//! Engine-independent data shapes exchanged between the UI, the Tauri
//! commands and every [`Driver`](super::Driver). Field names are serialised in
//! camelCase and mirror `src/core/types.ts`; changing them breaks the UI.

use std::{fmt::Write as _, time::Instant};

use serde::{Deserialize, Serialize};

/// One cell rendered as text; `None` is SQL NULL (or a missing document field).
pub type Cell = Option<String>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
    pub numeric: bool,
    pub binary: bool,
    pub primary: bool,
    pub vector_dims: Option<u32>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSet {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub affected_rows: u64,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryOutcome {
    pub sets: Vec<ResultSet>,
    pub elapsed_ms: u64,
    /// The session's transaction state as reported by the engine after the run.
    pub transaction: TransactionState,
}

pub use crate::contract::session::{TransactionOp, TransactionState, TRANSACTION_ALREADY_OPEN};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    pub name: String,
    pub kind: String,
    pub rows: Option<u64>,
    pub comment: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDesign {
    pub name: String,
    pub column_type: String,
    pub nullable: bool,
    pub key: String,
    pub default_value: Cell,
    pub extra: String,
    pub comment: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableDesign {
    pub columns: Vec<ColumnDesign>,
    pub ddl: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TablePage {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub total: u64,
    pub primary_key: Vec<String>,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseRequest {
    pub connection_id: String,
    pub database: String,
    pub table: String,
    pub offset: u64,
    pub limit: u64,
    pub order_by: Option<String>,
    pub descending: bool,
    pub filter: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CellValue {
    pub column: String,
    pub value: Cell,
}

/// One pending grid edit. `key` holds the primary-key cells that identify the row.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RowChange {
    Insert { values: Vec<CellValue> },
    Update { key: Vec<CellValue>, values: Vec<CellValue> },
    Delete { key: Vec<CellValue> },
}

/// A catalog row read as text, addressed by column index.
pub(crate) struct TextRow(pub(crate) Vec<Cell>);

impl TextRow {
    pub(crate) fn text(&self, index: usize) -> String {
        self.optional(index).unwrap_or_default()
    }

    pub(crate) fn optional(&self, index: usize) -> Cell {
        self.0.get(index).cloned().flatten()
    }
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(2 + bytes.len() * 2);
    text.push_str("0x");
    for byte in bytes {
        let _ = write!(text, "{byte:02X}");
    }
    text
}

pub(crate) fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
