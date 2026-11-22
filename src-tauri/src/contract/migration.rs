//! Schema compare and sync: the options, the diff result the UI shows, the migration steps a sync script is planned
//! from, and the per-engine [`SchemaDialect`] port that normalizes catalog text and renders the steps as SQL.
//!
//! Direction is always "make the target match the source". Nothing here runs anything: a sync script is text that the
//! UI opens in a query tab on the target, where the statement gateway applies the workspace's rules.

use serde::{Deserialize, Serialize};

use super::snapshot::{CheckDef, ColumnDef, ForeignKeyDef, IndexDef, KeyDef, SkippedObject, TableDef, ViewDef};

// --- Options ------------------------------------------------------------------------------------------------------------

/// Migration-tool bookkeeping tables, never synced by default (glob patterns).
pub const DEFAULT_EXCLUDES: [&str; 9] = [
    "flyway_schema_history",
    "__EFMigrationsHistory",
    "schema_migrations",
    "ar_internal_metadata",
    "knex_migrations*",
    "_prisma_migrations",
    "alembic_version",
    "django_migrations",
    "goose_db_version",
];

/// The object kinds compared in this version.
pub const COMPARED_KINDS: [&str; 2] = ["table", "view"];

/// What a compare ignores. Changing an option re-diffs the same snapshots without reading the databases again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CompareOptions {
    pub ignore_column_order: bool,
    pub ignore_comments: bool,
    pub ignore_collation: bool,
    pub ignore_defaults: bool,
    /// Off (the default) is "auto": constraint names matter only when both sides are user-named; system-named
    /// constraints match by definition. On: every constraint matches by definition.
    pub ignore_constraint_names: bool,
    pub ignore_index_names: bool,
    /// `None` = from the snapshots (either side folding case is enough).
    pub case_insensitive_names: Option<bool>,
    /// Kinds to compare (`table`, `view`).
    pub include_kinds: Vec<String>,
    /// Glob patterns (`*`, `?`, case-insensitive) of table and view names left out of the compare.
    pub exclude: Vec<String>,
    /// Wrap the script in a transaction; `None` = where the dialect's DDL is transactional.
    pub transactional: Option<bool>,
    /// Flag a lone dropped column next to a lone added one of the same type as a possible rename (a hint only).
    pub detect_renames: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            ignore_column_order: true,
            ignore_comments: false,
            ignore_collation: true,
            ignore_defaults: false,
            ignore_constraint_names: false,
            ignore_index_names: false,
            case_insensitive_names: None,
            include_kinds: COMPARED_KINDS.iter().map(|kind| (*kind).to_owned()).collect(),
            exclude: DEFAULT_EXCLUDES.iter().map(|pattern| (*pattern).to_owned()).collect(),
            transactional: None,
            detect_renames: true,
        }
    }
}

// --- Diff result --------------------------------------------------------------------------------------------------------

/// How risky a change is. Ordered: the risk of an item is the highest of its changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Risk {
    #[default]
    None,
    /// May fail on existing data, or needs a human look (possible rename, type family change).
    Review,
    /// Loses data (drop table or column, narrowing type change).
    DataLoss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffStatus {
    /// Will be created on the target.
    OnlyInSource,
    /// Would be dropped from the target.
    OnlyInTarget,
    Different,
    Identical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    /// In the source only: the script adds it to the target.
    Added,
    /// In the target only: the script removes it.
    Removed,
    Modified,
}

/// One attribute that differs; `None` where a side has no value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub field: String,
    pub source: Option<String>,
    pub target: Option<String>,
}

/// One difference inside an item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// `column`, `columnOrder`, `primaryKey`, `unique`, `check`, `index`, `foreignKey`, `comment`, `option`, `definition`.
    pub part: String,
    pub name: String,
    pub what: ChangeKind,
    pub fields: Vec<FieldChange>,
    pub risk: Risk,
    pub note: Option<String>,
    /// For a column: the column on the other side this one may be a rename of (a hint; the script drops and adds).
    pub possible_rename: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffItem {
    /// `table:orders`, `view:v_totals`: stable across runs (the source's spelling when both sides have it).
    pub id: String,
    /// `table` or `view`.
    pub kind: String,
    pub name: String,
    pub status: DiffStatus,
    pub changes: Vec<Change>,
    pub risk: Risk,
    /// Canonical DDL from the target dialect for each side, with ignored attributes left out.
    pub source_ddl: Option<String>,
    pub target_ddl: Option<String>,
}

