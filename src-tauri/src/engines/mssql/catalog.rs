//! The SQL Server catalog from the `sys` views of each database (three-part names, so any pooled client can read any
//! database): databases and tables for the tree, one object's description, and the schema graph in bulk queries.

use std::collections::{HashMap, HashSet};

use futures_util::TryStreamExt;
use tiberius::{QueryItem, ToSql};

use super::{
    connect::TdsClient,
    values::{is_clr_type, kind_of, plain_text, type_label},
};
use crate::{
    contract::{
        BuiltinAction, ColumnHints, ColumnInfo, ForeignKeyInfo, IndexInfo, KeyInfo, NodeAction, NodeKey, NodeKind, ObjectDescription,
        ObjectNode, ObjectRef, ReferencingKey, Relation, SchemaColumn, SchemaGraph, SchemaTable, ValueKind,
    },
    error::{AppError, AppResult},
};

pub const DEFAULT_SCHEMA: &str = "dbo";
/// Databases the server keeps for itself; shown dimmed in the tree.
pub const SYSTEM_DATABASES: [&str; 4] = ["master", "model", "msdb", "tempdb"];

/// A catalog row with every value as text.
pub struct TextRow(pub Vec<Option<String>>);

impl TextRow {
    pub fn text(&self, index: usize) -> String {
        self.opt(index).unwrap_or_default()
    }

    pub fn opt(&self, index: usize) -> Option<String> {
        self.0.get(index).cloned().flatten()
    }

    pub fn number(&self, index: usize) -> Option<i64> {
        self.opt(index)?.trim().parse().ok()
    }

    /// A `bit` column (`true`/`false`, or `1`/`0` from a CASE).
    pub fn flag(&self, index: usize) -> bool {
        matches!(self.opt(index).as_deref(), Some("true" | "1"))
    }
}

/// Every result set of a statement batch (empty ones included), every value as text.
pub async fn text_sets(client: &mut TdsClient, sql: &str, params: &[&dyn ToSql]) -> AppResult<Vec<Vec<TextRow>>> {
    let mut stream = client.query(sql, params).await?;
    let mut sets: Vec<Vec<TextRow>> = Vec::new();
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(_) => sets.push(Vec::new()),
            QueryItem::Row(row) => {
                if sets.is_empty() {
                    sets.push(Vec::new());
                }
                let values = row.into_iter().map(plain_text).collect();
                sets.last_mut().expect("a set exists").push(TextRow(values));
            }
        }
    }
    Ok(sets)
}

/// The rows of the first result set.
pub async fn text_rows(client: &mut TdsClient, sql: &str, params: &[&dyn ToSql]) -> AppResult<Vec<TextRow>> {
    Ok(text_sets(client, sql, params).await?.into_iter().next().unwrap_or_default())
}

pub fn bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// The tree's label and the schema graph's table name: the default schema is left out.
pub fn object_name(schema: &str, name: &str) -> String {
    if schema.eq_ignore_ascii_case(DEFAULT_SCHEMA) {
        name.to_owned()
    } else {
        format!("{schema}.{name}")
    }
}

/// A table or view: database, schema and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub database: String,
    pub schema: String,
    pub name: String,
}

impl Target {
    /// `[db].[schema].[name]`.
    pub fn qualified(&self) -> String {
        format!("{}.{}.{}", bracket(&self.database), bracket(&self.schema), bracket(&self.name))
    }

    /// `[db].sys` for catalog views.
    pub fn sys(&self) -> String {
        format!("{}.sys", bracket(&self.database))
    }
}

/// The target of an object. A missing schema means `dbo`, or the part before the first dot of a `schema.name` name
/// (how the pre-contract driver addressed objects).
pub fn target_of(object: &ObjectRef) -> AppResult<Target> {
    let database = object
        .database
        .clone()
        .filter(|database| !database.is_empty())
        .ok_or_else(|| AppError::new(format!("{} has no database", object.display_name())))?;
    let (schema, name) = match object.schema.as_deref().filter(|schema| !schema.is_empty()) {
        Some(schema) => (schema.to_owned(), object.name.clone()),
        None => match object.name.split_once('.') {
            Some((schema, name)) => (schema.to_owned(), name.to_owned()),
            None => (DEFAULT_SCHEMA.to_owned(), object.name.clone()),
        },
    };
    Ok(Target { database, schema, name })
}

