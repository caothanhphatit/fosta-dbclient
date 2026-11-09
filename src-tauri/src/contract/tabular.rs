//! Browsing and editing rows (or documents) of one object.
//!
//! Edits use optimistic concurrency: no transaction is held while the user edits in the grid. Every update and delete
//! carries what the row looked like when it was read (the key, the original values of the edited columns, and the row
//! version where the engine has one), and the engine applies it only if the row is unchanged. If any change matches no
//! row, the whole apply is rolled back and a structured `CONFLICT` error lists each conflicting row with its original,
//! current and attempted values (or that it no longer exists).

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{describe::ColumnInfo, descriptor::RawFilterLang, errors::CONFLICT, object::ObjectRef, value::Value};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    pub const ALL: [FilterOp; 17] = [
        FilterOp::Eq,
        FilterOp::Ne,
        FilterOp::Lt,
        FilterOp::Le,
        FilterOp::Gt,
        FilterOp::Ge,
        FilterOp::Contains,
        FilterOp::NotContains,
        FilterOp::StartsWith,
        FilterOp::EndsWith,
        FilterOp::Like,
        FilterOp::NotLike,
        FilterOp::In,
        FilterOp::NotIn,
        FilterOp::Between,
        FilterOp::IsNull,
        FilterOp::IsNotNull,
    ];

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterCond {
    pub column: String,
    pub op: FilterOp,
    #[serde(default)]
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

    /// Checks the number of values against the operator.
    pub fn validate(&self) -> AppResult<()> {
        let ok = match self.op.arity() {
            Some(n) => self.values.len() == n,
            None => !self.values.is_empty(),
        };
        if ok {
            Ok(())
        } else {
            Err(AppError::new(format!(
                "The filter on {} has {} value(s), which does not fit its operator.",
                self.column,
                self.values.len()
            )))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortKey {
    pub column: String,
    #[serde(default)]
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

/// Structured conditions are always bound as parameters by the engine. `Raw` is the engine's own filter language and is
/// allowed only when the connection has `Capability::RawFilter`; it is validated by `Engine::validate_filter`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Filter {
    #[default]
    None,
    #[serde(rename_all = "camelCase")]
    Conditions {
        conditions: Vec<FilterCond>,
        #[serde(default)]
        match_any: bool,
    },
    Raw {
        language: RawFilterLang,
        text: String,
    },
}

impl Filter {
    pub fn is_none(&self) -> bool {
        match self {
            Filter::None => true,
            Filter::Conditions { conditions, .. } => conditions.is_empty(),
            Filter::Raw { text, .. } => text.trim().is_empty(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseRequest {
    pub object: ObjectRef,
    #[serde(default)]
    pub filter: Filter,
    #[serde(default)]
    pub sort: Vec<SortKey>,
    #[serde(default)]
    pub offset: u64,
    pub limit: u64,
    /// Keyset cursor: the `next_keyset` of the previous page. When set, `offset` is ignored.
    #[serde(default)]
    pub after: Option<Vec<Value>>,
    /// Ask for an estimated total (statistics for unfiltered tables, the planner's estimate otherwise).
    #[serde(default)]
    pub estimate_total: bool,
    /// Ask for an exact `COUNT(*)` (the user clicked "count exactly").
    #[serde(default)]
    pub exact_total: bool,
    /// Only these columns (all when `None`).
    #[serde(default)]
    pub columns: Option<Vec<String>>,
    /// Read from the end ("Last page" without a deep OFFSET): the order is reversed (sort keys and the key tiebreak), the
    /// rows still come in the normal order. `has_more` and `next_keyset` then point toward the start: `after` set to a
    /// reversed page's `next_keyset` reads the page before it. SQL engines only; the others refuse it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reverse: bool,
}

impl BrowseRequest {
    pub fn new(object: ObjectRef, limit: u64) -> Self {
        Self {
            object,
            filter: Filter::None,
            sort: Vec::new(),
            offset: 0,
            limit,
            after: None,
            estimate_total: false,
            exact_total: false,
            columns: None,
            reverse: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Total {
    pub value: u64,
    pub exact: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<Value>>,
    pub has_more: bool,
    /// Columns that identify a row for edits (primary key, `_id`, or the engine's row id).
    pub key_columns: Vec<String>,
    /// Pass back as `after` to get the next page by keyset; `None` when keyset paging is not possible for this order.
    #[serde(default)]
    pub next_keyset: Option<Vec<Value>>,
    #[serde(default)]
    pub total: Option<Total>,
    /// What was run (with placeholders), for the UI's "show query".
    #[serde(default)]
    pub statement: Option<String>,
    pub elapsed_ms: u64,
    /// One row version per row (PostgreSQL `xmin`, SQL Server `rowversion`) when the engine has one; the UI sends it back
    /// as `RowChange::version` so the engine can check the row is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_versions: Option<Vec<Value>>,
}

/// A column and its value. `column` may be a dotted path for documents (`address.city`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub column: String,
    pub value: Value,
}

impl Field {
    pub fn new(column: &str, value: Value) -> Self {
        Self {
            column: column.to_owned(),
            value,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One row edit.
///
/// - `key` identifies the row as it was read (primary key or `_id`).
/// - `original` holds the values the edited columns had when the row was read (for a delete: the values of the row's
///   comparable columns); `version` the row version from `Page::row_versions`.
/// - `force` skips the check (the user chose "Overwrite" in the conflict dialog); a row that no longer exists is still a
///   conflict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum RowChange {
    Insert {
        values: Vec<Field>,
    },
    Update {
        key: Vec<Field>,
        values: Vec<Field>,
        #[serde(default)]
        original: Vec<Field>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<Value>,
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
    },
    Delete {
        key: Vec<Field>,
        #[serde(default)]
        original: Vec<Field>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<Value>,
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
    },
}

/// How an engine must guard one update or delete.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RowCheck<'a> {
    /// Only the key (forced overwrite, or nothing to compare): a missing row is still detected.
    KeyOnly,
    /// `WHERE key AND version = ?`.
    Version(&'a Value),
    /// `WHERE key AND col <=> ? …` over these original values (NULL-safe).
    Original(&'a [Field]),
}

impl RowChange {
    pub fn op_name(&self) -> &'static str {
        match self {
            RowChange::Insert { .. } => "insert",
            RowChange::Update { .. } => "update",
            RowChange::Delete { .. } => "delete",
        }
    }

    pub fn key(&self) -> &[Field] {
        match self {
            RowChange::Insert { .. } => &[],
            RowChange::Update { key, .. } | RowChange::Delete { key, .. } => key,
        }
    }

    /// The check an engine applies: the version when there is one, else the original values; nothing extra when forced.
    pub fn check(&self) -> RowCheck<'_> {
        match self {
            RowChange::Insert { .. } => RowCheck::KeyOnly,
            RowChange::Update {
                original, version, force, ..
            }
            | RowChange::Delete {
                original, version, force, ..
            } => match (force, version, original.is_empty()) {
                (true, _, _) => RowCheck::KeyOnly,
                (false, Some(version), _) => RowCheck::Version(version),
                (false, None, false) => RowCheck::Original(original),
                (false, None, true) => RowCheck::KeyOnly,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowChangeSet {
    pub object: ObjectRef,
    pub changes: Vec<RowChange>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    pub inserted: u64,
    pub updated: u64,
    pub deleted: u64,
}

/// One change that matched no row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowConflict {
    /// Position of the change in `RowChangeSet::changes`.
    pub index: usize,
    /// `"update"` or `"delete"`.
    pub op: String,
    pub key: Vec<Field>,
    /// When the row was read (the change's `original`).
    pub original: Vec<Field>,
    /// The row as it is now, re-read after the rollback (all columns); `None` when the row no longer exists.
    pub current: Option<Vec<Field>>,
    /// What the user tried to write (the update's values; empty for a delete).
    pub attempted: Vec<Field>,
}

impl RowConflict {
    pub fn new(index: usize, change: &RowChange, current: Option<Vec<Field>>) -> Self {
        let (original, attempted) = match change {
            RowChange::Insert { values } => (Vec::new(), values.clone()),
            RowChange::Update { values, original, .. } => (original.clone(), values.clone()),
            RowChange::Delete { original, .. } => (original.clone(), Vec::new()),
        };
        Self {
            index,
            op: change.op_name().to_owned(),
            key: change.key().to_vec(),
            original,
            current,
            attempted,
        }
    }
}

/// The structured `CONFLICT` error: nothing was applied.
pub fn conflict_error(object: &ObjectRef, conflicts: Vec<RowConflict>) -> AppError {
    let missing = conflicts.iter().filter(|c| c.current.is_none()).count();
    let changed = conflicts.len() - missing;
    let mut parts = Vec::new();
    if changed > 0 {
        parts.push(format!("{changed} row(s) were changed by someone else"));
    }
    if missing > 0 {
        parts.push(format!("{missing} row(s) no longer exist"));
    }
    AppError::structured(
        format!(
            "Nothing was saved: {} since they were loaded from {}.",
            parts.join(" and "),
            object.display_name()
        ),
        json!({ "code": CONFLICT, "object": object, "conflicts": conflicts }),
    )
}

/// Turns the conflicts of a rolled-back apply into the result: `Ok` when there were none.
pub fn check_conflicts(object: &ObjectRef, conflicts: Vec<RowConflict>, result: ApplyResult) -> AppResult<ApplyResult> {
    if conflicts.is_empty() {
        Ok(result)
    } else {
        Err(conflict_error(object, conflicts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(column: &str, i: i64) -> Field {
        Field::new(column, Value::Int(i))
    }

    #[test]
    fn filters_and_changes_have_a_tagged_wire_form() {
        let filter: Filter = serde_json::from_value(json!({
            "kind": "conditions", "matchAny": false,
            "conditions": [ { "column": "status", "op": "eq", "values": ["PAID"] } ]
        }))
        .unwrap();
        assert!(matches!(&filter, Filter::Conditions { conditions, .. } if conditions[0].op == FilterOp::Eq));
        assert!(Filter::default().is_none());
        let change: RowChange = serde_json::from_value(json!({
            "op": "update", "key": [ { "column": "id", "value": { "t": "int", "v": 1 } } ],
            "values": [ { "column": "status", "value": { "t": "text", "v": "SHIPPED" } } ],
            "original": [ { "column": "status", "value": { "t": "text", "v": "PAID" } } ]
        }))
        .unwrap();
        assert!(matches!(change.check(), RowCheck::Original(fields) if fields.len() == 1));
        let back = serde_json::to_value(&change).unwrap();
        assert!(back.get("force").is_none() && back.get("version").is_none());
    }

    #[test]
    fn version_wins_and_force_skips_the_check() {
        let mut change = RowChange::Update {
            key: vec![int("id", 1)],
            values: vec![int("n", 2)],
            original: vec![int("n", 1)],
            version: Some(Value::Bytes(vec![0, 0, 0, 1])),
            force: false,
        };
        assert!(matches!(change.check(), RowCheck::Version(_)));
        if let RowChange::Update { force, .. } = &mut change {
            *force = true;
        }
        assert_eq!(change.check(), RowCheck::KeyOnly);
        let delete = RowChange::Delete {
            key: vec![int("id", 1)],
            original: vec![],
            version: None,
            force: false,
        };
        assert_eq!(delete.check(), RowCheck::KeyOnly);
    }

    #[test]
    fn conflicts_list_original_current_and_attempted_values() {
        let object = ObjectRef::table("shop", None, "orders");
        let update = RowChange::Update {
            key: vec![int("id", 1)],
            values: vec![int("n", 3)],
            original: vec![int("n", 1)],
            version: None,
            force: false,
        };
        let delete = RowChange::Delete {
            key: vec![int("id", 2)],
            original: vec![],
            version: None,
            force: false,
        };
        let error = conflict_error(
            &object,
            vec![
                RowConflict::new(0, &update, Some(vec![int("id", 1), int("n", 2)])),
                RowConflict::new(1, &delete, None),
            ],
        );
        assert_eq!(error.code(), Some(CONFLICT));
        let detail = error.payload().unwrap();
        assert_eq!(detail["conflicts"][0]["attempted"][0]["value"]["v"], 3);
        assert_eq!(detail["conflicts"][0]["current"][1]["value"]["v"], 2);
        assert!(detail["conflicts"][1]["current"].is_null());
        assert!(error.to_string().contains("1 row(s) no longer exist"), "{error}");
        assert!(check_conflicts(&object, vec![], ApplyResult::default()).is_ok());
    }
}
