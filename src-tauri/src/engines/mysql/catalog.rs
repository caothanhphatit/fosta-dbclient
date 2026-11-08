//! The MariaDB/MySQL catalog from `information_schema`: databases and tables for the tree, one object's description,
//! and the whole-database schema graph in three bulk queries.

use std::collections::{HashMap, HashSet};

use mysql_async::{prelude::*, Conn, Params, Row};

use super::values::{kind_of, plain_text, vector_dims};
use crate::{
    contract::{
        BuiltinAction, ColumnHints, ColumnInfo, ForeignKeyInfo, IndexInfo, KeyInfo, NodeAction, NodeKey, NodeKind, ObjectDescription,
        ObjectNode, ObjectRef, ReferencingKey, Relation, SchemaColumn, SchemaGraph, SchemaTable, ValueKind,
    },
    error::{AppError, AppResult},
};

/// Schemas the server keeps for itself; shown dimmed in the tree.
pub const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];
/// Tables reporting 0 rows that the table list checks for data; beyond that a 0 is shown as reported.
const MAX_EMPTY_PROBES: usize = 32;
/// Size of one InnoDB page: what an empty table reports as `DATA_LENGTH`.
const EMPTY_TABLE_BYTES: u64 = 16 * 1024;

/// A catalog row with every value as text.
pub struct TextRow(pub Vec<Option<String>>);

impl TextRow {
    pub fn text(&self, index: usize) -> String {
        self.opt(index).unwrap_or_default()
    }

    pub fn opt(&self, index: usize) -> Option<String> {
        self.0.get(index).cloned().flatten()
    }

    pub fn number(&self, index: usize) -> Option<u64> {
        self.opt(index)?.trim().parse().ok()
    }
}

/// Runs a catalog query as a prepared statement and reads every value as text.
pub async fn text_rows(conn: &mut Conn, sql: &str, params: impl Into<Params>) -> AppResult<Vec<TextRow>> {
    let params: Params = params.into();
    let rows: Vec<Row> = conn.exec(sql, params).await?;
    Ok(rows
        .into_iter()
        .map(|row| TextRow(row.unwrap().into_iter().map(plain_text).collect()))
        .collect())
}

pub fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

pub fn qualified(database: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(database), quote_ident(table))
}

pub fn database_of(object: &ObjectRef) -> AppResult<&str> {
    object
        .database
        .as_deref()
        .filter(|database| !database.is_empty())
        .ok_or_else(|| AppError::new(format!("{} has no database", object.display_name())))
}

pub async fn databases(conn: &mut Conn) -> AppResult<Vec<String>> {
    Ok(conn.query("SHOW DATABASES").await?)
}

pub struct TableEntry {
    pub name: String,
    pub view: bool,
    pub rows: Option<u64>,
    pub comment: String,
}

/// The tree's row estimate. InnoDB's `TABLE_ROWS` stays 0 until statistics are refreshed (right after a bulk load, for
/// example), so a table that holds data but claims 0 rows reports "unknown" instead.
pub fn row_estimate(table_rows: Option<u64>, data_length: Option<u64>) -> Option<u64> {
    let rows = table_rows?;
    (rows > 0 || data_length.unwrap_or(0) <= EMPTY_TABLE_BYTES).then_some(rows)
}

/// Tables and views of a database with their row estimates.
pub async fn tables(conn: &mut Conn, database: &str) -> AppResult<Vec<TableEntry>> {
    let rows = text_rows(
        conn,
        "SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS, TABLE_COMMENT, DATA_LENGTH FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
        (database.to_owned(),),
    )
    .await?;
    let mut tables: Vec<TableEntry> = rows
        .iter()
        .map(|row| TableEntry {
            name: row.text(0),
            view: row.text(1).contains("VIEW"),
            rows: row_estimate(row.number(2), row.number(4)),
            comment: row.text(3),
        })
        .collect();
    // InnoDB statistics can claim 0 rows long after a bulk load. A cheap EXISTS tells a really empty table from a stale
    // estimate, which then shows as unknown.
    let zero: Vec<usize> = (0..tables.len())
        .filter(|&index| !tables[index].view && tables[index].rows == Some(0))
        .take(MAX_EMPTY_PROBES)
        .collect();
    for index in zero {
        let probe = format!("SELECT EXISTS(SELECT 1 FROM {})", qualified(database, &tables[index].name));
        if let Ok(Some(1_i64)) = conn.query_first::<i64, _>(probe).await {
            tables[index].rows = None;
        }
    }
    Ok(tables)
}