pub async fn databases(client: &mut TdsClient) -> AppResult<Vec<String>> {
    let rows = text_rows(
        client,
        "SELECT name FROM sys.databases WHERE state = 0 AND HAS_DBACCESS(name) = 1 ORDER BY name",
        &[],
    )
    .await?;
    Ok(rows.iter().map(|row| row.text(0)).collect())
}

pub struct TableEntry {
    pub schema: String,
    pub name: String,
    pub view: bool,
    pub rows: Option<u64>,
    pub comment: String,
}

fn description_join(sys: &str, major: &str, minor: &str) -> String {
    format!(
        "LEFT JOIN {sys}.extended_properties ep ON ep.class = 1 AND ep.major_id = {major} AND ep.minor_id = {minor} \
         AND ep.name = 'MS_Description'"
    )
}

/// Tables and views of a database, with row counts from `sys.partitions`.
pub async fn tables(client: &mut TdsClient, database: &str) -> AppResult<Vec<TableEntry>> {
    let sys = format!("{}.sys", bracket(database));
    let sql = format!(
        "SELECT s.name, o.name, o.type, \
                (SELECT SUM(p.rows) FROM {sys}.partitions p WHERE p.object_id = o.object_id AND p.index_id IN (0, 1)), \
                CAST(ep.value AS NVARCHAR(4000)) \
         FROM {sys}.objects o JOIN {sys}.schemas s ON s.schema_id = o.schema_id {} \
         WHERE o.type IN ('U', 'V') AND o.is_ms_shipped = 0 ORDER BY s.name, o.name",
        description_join(&sys, "o.object_id", "0")
    );
    let rows = text_rows(client, &sql, &[]).await?;
    Ok(rows
        .iter()
        .map(|row| {
            let view = row.text(2).trim() == "V";
            TableEntry {
                schema: row.text(0),
                name: row.text(1),
                view,
                rows: if view { None } else { row.number(3).map(|rows| rows.max(0) as u64) },
                comment: row.text(4),
            }
        })
        .collect())
}

pub fn database_node(name: &str) -> ObjectNode {
    let object = ObjectRef::new(Some(name), None, name, NodeKind::DATABASE);
    ObjectNode {
        key: NodeKey {
            id: format!("db:{name}"),
            kind: NodeKind::DATABASE,
            object: Some(object.clone()),
        },
        label: name.to_owned(),
        kind: NodeKind::DATABASE,
        icon: "database".into(),
        object: Some(object),
        detail: SYSTEM_DATABASES
            .contains(&name.to_ascii_lowercase().as_str())
            .then(|| "system".to_owned()),
        badges: Vec::new(),
        expandable: true,
        default_view: None,
        actions: vec![
            NodeAction::builtin("newQuery", "New query", BuiltinAction::NewQuery),
            NodeAction::builtin("diagram", "ER diagram", BuiltinAction::Diagram),
            NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
            NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh).separated(),
        ],
    }
}

pub fn table_node(database: &str, table: &TableEntry) -> ObjectNode {
    let kind = if table.view { NodeKind::VIEW } else { NodeKind::TABLE };
    let object = ObjectRef::new(Some(database), Some(&table.schema), &table.name, kind.clone());
    let mut actions = vec![
        NodeAction::view("design", "Design", "design"),
        NodeAction::builtin("copyName", "Copy name", BuiltinAction::CopyName),
    ];
    if !table.view {
        actions.push(
            NodeAction::query("truncate", "Truncate…", "TRUNCATE TABLE {qualified}")
                .danger()
                .separated(),
        );
    }
    ObjectNode {
        key: NodeKey {
            id: format!("db:{database}/schema:{}/{}:{}", table.schema, kind.as_str(), table.name),
            kind: kind.clone(),
            object: Some(object.clone()),
        },
        label: object_name(&table.schema, &table.name),
        icon: if table.view { "view" } else { "table" }.into(),
        object: Some(object),
        detail: table.rows.map(|rows| format!("~{rows} rows")),
        badges: Vec::new(),
        expandable: false,
        default_view: Some("data".into()),
        actions,
        kind,
    }
}

