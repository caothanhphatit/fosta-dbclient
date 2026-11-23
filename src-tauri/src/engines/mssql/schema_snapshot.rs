//! The schema compare snapshot of one SQL Server schema: one batch of bulk reads over the database's `sys` views
//! (tables, columns, indexes and key constraints, checks, foreign keys, views and their dependencies, objects not
//! compared), never one round trip per table.
//!
//! Names the server chose (`PK__orders__3213E83F`, `CK__orders__qty__…`) are flagged system-named, so the compare
//! matches those constraints by definition. Defaults are named constraints: the snapshot keeps each default's name so a
//! script can drop it. Filtered indexes keep their predicate and included columns; computed columns keep their
//! expression. A view's body is its `sys.sql_modules` text after `CREATE VIEW name [(columns)] [WITH ...] AS`; the column
//! list and the `WITH` attributes are its options. Without readable text (encrypted, no permission) it is unreadable.

use std::collections::HashMap;

use super::{
    catalog::{bracket, text_sets, TextRow, DEFAULT_SCHEMA},
    connect::TdsClient,
    values::type_label,
};
use crate::{
    contract::{
        CheckDef, ColumnDef, ForeignKeyDef, GeneratedDef, IdentityDef, IdentityKind, IndexDef, IndexPart, KeyDef, SchemaSnapshot,
        SkippedObject, SnapshotScope, TableDef, ViewDef,
    },
    error::{AppError, AppResult},
    lang::{tokenize, Dialect, TokenKind},
};

const NOT_COMPARED: &str = "Not compared in this version";

fn sql(sys: &str) -> String {
    let description = |major: &str, minor: &str| {
        format!(
            "LEFT JOIN {sys}.extended_properties ep ON ep.class = 1 AND ep.major_id = {major} AND ep.minor_id = {minor} \
             AND ep.name = 'MS_Description'"
        )
    };
    let in_scope = format!("JOIN {sys}.schemas s ON s.schema_id = o.schema_id AND s.name = @P1");
    format!(
        // 0: the server, the database's collation and whether the schema exists.
        "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128)), CAST(DATABASEPROPERTYEX(@P2, 'Collation') AS NVARCHAR(128)), \
                (SELECT COUNT(*) FROM {sys}.schemas WHERE name = @P1); \
         SELECT o.name, o.type, CAST(ep.value AS NVARCHAR(4000)) \
         FROM {sys}.objects o {in_scope} {} \
         WHERE o.type IN ('U', 'V') AND o.is_ms_shipped = 0 ORDER BY o.name; \
         SELECT o.name, c.name, COALESCE(bt.name, t.name), c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, \
                CAST(ic.seed_value AS NVARCHAR(40)), CAST(ic.increment_value AS NVARCHAR(40)), c.is_computed, cc.definition, \
                cc.is_persisted, dc.definition, dc.name, c.collation_name, CAST(ep.value AS NVARCHAR(4000)) \
         FROM {sys}.columns c \
         JOIN {sys}.objects o ON o.object_id = c.object_id AND o.type = 'U' AND o.is_ms_shipped = 0 {in_scope} \
         JOIN {sys}.types t ON t.user_type_id = c.user_type_id \
         LEFT JOIN {sys}.types bt ON bt.user_type_id = c.system_type_id \
         LEFT JOIN {sys}.default_constraints dc ON dc.object_id = c.default_object_id \
         LEFT JOIN {sys}.computed_columns cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id \
         LEFT JOIN {sys}.identity_columns ic ON ic.object_id = c.object_id AND ic.column_id = c.column_id {} \
         ORDER BY o.name, c.column_id; \
         SELECT o.name, i.name, i.is_primary_key, i.is_unique_constraint, i.is_unique, i.type, i.type_desc, i.filter_definition, \
                ic.is_included_column, ic.is_descending_key, c.name, kc.is_system_named \
         FROM {sys}.indexes i \
         JOIN {sys}.objects o ON o.object_id = i.object_id AND o.type = 'U' AND o.is_ms_shipped = 0 {in_scope} \
         JOIN {sys}.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
         JOIN {sys}.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
         LEFT JOIN {sys}.key_constraints kc ON kc.parent_object_id = i.object_id AND kc.unique_index_id = i.index_id \
         WHERE i.index_id > 0 AND i.is_hypothetical = 0 \
         ORDER BY o.name, i.name, ic.is_included_column, ic.key_ordinal, ic.index_column_id; \
         SELECT o.name, ck.name, ck.definition, ck.is_system_named \
         FROM {sys}.check_constraints ck JOIN {sys}.objects o ON o.object_id = ck.parent_object_id {in_scope} \
         ORDER BY o.name, ck.name; \
         SELECT fk.name, o.name, cp.name, sr.name, tr.name, cr.name, fk.update_referential_action_desc, \
                fk.delete_referential_action_desc, fk.is_system_named \
         FROM {sys}.foreign_keys fk \
         JOIN {sys}.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
         JOIN {sys}.objects o ON o.object_id = fk.parent_object_id {in_scope} \
         JOIN {sys}.columns cp ON cp.object_id = fkc.parent_object_id AND cp.column_id = fkc.parent_column_id \
         JOIN {sys}.objects tr ON tr.object_id = fk.referenced_object_id \
         JOIN {sys}.schemas sr ON sr.schema_id = tr.schema_id \
         JOIN {sys}.columns cr ON cr.object_id = fkc.referenced_object_id AND cr.column_id = fkc.referenced_column_id \
         ORDER BY o.name, fk.name, fkc.constraint_column_id; \
         SELECT o.name, m.definition, CAST(ep.value AS NVARCHAR(4000)) \
         FROM {sys}.objects o {in_scope} LEFT JOIN {sys}.sql_modules m ON m.object_id = o.object_id {} \
         WHERE o.type = 'V' AND o.is_ms_shipped = 0 ORDER BY o.name; \
         SELECT DISTINCT o.name, ro.name \
         FROM {sys}.sql_expression_dependencies d \
         JOIN {sys}.objects o ON o.object_id = d.referencing_id AND o.type = 'V' {in_scope} \
         JOIN {sys}.objects ro ON ro.object_id = d.referenced_id AND ro.schema_id = o.schema_id AND ro.type IN ('U', 'V') \
         ORDER BY o.name, ro.name; \
         SELECT o.type, o.name, p.name \
         FROM {sys}.objects o {in_scope} LEFT JOIN {sys}.objects p ON p.object_id = o.parent_object_id \
         WHERE o.is_ms_shipped = 0 AND o.type IN ('P', 'PC', 'X', 'FN', 'IF', 'TF', 'FS', 'FT', 'TR', 'SO', 'SN') \
         ORDER BY o.type, o.name; \
         SELECT t.name, t.is_table_type FROM {sys}.types t JOIN {sys}.schemas s ON s.schema_id = t.schema_id AND s.name = @P1 \
         WHERE t.is_user_defined = 1 ORDER BY t.name",
        description("o.object_id", "0"),
        description("c.object_id", "c.column_id"),
        description("o.object_id", "0"),
    )
}