/// One side of a compare.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSummary {
    pub engine: String,
    pub server_version: String,
    pub database: String,
    pub schema: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaDiff {
    pub source: ScopeSummary,
    pub target: ScopeSummary,
    pub options: CompareOptions,
    /// Tables first, then views, each by name.
    pub items: Vec<DiffItem>,
    /// Not compared on either side (unsupported kinds, excluded names).
    pub skipped: Vec<SkippedObject>,
}

// --- Migration steps ----------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConstraintKind {
    PrimaryKey,
    Unique,
    Check,
}

/// A unique or check constraint to add.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ConstraintDef {
    Unique { key: KeyDef },
    Check { check: CheckDef },
}

/// Which attributes of a column an `AlterColumn` changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnChanges {
    pub type_name: bool,
    pub nullability: bool,
    pub default: bool,
    pub identity: bool,
    pub generated: bool,
    pub collation: bool,
}

impl ColumnChanges {
    /// The target column `from` once these changes of the source column `to` are applied: what a dialect that restates
    /// whole columns (MySQL `MODIFY COLUMN`) writes, so the attributes that do not change (ignored ones included) and the
    /// comment stay as the target has them.
    pub fn apply(&self, from: &ColumnDef, to: &ColumnDef) -> ColumnDef {
        let mut column = from.clone();
        if self.type_name {
            column.type_name = to.type_name.clone();
        }
        if self.nullability {
            column.nullable = to.nullable;
        }
        if self.default {
            column.default = to.default.clone();
            column.default_name = to.default_name.clone();
        }
        if self.identity {
            column.identity = to.identity.clone();
        }
        if self.generated {
            column.generated = to.generated.clone();
        }
        if self.collation {
            column.collation = to.collation.clone();
        }
        column
    }
}

/// What a comment is on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentTarget {
    /// `table`, `view` or `materializedView`.
    pub kind: String,
    pub name: String,
    pub column: Option<String>,
    /// For a column: the column as it stands after the script's earlier steps, with the new comment, for dialects that
    /// restate a whole column to comment it (MySQL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<ColumnDef>,
}

/// One engine-neutral step of a sync script, in the order the planner puts them. Table names are the target's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "camelCase")]
pub enum MigrationStep {
    DropForeignKey {
        table: String,
        name: String,
    },
    DropView {
        name: String,
        materialized: bool,
    },
    DropIndex {
        table: String,
        name: String,
    },
    DropConstraint {
        table: String,
        kind: ConstraintKind,
        name: Option<String>,
    },
    DropTable {
        name: String,
    },
    /// Columns, primary key, uniques and checks; foreign keys come later as `AddForeignKey`.
    CreateTable {
        table: TableDef,
    },
    AddColumn {
        table: String,
        column: ColumnDef,
    },
    AlterColumn {
        table: String,
        from: ColumnDef,
        to: ColumnDef,
        changes: ColumnChanges,
    },
    DropColumn {
        table: String,
        column: String,
    },
    /// Create a new table, copy the mapped columns, drop the old one and rename (SQLite and any dialect that cannot
    /// alter in place). `column_map` is (source column, target column).
    RebuildTable {
        source_table: TableDef,
        target_table: TableDef,
        column_map: Vec<(String, String)>,
    },
    AddPrimaryKey {
        table: String,
        key: KeyDef,
    },
    AddConstraint {
        table: String,
        constraint: ConstraintDef,
    },
    CreateIndex {
        table: String,
        index: IndexDef,
    },
    CreateView {
        view: ViewDef,
        replace: bool,
    },
    AddForeignKey {
        table: String,
        key: ForeignKeyDef,
    },
    SetComment {
        target: CommentTarget,
        comment: Option<String>,
    },
    SetTableOption {
        table: String,
        name: String,
        value: String,
    },
    /// Anything the dialect cannot express: emitted as a commented block, never left out silently.
    Manual {
        reason: String,
        ddl_hint: Option<String>,
    },
}