/// The columns of a table with their kinds and hints, its primary key in key order, and its rowversion column.
pub struct TableColumns {
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Vec<String>,
    pub version_column: Option<String>,
}

fn columns_sql(sys: &str) -> String {
    format!(
        "SELECT c.name, COALESCE(bt.name, t.name), c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, \
                c.is_computed, dc.definition, cc.definition, CAST(ep.value AS NVARCHAR(4000)), \
                CAST(ic.seed_value AS NVARCHAR(40)), CAST(ic.increment_value AS NVARCHAR(40)) \
         FROM {sys}.columns c \
         JOIN {sys}.types t ON t.user_type_id = c.user_type_id \
         LEFT JOIN {sys}.types bt ON bt.user_type_id = c.system_type_id \
         LEFT JOIN {sys}.default_constraints dc ON dc.object_id = c.default_object_id \
         LEFT JOIN {sys}.computed_columns cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id \
         LEFT JOIN {sys}.identity_columns ic ON ic.object_id = c.object_id AND ic.column_id = c.column_id {} \
         WHERE c.object_id = OBJECT_ID(@P1) ORDER BY c.column_id; \
         SELECT c.name FROM {sys}.indexes i \
         JOIN {sys}.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
         JOIN {sys}.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
         WHERE i.object_id = OBJECT_ID(@P1) AND i.is_primary_key = 1 ORDER BY ic.key_ordinal",
        description_join(sys, "c.object_id", "c.column_id")
    )
}

fn column_of(row: &TextRow, primary: &[String]) -> ColumnInfo {
    let name = row.text(0);
    let base = row.text(1).to_ascii_lowercase();
    let label = type_label(
        &base,
        row.number(2).unwrap_or(0),
        row.number(3).unwrap_or(0),
        row.number(4).unwrap_or(0),
    );
    let mut column = ColumnInfo::result(&name, &label, kind_of(&base));
    column.nullable = row.flag(5);
    let identity = row.flag(6);
    let computed = row.flag(7);
    column.default = row.opt(8);
    column.generated = row.opt(9).filter(|_| computed);
    column.comment = row.opt(10).filter(|comment| !comment.is_empty());
    column.identity = identity.then(|| {
        format!(
            "IDENTITY({},{})",
            row.opt(11).unwrap_or_else(|| "1".into()),
            row.opt(12).unwrap_or_else(|| "1".into())
        )
    });
    let rowversion = base == "timestamp";
    column.hints = ColumnHints {
        primary: primary.contains(&name),
        binary: column.kind == ValueKind::Bytes,
        vector_dims: None,
        // Identity, computed and rowversion columns cannot be written (identity needs IDENTITY_INSERT).
        read_only: identity || computed || rowversion,
    };
    column
}

fn table_columns_of(sets: &[Vec<TextRow>], target: &Target) -> AppResult<TableColumns> {
    let rows = sets.first().map(Vec::as_slice).unwrap_or_default();
    if rows.is_empty() {
        return Err(AppError::new(format!(
            "{} does not exist or has no columns",
            object_name(&target.schema, &target.name)
        )));
    }
    let primary_key: Vec<String> = sets
        .get(1)
        .map(|set| set.iter().map(|row| row.text(0)).collect())
        .unwrap_or_default();
    let columns: Vec<ColumnInfo> = rows.iter().map(|row| column_of(row, &primary_key)).collect();
    let version_column = columns
        .iter()
        .find(|column| column.type_name == "rowversion")
        .map(|column| column.name.clone());
    Ok(TableColumns {
        columns,
        primary_key,
        version_column,
    })
}

pub async fn table_columns(client: &mut TdsClient, target: &Target) -> AppResult<TableColumns> {
    let id = target.qualified();
    let sets = text_sets(client, &columns_sql(&target.sys()), &[&id]).await?;
    table_columns_of(&sets, target)
}

/// One foreign key with its rules, schema-qualified.
struct ForeignKey {
    name: String,
    schema: String,
    table: String,
    columns: Vec<String>,
    ref_schema: String,
    ref_table: String,
    ref_columns: Vec<String>,
    on_update: Option<String>,
    on_delete: Option<String>,
}