/// Reads the snapshot of `scope.schema` (`dbo` when none) in `scope.database`.
pub async fn snapshot(client: &mut TdsClient, scope: &SnapshotScope) -> AppResult<SchemaSnapshot> {
    let database = scope.database.trim().to_owned();
    if database.is_empty() {
        return Err(AppError::new("Pick a database to compare"));
    }
    let schema = scope
        .schema
        .as_deref()
        .map(str::trim)
        .filter(|schema| !schema.is_empty())
        .unwrap_or(DEFAULT_SCHEMA)
        .to_owned();
    let sys = format!("{}.sys", bracket(&database));
    let sets = text_sets(client, &sql(&sys), &[&schema, &database]).await?;
    let set = |index: usize| sets.get(index).map(Vec::as_slice).unwrap_or_default();

    let head = set(0).first();
    if head.and_then(|row| row.number(2)).unwrap_or(0) == 0 {
        return Err(AppError::new(format!("There is no schema named '{schema}' in {database}")));
    }
    let server_version = head.map(|row| row.text(0)).unwrap_or_default();
    let database_collation = head.and_then(|row| row.opt(1)).unwrap_or_default();
    let case_insensitive_names = database_collation.to_ascii_uppercase().contains("_CI");

    let mut tables: Vec<TableDef> = Vec::new();
    let mut views: Vec<ViewDef> = Vec::new();
    let mut skipped: Vec<SkippedObject> = Vec::new();
    let mut skip = |kind: &str, name: String| {
        skipped.push(SkippedObject {
            kind: kind.to_owned(),
            name,
            reason: NOT_COMPARED.to_owned(),
        })
    };
    for row in set(1) {
        if row.text(1).trim() == "U" {
            tables.push(TableDef {
                comment: row.opt(2).filter(|c| !c.is_empty()),
                ..TableDef::new(&row.text(0))
            });
        }
    }
    let position: HashMap<String, usize> = tables.iter().enumerate().map(|(i, t)| (t.name.clone(), i)).collect();
    let at = |name: &str| position.get(name).copied();

    for row in set(2) {
        let Some(at) = at(&row.text(0)) else { continue };
        let table = &mut tables[at];
        table.columns.push(column_of(row, &database_collation));
    }

    for (table_name, index) in group_indexes(set(3)) {
        let Some(at) = at(&table_name) else { continue };
        let table = &mut tables[at];
        match index {
            GroupedIndex::PrimaryKey(key) => table.primary_key = Some(key),
            GroupedIndex::Unique(key) => table.uniques.push(key),
            GroupedIndex::Index(index) => table.indexes.push(index),
            GroupedIndex::Other(name, kind) => skip("index", format!("{table_name}.{name} ({})", kind.to_lowercase())),
        }
    }

    for row in set(4) {
        let Some(at) = at(&row.text(0)) else { continue };
        let table = &mut tables[at];
        table.checks.push(CheckDef {
            name: Some(row.text(1)),
            expression: row.text(2),
            system_named: row.flag(3),
        });
    }

    for (table_name, key) in group_foreign_keys(set(5), &schema) {
        if let Some(at) = at(&table_name) {
            let table = &mut tables[at];
            table.foreign_keys.push(key);
        }
    }

    let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();
    for row in set(7) {
        let (view, referenced) = (row.text(0), row.text(1));
        if view != referenced {
            dependencies.entry(view).or_default().push(referenced);
        }
    }
    for row in set(6) {
        let name = row.text(0);
        // NULL for an encrypted view, or without VIEW DEFINITION permission.
        let (definition, options, unreadable) = match row.opt(1).filter(|d| !d.trim().is_empty()) {
            Some(text) => {
                let (body, options) = view_parts(&text);
                (body, options, false)
            }
            None => (String::new(), Vec::new(), true),
        };
        views.push(ViewDef {
            definition,
            materialized: false,
            depends_on: dependencies.remove(&name).unwrap_or_default(),
            comment: row.opt(2).filter(|c| !c.is_empty()),
            name,
            options,
            unreadable,
        });
    }

    for row in set(8) {
        let kind = match row.text(0).trim() {
            "P" | "PC" | "X" => "procedure",
            "FN" | "IF" | "TF" | "FS" | "FT" => "function",
            "TR" => "trigger",
            "SO" => "sequence",
            "SN" => "synonym",
            _ => continue,
        };
        let name = match (kind, row.opt(2)) {
            ("trigger", Some(parent)) => format!("{parent}.{}", row.text(1)),
            _ => row.text(1),
        };
        skip(kind, name);
    }
    for row in set(9) {
        skip(if row.flag(1) { "table type" } else { "type" }, row.text(0));
    }

    Ok(SchemaSnapshot {
        engine: super::descriptor::ID.to_owned(),
        server_version,
        scope: SnapshotScope::new(&database, Some(&schema)),
        case_insensitive_names,
        tables,
        views,
        skipped,
        read_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// A column row: (table, name, type, max_length, precision, scale, nullable, identity, seed, increment, computed,
/// computed definition, persisted, default, default name, collation, comment). A collation equal to the database's
/// reads as none.
fn column_of(row: &TextRow, database_collation: &str) -> ColumnDef {
    let type_name = type_label(
        &row.text(2),
        row.number(3).unwrap_or(0),
        row.number(4).unwrap_or(0),
        row.number(5).unwrap_or(0),
    );
    let mut column = ColumnDef::new(&row.text(1), &type_name, row.flag(6));
    if row.flag(7) {
        column.identity = Some(IdentityDef {
            kind: IdentityKind::ByDefault,
            start: row.number(8),
            increment: row.number(9),
        });
    }
    if row.flag(10) {
        column.generated = Some(GeneratedDef {
            expression: row.text(11),
            stored: row.flag(12),
        });
    }
    column.default = row.opt(13);
    column.default_name = row.opt(14).filter(|_| column.default.is_some());
    column.collation = row.opt(15).filter(|c| !c.eq_ignore_ascii_case(database_collation));
    column.comment = row.opt(16).filter(|c| !c.is_empty());
    column
}

#[derive(Debug, PartialEq)]
enum GroupedIndex {
    PrimaryKey(KeyDef),
    Unique(KeyDef),
    Index(IndexDef),
    /// XML, spatial, columnstore … indexes: (name, type description).
    Other(String, String),
}

/// Index rows (table, index, primary key, unique constraint, unique, type, type description, filter, included,
/// descending, column, system-named) ordered by table and index, folded into keys and indexes. A clustered index keeps
/// `CLUSTERED` as its method; a nonclustered one (the default) has none.
fn group_indexes(rows: &[TextRow]) -> Vec<(String, GroupedIndex)> {
    let mut out: Vec<(String, String, GroupedIndex)> = Vec::new();
    for row in rows {
        let (table, name) = (row.text(0), row.text(1));
        let column = row.text(10);
        let fresh = !matches!(out.last(), Some((t, n, _)) if *t == table && *n == name);
        if fresh {
            let kind = row.number(5).unwrap_or(0);
            let grouped = if !matches!(kind, 1 | 2) {
                GroupedIndex::Other(name.clone(), row.text(6))
            } else if row.flag(2) || row.flag(3) {
                let key = KeyDef {
                    name: Some(name.clone()),
                    columns: Vec::new(),
                    system_named: row.flag(11),
                };
                if row.flag(2) {
                    GroupedIndex::PrimaryKey(key)
                } else {
                    GroupedIndex::Unique(key)
                }
            } else {
                GroupedIndex::Index(IndexDef {
                    name: name.clone(),
                    parts: Vec::new(),
                    unique: row.flag(4),
                    method: (kind == 1).then(|| "CLUSTERED".to_owned()),
                    predicate: row.opt(7).filter(|p| !p.is_empty()),
                    include: Vec::new(),
                    system_named: false,
                })
            };
            out.push((table.clone(), name.clone(), grouped));
        }
        let Some((_, _, grouped)) = out.last_mut() else { continue };
        match grouped {
            GroupedIndex::PrimaryKey(key) | GroupedIndex::Unique(key) => key.columns.push(column),
            GroupedIndex::Index(index) if row.flag(8) => index.include.push(column),
            GroupedIndex::Index(index) => index.parts.push(IndexPart {
                column: Some(column),
                expression: None,
                descending: row.flag(9),
            }),
            GroupedIndex::Other(..) => {}
        }
    }
    out.into_iter().map(|(table, _, grouped)| (table, grouped)).collect()
}

/// Foreign key rows (name, table, column, referenced schema, table, column, update rule, delete rule, system-named)
/// ordered by table and key, one key per constraint. A reference into `schema` has no schema.
fn group_foreign_keys(rows: &[TextRow], schema: &str) -> Vec<(String, ForeignKeyDef)> {
    let rule = |text: Option<String>| text.map(|text| text.replace('_', " "));
    let mut keys: Vec<(String, ForeignKeyDef)> = Vec::new();
    for row in rows {
        let (name, table) = (row.text(0), row.text(1));
        match keys.last_mut() {
            Some((last_table, key)) if *last_table == table && key.name.as_deref() == Some(name.as_str()) => {
                key.columns.push(row.text(2));
                key.ref_columns.push(row.text(5));
            }
            _ => {
                let ref_schema = row.text(3);
                keys.push((
                    table,
                    ForeignKeyDef {
                        name: Some(name),
                        columns: vec![row.text(2)],
                        ref_schema: (!ref_schema.eq_ignore_ascii_case(schema)).then_some(ref_schema),
                        ref_table: row.text(4),
                        ref_columns: vec![row.text(5)],
                        on_update: rule(row.opt(6)),
                        on_delete: rule(row.opt(7)),
                        system_named: row.flag(8),
                    },
                ))
            }
        }
    }
    keys
}

/// The body of a view from its `sys.sql_modules` text: what follows the `AS` that ends `CREATE [OR ALTER] VIEW name
/// [(columns)] [WITH options]`, outside comments and brackets. The whole text when it does not read like that.
#[cfg(test)]
pub(super) fn view_body(definition: &str) -> String {
    view_parts(definition).0
}

/// The body of a view and the options of its header: the column list (`columns`, the text inside the parentheses) and
/// the `WITH` attributes (`schemabinding`, `viewMetadata`, `encryption`), which a recreated view must keep.
pub(super) fn view_parts(definition: &str) -> (String, Vec<(String, String)>) {
    let tokens = tokenize(Dialect::MsSql, definition);
    let mut seen_view = false;
    let mut seen_with = false;
    let mut depth = 0i32;
    let mut columns_from: Option<usize> = None;
    let mut options: Vec<(String, String)> = Vec::new();
    for token in tokens.iter().filter(|token| !token.is_trivia()) {
        match token.kind {
            TokenKind::Punct if token.is_punct(definition, '(') => {
                if seen_view && depth == 0 && !seen_with && options.is_empty() {
                    columns_from = Some(token.end);
                }
                depth += 1;
            }
            TokenKind::Punct if token.is_punct(definition, ')') => {
                depth -= 1;
                if depth == 0 {
                    if let Some(from) = columns_from.take() {
                        options.push(("columns".into(), definition[from..token.start].trim().to_owned()));
                    }
                }
            }
            TokenKind::Word if !seen_view && token.is_word(definition, "VIEW") => seen_view = true,
            TokenKind::Word if seen_view && depth == 0 && token.is_word(definition, "AS") => {
                let body = definition[token.end..].trim().trim_end_matches(';').trim_end().to_owned();
                return (body, options);
            }
            TokenKind::Word if seen_view && depth == 0 && token.is_word(definition, "WITH") => seen_with = true,
            TokenKind::Word if seen_with && depth == 0 => {
                let option = match token.text(definition).to_ascii_uppercase().as_str() {
                    "SCHEMABINDING" => Some("schemabinding"),
                    "VIEW_METADATA" => Some("viewMetadata"),
                    "ENCRYPTION" => Some("encryption"),
                    _ => None,
                };
                if let Some(option) = option {
                    options.push((option.into(), "on".into()));
                }
            }
            _ => {}
        }
    }
    (definition.trim().to_owned(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(values: &[Option<&str>]) -> TextRow {
        TextRow(values.iter().map(|value| value.map(str::to_owned)).collect())
    }

    fn some(values: &[&str]) -> TextRow {
        TextRow(values.iter().map(|value| Some((*value).to_owned())).collect())
    }

    #[test]
    fn view_bodies_follow_the_header() {
        assert_eq!(
            view_body("CREATE VIEW [s].[v] AS\n  SELECT a AS b FROM t;\n"),
            "SELECT a AS b FROM t"
        );
        assert_eq!(
            view_body("-- AS a comment\ncreate or alter view dbo.v (x, y) with schemabinding as select 1 as x, 2 as y"),
            "select 1 as x, 2 as y"
        );
        assert_eq!(view_body("CREATE VIEW [AS] AS SELECT 1"), "SELECT 1");
        assert_eq!(view_body("SELECT 1"), "SELECT 1");
    }

    #[test]
    fn columns_keep_identity_computed_defaults_and_odd_collations() {
        let id = column_of(
            &row(&[
                Some("orders"),
                Some("id"),
                Some("bigint"),
                Some("8"),
                Some("19"),
                Some("0"),
                Some("false"),
                Some("true"),
                Some("1"),
                Some("1"),
                Some("false"),
                None,
                None,
                None,
                None,
                None,
                Some("The key"),
            ]),
            "SQL_Latin1_General_CP1_CI_AS",
        );
        assert_eq!(id.type_name, "bigint");
        assert!(!id.nullable);
        assert_eq!(
            id.identity.as_ref().map(|i| (i.kind, i.start, i.increment)),
            Some((IdentityKind::ByDefault, Some(1), Some(1)))
        );
        assert_eq!(id.comment.as_deref(), Some("The key"));
        let status = column_of(
            &row(&[
                Some("orders"),
                Some("status"),
                Some("nvarchar"),
                Some("40"),
                Some("0"),
                Some("0"),
                Some("true"),
                Some("false"),
                None,
                None,
                Some("false"),
                None,
                None,
                Some("(N'new')"),
                Some("DF__orders__statu__1A2B3C4D"),
                Some("SQL_Latin1_General_CP1_CI_AS"),
                None,
            ]),
            "SQL_Latin1_General_CP1_CI_AS",
        );
        assert_eq!(status.type_name, "nvarchar(20)");
        assert_eq!(status.default.as_deref(), Some("(N'new')"));
        assert_eq!(status.default_name.as_deref(), Some("DF__orders__statu__1A2B3C4D"));
        assert_eq!(status.collation, None);
        let total = column_of(
            &row(&[
                Some("orders"),
                Some("total"),
                Some("decimal"),
                Some("9"),
                Some("14"),
                Some("2"),
                Some("true"),
                Some("false"),
                None,
                None,
                Some("true"),
                Some("([qty]*[price])"),
                Some("true"),
                None,
                None,
                None,
                None,
            ]),
            "Latin1_General_100_CS_AS",
        );
        assert_eq!(total.type_name, "decimal(14,2)");
        assert_eq!(
            total.generated,
            Some(GeneratedDef {
                expression: "([qty]*[price])".into(),
                stored: true
            })
        );
    }

    #[test]
    fn index_rows_fold_into_keys_and_indexes() {
        // table, index, pk, unique constraint, unique, type, type_desc, filter, included, descending, column, system-named
        let rows = vec![
            some(&[
                "orders",
                "ix_orders_placed",
                "false",
                "false",
                "false",
                "2",
                "NONCLUSTERED",
                "([placed] IS NOT NULL)",
                "false",
                "true",
                "placed",
                "",
            ]),
            some(&[
                "orders",
                "ix_orders_placed",
                "false",
                "false",
                "false",
                "2",
                "NONCLUSTERED",
                "([placed] IS NOT NULL)",
                "true",
                "false",
                "note",
                "",
            ]),
            some(&[
                "orders",
                "PK__orders__3213E83F",
                "true",
                "false",
                "true",
                "1",
                "CLUSTERED",
                "",
                "false",
                "false",
                "id",
                "true",
            ]),
            some(&[
                "orders",
                "uq_orders_code",
                "false",
                "true",
                "true",
                "2",
                "NONCLUSTERED",
                "",
                "false",
                "false",
                "code",
                "false",
            ]),
            some(&[
                "orders",
                "uq_orders_code",
                "false",
                "true",
                "true",
                "2",
                "NONCLUSTERED",
                "",
                "false",
                "false",
                "region",
                "false",
            ]),
            some(&[
                "orders",
                "cx_orders",
                "false",
                "false",
                "true",
                "1",
                "CLUSTERED",
                "",
                "false",
                "false",
                "code",
                "",
            ]),
            some(&[
                "orders", "xml_ix", "false", "false", "false", "3", "XML", "", "false", "false", "doc", "",
            ]),
        ];
        let grouped = group_indexes(&rows);
        assert_eq!(grouped.len(), 5);
        assert_eq!(
            grouped[0].1,
            GroupedIndex::Index(IndexDef {
                name: "ix_orders_placed".into(),
                parts: vec![IndexPart {
                    column: Some("placed".into()),
                    expression: None,
                    descending: true
                }],
                unique: false,
                method: None,
                predicate: Some("([placed] IS NOT NULL)".into()),
                include: vec!["note".into()],
                system_named: false,
            })
        );
        assert_eq!(
            grouped[1].1,
            GroupedIndex::PrimaryKey(KeyDef {
                name: Some("PK__orders__3213E83F".into()),
                columns: vec!["id".into()],
                system_named: true
            })
        );
        assert!(matches!(&grouped[2].1, GroupedIndex::Unique(key) if key.columns == ["code", "region"] && !key.system_named));
        assert!(matches!(&grouped[3].1, GroupedIndex::Index(index) if index.unique && index.method.as_deref() == Some("CLUSTERED")));
        assert_eq!(grouped[4].1, GroupedIndex::Other("xml_ix".into(), "XML".into()));
    }

    #[test]
    fn foreign_key_rows_fold_and_drop_the_scope_schema() {
        let rows = vec![
            some(&[
                "fk_items_order",
                "items",
                "order_id",
                "sales",
                "orders",
                "id",
                "NO_ACTION",
                "CASCADE",
                "false",
            ]),
            some(&[
                "fk_items_order",
                "items",
                "line",
                "sales",
                "orders",
                "line",
                "NO_ACTION",
                "CASCADE",
                "false",
            ]),
            some(&[
                "FK__items__sku__1",
                "items",
                "sku",
                "dbo",
                "skus",
                "sku",
                "SET_NULL",
                "NO_ACTION",
                "true",
            ]),
        ];
        let keys = group_foreign_keys(&rows, "sales");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].1.columns, ["order_id", "line"]);
        assert_eq!(keys[0].1.ref_schema, None);
        assert_eq!(keys[0].1.on_delete.as_deref(), Some("CASCADE"));
        assert_eq!(keys[1].1.ref_schema.as_deref(), Some("dbo"));
        assert_eq!(keys[1].1.on_update.as_deref(), Some("SET NULL"));
        assert!(keys[1].1.system_named);
    }
}