/// What a renderer needs besides the step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderContext {
    /// The source scope's schema, mapped to `target_schema` in references and bodies.
    pub source_schema: Option<String>,
    /// The schema statements are qualified with.
    pub target_schema: Option<String>,
}

/// One statement of a script with what the user should know before running it (`-- WARNING:` lines).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedStatement {
    pub sql: String,
    pub risk: Risk,
    pub warnings: Vec<String>,
}

impl RenderedStatement {
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptWarning {
    pub item_id: String,
    pub risk: Risk,
    pub message: String,
}

/// A generated sync script: text only, never run by the backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncScript {
    pub sql: String,
    pub warnings: Vec<ScriptWarning>,
    pub statements: usize,
    pub transactional: bool,
    /// The engine id whose dialect wrote the script (the target's).
    pub dialect: String,
}

// --- Dialect port -------------------------------------------------------------------------------------------------------

/// What a dialect's DDL can do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DialectFeatures {
    /// DDL runs inside a transaction and rolls back (PostgreSQL, SQL Server, SQLite; not MySQL).
    pub transactional_ddl: bool,
    pub alter_column_type: bool,
    pub drop_column: bool,
    /// `RESTRICT` and `NO ACTION` behave the same on foreign keys (MySQL).
    pub restrict_equals_no_action: bool,
    /// A line that ends a batch (`GO` on SQL Server).
    pub batch_separator: Option<&'static str>,
    /// `CREATE TABLE` and `ADD COLUMN` carry the table's and the columns' comments (MySQL), so no separate comment steps
    /// follow them.
    pub inline_comments: bool,
}

/// Pure, per engine (no connection): normalizes catalog text for comparison, rates type changes and renders
/// migration steps. The `schema` passed to the normalizers is the scope schema of the side the text comes from; its
/// qualifier is dropped so both sides compare (see [`strip_schema_qualifier`]).
pub trait SchemaDialect: Send + Sync {
    fn normalize_type(&self, raw: &str, schema: Option<&str>) -> String;
    fn normalize_default(&self, raw: &str, schema: Option<&str>) -> String;
    /// Checks, index predicates, generated columns and view bodies.
    fn normalize_expression(&self, raw: &str, schema: Option<&str>) -> String;
    /// The risk of changing a target column `from` into the source's `to`.
    fn type_change_risk(&self, from: &ColumnDef, to: &ColumnDef) -> Risk;
    fn features(&self) -> DialectFeatures;
    /// Whether applying this item's changes needs a table rebuild (SQLite, beyond a plain ADD COLUMN).
    fn needs_rebuild(&self, _item: &DiffItem) -> bool {
        false
    }
    fn quote_ident(&self, name: &str) -> String;
    /// Canonical `CREATE TABLE` text: shown side by side and used to create tables.
    fn table_ddl(&self, schema: Option<&str>, table: &TableDef) -> String;
    fn view_ddl(&self, schema: Option<&str>, view: &ViewDef) -> String;
    fn render(&self, step: &MigrationStep, ctx: &RenderContext) -> Vec<RenderedStatement>;
    /// Adds the transaction wrapper (and batch separators) around a script body.
    fn wrap(&self, body: Vec<RenderedStatement>, transactional: bool) -> Vec<RenderedStatement>;
}

/// `text` without the `schema.` qualifier on identifiers (bare, `"schema".`, `` `schema`. `` or `[schema].`), outside
/// string literals and other quoted identifiers. Bare names match case-insensitively, quoted ones exactly. A qualifier
/// that follows a `.` (`db.schema.t`) is kept.
pub fn strip_schema_qualifier(text: &str, schema: &str) -> String {
    replace_schema_qualifier(text, schema, None)
}