fn foreign_keys_sql(sys: &str, filter: &str) -> String {
    format!(
        "SELECT fk.name, sp.name, tp.name, cp.name, sr.name, tr.name, cr.name, \
                fk.update_referential_action_desc, fk.delete_referential_action_desc \
         FROM {sys}.foreign_keys fk \
         JOIN {sys}.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
         JOIN {sys}.tables tp ON tp.object_id = fk.parent_object_id \
         JOIN {sys}.schemas sp ON sp.schema_id = tp.schema_id \
         JOIN {sys}.columns cp ON cp.object_id = fkc.parent_object_id AND cp.column_id = fkc.parent_column_id \
         JOIN {sys}.tables tr ON tr.object_id = fk.referenced_object_id \
         JOIN {sys}.schemas sr ON sr.schema_id = tr.schema_id \
         JOIN {sys}.columns cr ON cr.object_id = fkc.referenced_object_id AND cr.column_id = fkc.referenced_column_id \
         {filter} ORDER BY sp.name, tp.name, fk.name, fkc.constraint_column_id"
    )
}

/// `NO_ACTION` → `NO ACTION`.
fn rule(text: Option<String>) -> Option<String> {
    text.map(|text| text.replace('_', " "))
}

/// Rows of (constraint, schema, table, column, referenced schema, table, column, update rule, delete rule) ordered by
/// constraint, folded into one key per constraint.
fn group_keys(rows: &[TextRow]) -> Vec<ForeignKey> {
    let mut keys: Vec<ForeignKey> = Vec::new();
    for row in rows {
        let (name, schema, table) = (row.text(0), row.text(1), row.text(2));
        match keys.last_mut() {
            Some(last) if last.name == name && last.schema == schema && last.table == table => {
                last.columns.push(row.text(3));
                last.ref_columns.push(row.text(6));
            }
            _ => keys.push(ForeignKey {
                name,
                schema,
                table,
                columns: vec![row.text(3)],
                ref_schema: row.text(4),
                ref_table: row.text(5),
                ref_columns: vec![row.text(6)],
                on_update: rule(row.opt(7)),
                on_delete: rule(row.opt(8)),
            }),
        }
    }
    keys
}

fn indexes_of(rows: &[TextRow]) -> Vec<IndexInfo> {
    let mut indexes: Vec<IndexInfo> = Vec::new();
    for row in rows {
        let name = row.text(0);
        match indexes.last_mut() {
            Some(last) if last.name == name => last.columns.push(row.text(5)),
            _ => indexes.push(IndexInfo {
                primary: row.flag(1),
                unique: row.flag(2),
                columns: vec![row.text(5)],
                method: row.opt(4).map(|method| method.to_ascii_lowercase()),
                definition: row.opt(6).map(|filter| format!("WHERE {filter}")),
                name,
            }),
        }
    }
    indexes
}