/// The statistics-based row count of one table (`None` for views and stale statistics).
pub async fn table_estimate(conn: &mut Conn, database: &str, table: &str) -> AppResult<Option<u64>> {
    let rows = text_rows(
        conn,
        "SELECT TABLE_ROWS, DATA_LENGTH FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows.first().and_then(|row| row_estimate(row.number(0), row.number(1))))
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
        detail: SYSTEM_SCHEMAS
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
    let object = ObjectRef::new(Some(database), None, &table.name, kind.clone());
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
            id: format!("db:{database}/{}:{}", kind.as_str(), table.name),
            kind: kind.clone(),
            object: Some(object.clone()),
        },
        label: table.name.clone(),
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

/// The columns of a table with their kinds and hints, and its primary key in key order.
pub struct TableColumns {
    pub columns: Vec<ColumnInfo>,
    pub primary_key: Vec<String>,
}

/// MariaDB's JSON is LONGTEXT with a `json_valid` check named after the column.
async fn json_columns(conn: &mut Conn, mariadb: bool, database: &str, table: &str) -> AppResult<HashSet<String>> {
    if !mariadb {
        return Ok(HashSet::new());
    }
    let rows = text_rows(
        conn,
        "SELECT CONSTRAINT_NAME FROM information_schema.CHECK_CONSTRAINTS \
         WHERE CONSTRAINT_SCHEMA = ? AND TABLE_NAME = ? AND CHECK_CLAUSE LIKE 'json_valid(%'",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .unwrap_or_default();
    Ok(rows.iter().map(|row| row.text(0)).collect())
}

/// A default as the catalog reports it; MariaDB spells "no default" on a nullable column as the text `NULL`.
fn default_of(raw: Option<String>, mariadb: bool) -> Option<String> {
    raw.filter(|text| !(mariadb && text == "NULL"))
}

/// The primary key in key order (`COLUMN_KEY` marks the columns but not their order).
async fn primary_key_order(conn: &mut Conn, database: &str, table: &str) -> AppResult<Vec<String>> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND INDEX_NAME = 'PRIMARY' ORDER BY SEQ_IN_INDEX",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(rows.iter().map(|row| row.text(0)).collect())
}

pub async fn table_columns(conn: &mut Conn, mariadb: bool, database: &str, table: &str) -> AppResult<TableColumns> {
    let rows = text_rows(
        conn,
        "SELECT COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT, \
                GENERATION_EXPRESSION, COLUMN_KEY \
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    if rows.is_empty() {
        return Err(AppError::new(format!("{database}.{table} does not exist or has no columns")));
    }
    // MariaDB's JSON columns are LONGTEXT; only then is the check list worth reading.
    let json = if rows.iter().any(|row| row.text(1).eq_ignore_ascii_case("longtext")) {
        json_columns(conn, mariadb, database, table).await?
    } else {
        HashSet::new()
    };
    let mut primary: Vec<String> = rows
        .iter()
        .filter(|row| row.text(8).eq_ignore_ascii_case("PRI"))
        .map(|row| row.text(0))
        .collect();
    if primary.len() > 1 {
        primary = primary_key_order(conn, database, table).await?;
    }
    let columns = rows
        .iter()
        .map(|row| {
            let name = row.text(0);
            let (data_type, column_type) = (row.text(1), row.text(2));
            let extra = row.text(5).to_ascii_lowercase();
            let generated = row.opt(7).filter(|expression| !expression.is_empty());
            let kind = kind_of(
                &data_type,
                &column_type,
                json.contains(&name) || data_type.eq_ignore_ascii_case("json"),
            );
            let mut column = ColumnInfo::result(&name, &column_type, kind);
            column.nullable = row.text(3) == "YES";
            column.default = default_of(row.opt(4), mariadb);
            column.comment = row.opt(6).filter(|comment| !comment.is_empty());
            column.identity = extra.contains("auto_increment").then(|| "auto_increment".to_owned());
            let virtual_column = extra.contains("virtual") || extra.contains("stored") || extra.contains("persistent");
            column.generated = generated.filter(|_| virtual_column);
            column.hints = ColumnHints {
                primary: primary.contains(&name),
                binary: column.kind == ValueKind::Bytes,
                vector_dims: vector_dims(&data_type, &column_type),
                read_only: column.generated.is_some(),
            };
            column
        })
        .collect();
    Ok(TableColumns {
        columns,
        primary_key: primary,
    })
}

/// One foreign key with its rules.
struct ForeignKey {
    relation: Relation,
    on_update: Option<String>,
    on_delete: Option<String>,
}

/// Rows of (constraint, schema, table, column, referenced schema, table, column[, update rule, delete rule]) ordered by
/// constraint, folded into one key per constraint.
fn group_keys(rows: &[TextRow]) -> Vec<ForeignKey> {
    let mut keys: Vec<ForeignKey> = Vec::new();
    for row in rows {
        let identity = (row.text(1), row.text(2), row.text(0));
        match keys.last_mut() {
            Some(last)
                if (
                    last.relation.database.as_str(),
                    last.relation.table.as_str(),
                    last.relation.constraint.as_str(),
                ) == (identity.0.as_str(), identity.1.as_str(), identity.2.as_str()) =>
            {
                last.relation.columns.push(row.text(3));
                last.relation.referenced_columns.push(row.text(6));
            }
            _ => keys.push(ForeignKey {
                relation: Relation {
                    constraint: identity.2,
                    database: identity.0,
                    table: identity.1,
                    columns: vec![row.text(3)],
                    referenced_database: row.text(4),
                    referenced_table: row.text(5),
                    referenced_columns: vec![row.text(6)],
                },
                on_update: row.opt(7),
                on_delete: row.opt(8),
            }),
        }
    }
    keys
}

pub async fn describe(conn: &mut Conn, mariadb: bool, object: &ObjectRef) -> AppResult<ObjectDescription> {
    let database = database_of(object)?;
    let table = object.name.as_str();
    let TableColumns { columns, primary_key } = table_columns(conn, mariadb, database, table).await?;
    let mut description = ObjectDescription::new(object.clone());
    let info = text_rows(
        conn,
        "SELECT TABLE_TYPE, ENGINE, TABLE_ROWS, DATA_LENGTH, TABLE_COLLATION, TABLE_COMMENT FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    if let Some(row) = info.first() {
        if row.text(0).contains("VIEW") {
            description.object.kind = NodeKind::VIEW;
        }
        description.row_estimate = row_estimate(row.number(2), row.number(3)).map(|rows| rows as i64);
        description.comment = row.opt(5).filter(|comment| !comment.is_empty() && comment != "VIEW");
        for (label, value) in [("Engine", row.opt(1)), ("Collation", row.opt(4))] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                description.extra.push((label.to_owned(), value));
            }
        }
    }
    description.columns = columns;
    description.primary_key = (!primary_key.is_empty()).then(|| KeyInfo {
        name: Some("PRIMARY".into()),
        columns: primary_key,
    });

    let index_rows = text_rows(
        conn,
        "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, INDEX_TYPE FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? ORDER BY INDEX_NAME = 'PRIMARY' DESC, INDEX_NAME, SEQ_IN_INDEX",
        (database.to_owned(), table.to_owned()),
    )
    .await?;
    let mut indexes: Vec<IndexInfo> = Vec::new();
    for row in &index_rows {
        let name = row.text(0);
        let column = row.opt(2).unwrap_or_else(|| "(expression)".to_owned());
        match indexes.last_mut() {
            Some(last) if last.name == name => last.columns.push(column),
            _ => indexes.push(IndexInfo {
                primary: name == "PRIMARY",
                unique: row.text(1) == "0",
                columns: vec![column],
                method: row.opt(3).map(|method| method.to_ascii_lowercase()),
                definition: None,
                name,
            }),
        }
    }
    description.unique_keys = indexes
        .iter()
        .filter(|index| index.unique && !index.primary)
        .map(|index| KeyInfo {
            name: Some(index.name.clone()),
            columns: index.columns.clone(),
        })
        .collect();
    description.indexes = indexes;

    let key_rows = text_rows(
        conn,
        "SELECT k.CONSTRAINT_NAME, k.TABLE_SCHEMA, k.TABLE_NAME, k.COLUMN_NAME, \
                k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE \
         FROM information_schema.KEY_COLUMN_USAGE k \
         LEFT JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA \
              AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME \
         WHERE k.REFERENCED_TABLE_NAME IS NOT NULL \
           AND ((k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ?) OR (k.REFERENCED_TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME = ?)) \
         ORDER BY k.TABLE_SCHEMA, k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
        (database.to_owned(), table.to_owned(), database.to_owned(), table.to_owned()),
    )
    .await?;
    for key in group_keys(&key_rows) {
        let relation = key.relation;
        if relation.database == database && relation.table == table {
            description.foreign_keys.push(ForeignKeyInfo {
                name: Some(relation.constraint.clone()),
                columns: relation.columns.clone(),
                ref_database: (relation.referenced_database != database).then(|| relation.referenced_database.clone()),
                ref_schema: None,
                ref_table: relation.referenced_table.clone(),
                ref_columns: relation.referenced_columns.clone(),
                on_update: key.on_update.clone(),
                on_delete: key.on_delete.clone(),
            });
        }
        if relation.referenced_database == database && relation.referenced_table == table {
            description.referenced_by.push(ReferencingKey {
                name: Some(relation.constraint),
                database: Some(relation.database),
                schema: None,
                table: relation.table,
                columns: relation.columns,
                ref_columns: relation.referenced_columns,
            });
        }
    }
    description.ddl = create_statement(conn, database, table).await.ok().filter(|ddl| !ddl.is_empty());
    Ok(description)
}