/// `text` with the `schema.` qualifier replaced by `replacement.` (an already quoted identifier), or dropped when
/// `replacement` is `None`; the same rules as [`strip_schema_qualifier`]. Renderers use it to point references to the
/// source scope at the target scope.
pub fn replace_schema_qualifier(text: &str, schema: &str, replacement: Option<&str>) -> String {
    if schema.is_empty() {
        return text.to_owned();
    }
    let replace = |out: &mut String| {
        if let Some(replacement) = replacement {
            out.push_str(replacement);
            out.push('.');
        }
    };
    let is_ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let quoted = [format!("\"{schema}\"."), format!("`{schema}`."), format!("[{schema}].")];
    let mut out = String::with_capacity(text.len());
    let mut previous: Option<char> = None;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let after_dot_or_ident = previous.is_some_and(|p| p == '.' || is_ident(p));
        if !after_dot_or_ident {
            if let Some(pattern) = quoted.iter().find(|pattern| rest.starts_with(pattern.as_str())) {
                rest = &rest[pattern.len()..];
                replace(&mut out);
                previous = Some('.');
                continue;
            }
            let bare = rest.get(..schema.len()).is_some_and(|head| head.eq_ignore_ascii_case(schema));
            if bare && rest[schema.len()..].starts_with('.') {
                rest = &rest[schema.len() + 1..];
                replace(&mut out);
                previous = Some('.');
                continue;
            }
        }
        let close = match c {
            '\'' => Some('\''),
            '"' => Some('"'),
            '`' => Some('`'),
            '[' => Some(']'),
            _ => None,
        };
        let end = match close {
            // Copy a literal or quoted identifier whole; a doubled closing quote is an escape.
            Some(close) => {
                let mut end = c.len_utf8();
                loop {
                    match rest[end..].find(close) {
                        Some(at) => {
                            end += at + close.len_utf8();
                            if close != ']' && rest[end..].starts_with(close) {
                                end += close.len_utf8();
                                continue;
                            }
                            break end;
                        }
                        None => break rest.len(),
                    }
                }
            }
            None => c.len_utf8(),
        };
        out.push_str(&rest[..end]);
        previous = rest[..end].chars().last();
        rest = &rest[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualifiers_of_the_scope_are_stripped_outside_literals() {
        let strip = |text: &str| strip_schema_qualifier(text, "shop");
        assert_eq!(
            strip("SELECT * FROM shop.orders o JOIN \"shop\".items i"),
            "SELECT * FROM orders o JOIN items i"
        );
        assert_eq!(strip("SELECT `shop`.`t`.a FROM [shop].[t]"), "SELECT `t`.a FROM [t]");
        assert_eq!(strip("SHOP.orders"), "orders");
        assert_eq!(
            strip("x = 'shop.orders' AND y = \"a shop.b\""),
            "x = 'shop.orders' AND y = \"a shop.b\""
        );
        assert_eq!(
            strip("myshop.orders, db.shop.orders, shop_x.t"),
            "myshop.orders, db.shop.orders, shop_x.t"
        );
        assert_eq!(strip("'it''s shop.x' || shop.f()"), "'it''s shop.x' || f()");
        assert_eq!(strip_schema_qualifier("a.b", ""), "a.b");
    }

    #[test]
    fn qualifiers_of_the_scope_are_replaced_outside_literals() {
        let map = |text: &str| replace_schema_qualifier(text, "compare_a", Some("compare_b"));
        assert_eq!(
            map("SELECT o.id FROM compare_a.orders o JOIN \"compare_a\".items i ON 'compare_a.x' = i.y"),
            "SELECT o.id FROM compare_b.orders o JOIN compare_b.items i ON 'compare_a.x' = i.y"
        );
        assert_eq!(map("compare_ab.t, x.compare_a.t"), "compare_ab.t, x.compare_a.t");
    }

    #[test]
    fn risks_order_from_none_to_data_loss() {
        assert!(Risk::None < Risk::Review && Risk::Review < Risk::DataLoss);
        assert_eq!([Risk::Review, Risk::None].into_iter().max(), Some(Risk::Review));
    }

    #[test]
    fn options_fill_missing_fields_with_defaults() {
        let options: CompareOptions = serde_json::from_str(r#"{"ignoreComments":true}"#).unwrap();
        assert!(options.ignore_comments && options.ignore_column_order && options.detect_renames);
        assert_eq!(options.exclude.len(), DEFAULT_EXCLUDES.len());
        assert_eq!(options.include_kinds, ["table", "view"]);
    }
}