pub async fn describe(client: &mut TdsClient, object: &ObjectRef) -> AppResult<ObjectDescription> {
    let target = target_of(object)?;
    let sys = target.sys();
    let sql = format!(
        "{}; \
         SELECT o.type, (SELECT SUM(p.rows) FROM {sys}.partitions p WHERE p.object_id = o.object_id AND p.index_id IN (0, 1)), \
                CAST(ep.value AS NVARCHAR(4000)) \
         FROM {sys}.objects o {} WHERE o.object_id = OBJECT_ID(@P1); \
         SELECT i.name, i.is_primary_key, i.is_unique, i.is_unique_constraint, i.type_desc, c.name, i.filter_definition \
         FROM {sys}.indexes i \
         JOIN {sys}.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
         JOIN {sys}.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
         WHERE i.object_id = OBJECT_ID(@P1) AND i.index_id > 0 AND ic.is_included_column = 0 \
         ORDER BY i.is_primary_key DESC, i.name, ic.key_ordinal; \
         {}; \
         SELECT m.definition FROM {sys}.sql_modules m WHERE m.object_id = OBJECT_ID(@P1)",
        columns_sql(&sys),
        description_join(&sys, "o.object_id", "0"),
        foreign_keys_sql(
            &sys,
            "WHERE fk.parent_object_id = OBJECT_ID(@P1) OR fk.referenced_object_id = OBJECT_ID(@P1)"
        ),
    );
    let id = target.qualified();
    let sets = text_sets(client, &sql, &[&id]).await?;
    let set = |index: usize| sets.get(index).map(Vec::as_slice).unwrap_or_default();
    let TableColumns { columns, primary_key, .. } = table_columns_of(&sets, &target)?;
    let mut description = ObjectDescription::new(ObjectRef {
        schema: Some(target.schema.clone()),
        name: target.name.clone(),
        ..object.clone()
    });
    let mut view = false;
    if let Some(row) = set(2).first() {
        view = row.text(0).trim() == "V";
        if view {
            description.object.kind = NodeKind::VIEW;
        } else {
            description.row_estimate = row.number(1);
        }
        description.comment = row.opt(2).filter(|comment| !comment.is_empty());
    }
    description.extra.push(("Schema".into(), target.schema.clone()));
    let indexes = indexes_of(set(3));
    let primary_name = indexes.iter().find(|index| index.primary).map(|index| index.name.clone());
    description.unique_keys = indexes
        .iter()
        .filter(|index| index.unique && !index.primary && index.definition.is_none())
        .map(|index| KeyInfo {
            name: Some(index.name.clone()),
            columns: index.columns.clone(),
        })
        .collect();
    let unique_constraints: HashSet<String> = set(3).iter().filter(|row| row.flag(3)).map(|row| row.text(0)).collect();
    let keys = group_keys(set(4));
    for key in &keys {
        if key.schema == target.schema && key.table == target.name {
            description.foreign_keys.push(ForeignKeyInfo {
                name: Some(key.name.clone()),
                columns: key.columns.clone(),
                ref_database: None,
                ref_schema: Some(key.ref_schema.clone()),
                ref_table: key.ref_table.clone(),
                ref_columns: key.ref_columns.clone(),
                on_update: key.on_update.clone(),
                on_delete: key.on_delete.clone(),
            });
        }
        if key.ref_schema == target.schema && key.ref_table == target.name {
            description.referenced_by.push(ReferencingKey {
                name: Some(key.name.clone()),
                database: Some(target.database.clone()),
                schema: Some(key.schema.clone()),
                table: key.table.clone(),
                columns: key.columns.clone(),
                ref_columns: key.ref_columns.clone(),
            });
        }
    }
    description.ddl = if view {
        set(5).first().and_then(|row| row.opt(0))
    } else {
        Some(create_table(
            &target,
            &columns,
            primary_name.as_deref(),
            &primary_key,
            &indexes,
            &unique_constraints,
            &description.foreign_keys,
        ))
    };
    description.primary_key = (!primary_key.is_empty()).then_some(KeyInfo {
        name: primary_name,
        columns: primary_key,
    });
    description.indexes = indexes;
    description.columns = columns;
    Ok(description)
}

fn column_list(columns: &[String]) -> String {
    columns.iter().map(|column| bracket(column)).collect::<Vec<_>>().join(", ")
}