/// `SHOW CREATE TABLE` (also answers for views).
pub async fn create_statement(conn: &mut Conn, database: &str, table: &str) -> AppResult<String> {
    let row: Option<Row> = conn
        .query_first(format!("SHOW CREATE TABLE {}", qualified(database, table)))
        .await?;
    Ok(row
        .and_then(|row| row.unwrap().into_iter().nth(1))
        .and_then(plain_text)
        .unwrap_or_default())
}

/// The ER diagram of one database: tables, columns and foreign keys in three bulk queries.
pub async fn schema_graph(conn: &mut Conn, database: &str) -> AppResult<SchemaGraph> {
    let listed = tables(conn, database).await?;
    let column_rows = text_rows(
        conn,
        "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY \
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION",
        (database.to_owned(),),
    )
    .await?;
    let mut columns: HashMap<String, Vec<SchemaColumn>> = HashMap::new();
    for row in &column_rows {
        columns.entry(row.text(0)).or_default().push(SchemaColumn {
            name: row.text(1),
            column_type: row.text(2),
            nullable: row.text(3) == "YES",
            primary: row.text(4).eq_ignore_ascii_case("PRI"),
        });
    }
    let relation_rows = text_rows(
        conn,
        "SELECT CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, \
                REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
         FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME IS NOT NULL \
         ORDER BY TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
        (database.to_owned(),),
    )
    .await?;
    let relations = group_keys(&relation_rows).into_iter().map(|key| key.relation).collect();
    let total_tables = listed.len();
    let tables = listed
        .into_iter()
        .map(|table| SchemaTable {
            columns: columns.remove(&table.name).unwrap_or_default(),
            kind: if table.view { "view" } else { "table" }.to_owned(),
            name: table.name,
            rows: table.rows,
            comment: table.comment,
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
    fn stale_zero_row_estimates_are_unknown() {
        assert_eq!(row_estimate(Some(16001), Some(1_589_248)), Some(16001));
        assert_eq!(row_estimate(Some(0), Some(16384)), Some(0), "an empty table");
        assert_eq!(row_estimate(Some(0), Some(1_589_248)), None, "data but no statistics yet");
        assert_eq!(row_estimate(None, None), None, "views");
    }

    #[test]
    fn tree_nodes_have_stable_ids_and_actions() {
        let node = database_node("mysql");
        assert_eq!(node.key.id, "db:mysql");
        assert_eq!(node.detail.as_deref(), Some("system"));
        assert!(database_node("shop").detail.is_none());
        let table = TableEntry {
            name: "orders".into(),
            view: false,
            rows: Some(12),
            comment: String::new(),
        };
        let node = table_node("shop", &table);
        assert_eq!(node.key.id, "db:shop/table:orders");
        assert_eq!(node.detail.as_deref(), Some("~12 rows"));
        assert!(node.actions.iter().any(|action| action.id == "truncate" && action.danger));
        let view = table_node("shop", &TableEntry { view: true, ..table });
        assert_eq!(view.key.id, "db:shop/view:orders");
        assert!(view.actions.iter().all(|action| action.id != "truncate"));
    }

    #[test]
    fn composite_keys_fold_into_one_relation() {
        let rows = vec![
            text(&["fk", "shop", "items", "a", "shop", "orders", "x", "CASCADE", "RESTRICT"]),
            text(&["fk", "shop", "items", "b", "shop", "orders", "y", "CASCADE", "RESTRICT"]),
            text(&["fk2", "shop", "items", "c", "crm", "people", "id", "NO ACTION", "NO ACTION"]),
        ];
        let keys = group_keys(&rows);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].relation.columns, ["a", "b"]);
        assert_eq!(keys[0].on_update.as_deref(), Some("CASCADE"));
        assert_eq!(keys[1].relation.referenced_database, "crm");
        assert_eq!(quote_ident("a`b"), "`a``b`");
    }
}