/// SQL Server has no SHOW CREATE TABLE: the statement is built from the catalog (columns, keys, foreign keys, indexes).
fn create_table(
    target: &Target,
    columns: &[ColumnInfo],
    primary_name: Option<&str>,
    primary_key: &[String],
    indexes: &[IndexInfo],
    unique_constraints: &HashSet<String>,
    foreign_keys: &[ForeignKeyInfo],
) -> String {
    let mut lines: Vec<String> = columns
        .iter()
        .map(|column| {
            if let Some(expression) = &column.generated {
                return format!("  {} AS {expression}", bracket(&column.name));
            }
            let mut line = format!("  {} {}", bracket(&column.name), column.type_name);
            if let Some(identity) = &column.identity {
                line.push(' ');
                line.push_str(identity);
            }
            line.push_str(if column.nullable { " NULL" } else { " NOT NULL" });
            if let Some(default) = &column.default {
                line.push_str(&format!(" DEFAULT {default}"));
            }
            line
        })
        .collect();
    if !primary_key.is_empty() {
        let name = primary_name
            .map(|name| format!("CONSTRAINT {} ", bracket(name)))
            .unwrap_or_default();
        lines.push(format!("  {name}PRIMARY KEY ({})", column_list(primary_key)));
    }
    for index in indexes.iter().filter(|index| unique_constraints.contains(&index.name)) {
        lines.push(format!(
            "  CONSTRAINT {} UNIQUE ({})",
            bracket(&index.name),
            column_list(&index.columns)
        ));
    }
    for key in foreign_keys {
        let mut line = format!(
            "  CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {}.{} ({})",
            bracket(key.name.as_deref().unwrap_or_default()),
            column_list(&key.columns),
            bracket(key.ref_schema.as_deref().unwrap_or(DEFAULT_SCHEMA)),
            bracket(&key.ref_table),
            column_list(&key.ref_columns)
        );
        for (clause, rule) in [("ON DELETE", &key.on_delete), ("ON UPDATE", &key.on_update)] {
            if let Some(rule) = rule.as_deref().filter(|rule| *rule != "NO ACTION") {
                line.push_str(&format!(" {clause} {rule}"));
            }
        }
        lines.push(line);
    }
    let table = format!("{}.{}", bracket(&target.schema), bracket(&target.name));
    let mut ddl = format!("CREATE TABLE {table} (\n{}\n);", lines.join(",\n"));
    for index in indexes
        .iter()
        .filter(|index| !index.primary && !unique_constraints.contains(&index.name))
    {
        let unique = if index.unique { "UNIQUE " } else { "" };
        let method = match index.method.as_deref() {
            Some("clustered") => "CLUSTERED ",
            _ => "",
        };
        let filter = index.definition.as_deref().map(|filter| format!(" {filter}")).unwrap_or_default();
        ddl.push_str(&format!(
            "\nCREATE {unique}{method}INDEX {} ON {table} ({}){filter};",
            bracket(&index.name),
            column_list(&index.columns)
        ));
    }
    ddl
}

/// The select-list expression of a column: CLR types are read as text.
pub fn select_expr(column: &ColumnInfo) -> String {
    let base = column.type_name.split('(').next().unwrap_or_default();
    if is_clr_type(base) {
        format!("{0}.ToString() AS {0}", bracket(&column.name))
    } else {
        bracket(&column.name)
    }
}

/// The statistics' row count of one table (`None` for views).
pub async fn table_estimate(client: &mut TdsClient, target: &Target) -> AppResult<Option<u64>> {
    let sql = format!(
        "SELECT SUM(p.rows) FROM {}.partitions p WHERE p.object_id = OBJECT_ID(@P1) AND p.index_id IN (0, 1)",
        target.sys()
    );
    let id = target.qualified();
    let rows = text_rows(client, &sql, &[&id]).await?;
    Ok(rows.first().and_then(|row| row.number(0)).map(|rows| rows.max(0) as u64))
}

/// The ER diagram of one database: tables, columns, primary keys and foreign keys in one batch. Table names leave out
/// the default schema, as in the tree.
pub async fn schema_graph(client: &mut TdsClient, database: &str) -> AppResult<SchemaGraph> {
    let listed = tables(client, database).await?;
    let sys = format!("{}.sys", bracket(database));
    let sql = format!(
        "SELECT s.name, o.name, c.name, COALESCE(bt.name, t.name), c.max_length, c.precision, c.scale, c.is_nullable \
         FROM {sys}.columns c \
         JOIN {sys}.objects o ON o.object_id = c.object_id AND o.type IN ('U', 'V') AND o.is_ms_shipped = 0 \
         JOIN {sys}.schemas s ON s.schema_id = o.schema_id \
         JOIN {sys}.types t ON t.user_type_id = c.user_type_id \
         LEFT JOIN {sys}.types bt ON bt.user_type_id = c.system_type_id \
         ORDER BY s.name, o.name, c.column_id; \
         SELECT s.name, o.name, c.name FROM {sys}.indexes i \
         JOIN {sys}.objects o ON o.object_id = i.object_id \
         JOIN {sys}.schemas s ON s.schema_id = o.schema_id \
         JOIN {sys}.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
         JOIN {sys}.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
         WHERE i.is_primary_key = 1; \
         {}",
        foreign_keys_sql(&sys, "")
    );
    let sets = text_sets(client, &sql, &[]).await?;
    let set = |index: usize| sets.get(index).map(Vec::as_slice).unwrap_or_default();
    let keys: HashSet<(String, String, String)> = set(1).iter().map(|row| (row.text(0), row.text(1), row.text(2))).collect();
    let mut columns: HashMap<String, Vec<SchemaColumn>> = HashMap::new();
    for row in set(0) {
        let (schema, table, name) = (row.text(0), row.text(1), row.text(2));
        let primary = keys.contains(&(schema.clone(), table.clone(), name.clone()));
        columns.entry(object_name(&schema, &table)).or_default().push(SchemaColumn {
            column_type: type_label(
                &row.text(3),
                row.number(4).unwrap_or(0),
                row.number(5).unwrap_or(0),
                row.number(6).unwrap_or(0),
            ),
            nullable: row.flag(7),
            primary,
            name,
        });
    }
    let relations = group_keys(set(2))
        .into_iter()
        .map(|key| Relation {
            constraint: key.name,
            database: database.to_owned(),
            table: object_name(&key.schema, &key.table),
            columns: key.columns,
            referenced_database: database.to_owned(),
            referenced_table: object_name(&key.ref_schema, &key.ref_table),
            referenced_columns: key.ref_columns,
        })
        .collect();
    let total_tables = listed.len();
    let tables = listed
        .into_iter()
        .map(|table| {
            let name = object_name(&table.schema, &table.name);
            SchemaTable {
                columns: columns.remove(&name).unwrap_or_default(),
                kind: if table.view { "view" } else { "table" }.to_owned(),
                name,
                rows: table.rows,
                comment: table.comment,
            }
        })
        .collect();
    Ok(SchemaGraph::assemble(database, tables, total_tables, relations))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(values: &[&str]) -> TextRow {
        TextRow(values.iter().map(|value| Some((*value).to_owned())).collect())
    }

    #[test]
    fn objects_resolve_to_database_schema_and_name() {
        let explicit = ObjectRef::new(Some("ERP"), Some("sales"), "Invoices", NodeKind::TABLE);
        let target = target_of(&explicit).unwrap();
        assert_eq!(target.qualified(), "[ERP].[sales].[Invoices]");
        let dotted = ObjectRef::new(Some("ERP"), None, "sales.Invoices", NodeKind::TABLE);
        assert_eq!(target_of(&dotted).unwrap(), target);
        let plain = ObjectRef::new(Some("ERP"), None, "Warehouses", NodeKind::TABLE);
        assert_eq!(target_of(&plain).unwrap().schema, "dbo");
        assert!(target_of(&ObjectRef::new(None, None, "t", NodeKind::TABLE)).is_err());
        assert_eq!(bracket("a]b"), "[a]]b]");
    }

    #[test]
    fn tree_nodes_have_stable_ids_and_schema_labels() {
        assert_eq!(database_node("msdb").detail.as_deref(), Some("system"));
        assert!(database_node("ERP").detail.is_none());
        let entry = TableEntry {
            schema: "sales".into(),
            name: "Invoices".into(),
            view: false,
            rows: Some(12),
            comment: String::new(),
        };
        let node = table_node("ERP", &entry);
        assert_eq!(node.key.id, "db:ERP/schema:sales/table:Invoices");
        assert_eq!(node.label, "sales.Invoices");
        let object = node.object.unwrap();
        assert_eq!((object.schema.as_deref(), object.name.as_str()), (Some("sales"), "Invoices"));
        assert_eq!(node.detail.as_deref(), Some("~12 rows"));
        assert!(node.actions.iter().any(|action| action.id == "truncate"));
        let dbo = table_node(
            "ERP",
            &TableEntry {
                schema: "dbo".into(),
                view: true,
                ..entry
            },
        );
        assert_eq!(dbo.label, "Invoices");
        assert_eq!(dbo.key.id, "db:ERP/schema:dbo/view:Invoices");
        assert!(dbo.actions.iter().all(|action| action.id != "truncate"));
    }

    #[test]
    fn catalog_rows_become_columns_with_hints() {
        let primary = vec!["id".to_owned()];
        let id = column_of(
            &text(&["id", "int", "4", "10", "0", "false", "true", "false", "", "", "", "1", "1"]),
            &primary,
        );
        assert_eq!(id.type_name, "int");
        assert!(id.hints.primary && id.hints.read_only && !id.nullable);
        assert_eq!(id.identity.as_deref(), Some("IDENTITY(1,1)"));
        let mut rv_row = text(&["rv", "timestamp", "8", "0", "0", "false", "false", "false"]);
        rv_row.0.extend([None, None, None, None, None]);
        let rv = column_of(&rv_row, &primary);
        assert_eq!((rv.type_name.as_str(), &rv.kind), ("rowversion", &ValueKind::Bytes));
        assert!(rv.hints.read_only && rv.hints.binary);
        let mut name_row = text(&["name", "nvarchar", "200", "0", "0", "true", "false", "false"]);
        name_row
            .0
            .extend([Some("(N'x')".into()), None, Some("The name".into()), None, None]);
        let name = column_of(&name_row, &primary);
        assert_eq!(name.type_name, "nvarchar(100)");
        assert_eq!(name.default.as_deref(), Some("(N'x')"));
        assert_eq!(name.comment.as_deref(), Some("The name"));
        assert!(name.nullable && !name.hints.read_only);
    }

    #[test]
    fn keys_fold_and_ddl_lists_constraints_and_indexes() {
        let rows = vec![
            text(&["fk", "dbo", "child", "a", "dbo", "parent", "x", "NO_ACTION", "CASCADE"]),
            text(&["fk", "dbo", "child", "b", "dbo", "parent", "y", "NO_ACTION", "CASCADE"]),
        ];
        let keys = group_keys(&rows);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].columns, ["a", "b"]);
        assert_eq!(keys[0].on_update.as_deref(), Some("NO ACTION"));
        let target = Target {
            database: "db".into(),
            schema: "dbo".into(),
            name: "child".into(),
        };
        let mut id = ColumnInfo::result("id", "int", ValueKind::Int);
        id.nullable = false;
        id.identity = Some("IDENTITY(1,1)".into());
        let mut total = ColumnInfo::result("total", "int", ValueKind::Int);
        total.generated = Some("([a]+[b])".into());
        let indexes = vec![
            IndexInfo {
                name: "uq_child".into(),
                columns: vec!["a".into()],
                unique: true,
                primary: false,
                method: Some("nonclustered".into()),
                definition: None,
            },
            IndexInfo {
                name: "ix_child_b".into(),
                columns: vec!["b".into()],
                unique: false,
                primary: false,
                method: Some("nonclustered".into()),
                definition: Some("WHERE [b] IS NOT NULL".into()),
            },
        ];
        let fk = ForeignKeyInfo {
            name: Some("fk".into()),
            columns: vec!["a".into()],
            ref_database: None,
            ref_schema: Some("dbo".into()),
            ref_table: "parent".into(),
            ref_columns: vec!["x".into()],
            on_update: Some("NO ACTION".into()),
            on_delete: Some("CASCADE".into()),
        };
        let unique: HashSet<String> = ["uq_child".to_owned()].into();
        let ddl = create_table(&target, &[id, total], Some("pk_child"), &["id".into()], &indexes, &unique, &[fk]);
        assert_eq!(
            ddl,
            "CREATE TABLE [dbo].[child] (\n  [id] int IDENTITY(1,1) NOT NULL,\n  [total] AS ([a]+[b]),\n  \
             CONSTRAINT [pk_child] PRIMARY KEY ([id]),\n  CONSTRAINT [uq_child] UNIQUE ([a]),\n  \
             CONSTRAINT [fk] FOREIGN KEY ([a]) REFERENCES [dbo].[parent] ([x]) ON DELETE CASCADE\n);\n\
             CREATE INDEX [ix_child_b] ON [dbo].[child] ([b]) WHERE [b] IS NOT NULL;"
        );
        let geo = ColumnInfo::result("shape", "geography", ValueKind::Other);
        assert_eq!(select_expr(&geo), "[shape].ToString() AS [shape]");
    }
}
