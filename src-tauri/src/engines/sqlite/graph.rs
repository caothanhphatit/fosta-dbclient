//! Relations: the foreign keys that point at one table, and the ER diagram of an attached database. SQLite keeps
//! foreign keys only on the child side (`pragma_foreign_key_list`), so both walk every table of the database.

use rusqlite::Connection;

use super::{map_err, SqliteDialect};
use crate::{
    contract::{
        describe::ReferencingKey,
        schema::{Relation, SchemaColumn, SchemaGraph, SchemaTable},
    },
    engines::common::{sqlbuild::SqlDialect, Result},
};

/// One foreign key of a child table.
struct ForeignKey {
    table: String,
    id: i64,
    parent: String,
    columns: Vec<String>,
    /// Empty entries when the key refers to the parent's primary key without naming its columns.
    parent_columns: Vec<String>,
}

/// Tables and views of `database` (without SQLite's own `sqlite_*` tables), with their type.
fn tables(c: &Connection, database: &str) -> Result<Vec<(String, String)>> {
    let sql = format!(
        "SELECT name, type FROM {}.sqlite_schema WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        SqliteDialect.quote_ident(database)
    );
    let mut statement = c.prepare(&sql).map_err(map_err)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(map_err)?;
    rows.collect::<std::result::Result<_, _>>().map_err(map_err)
}

fn foreign_keys(c: &Connection, database: &str, tables: &[(String, String)]) -> Result<Vec<ForeignKey>> {
    let mut keys = Vec::new();
    let mut statement = c
        .prepare("SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1, ?2) ORDER BY id, seq")
        .map_err(map_err)?;
    for (table, _) in tables.iter().filter(|(_, kind)| kind == "table") {
        let rows = statement
            .query_map([table.as_str(), database], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(map_err)?;
        let mut last = None;
        for row in rows {
            let (id, parent, from, to) = row.map_err(map_err)?;
            if last != Some(id) {
                keys.push(ForeignKey {
                    table: table.clone(),
                    id,
                    parent,
                    columns: Vec::new(),
                    parent_columns: Vec::new(),
                });
                last = Some(id);
            }
            let key = keys.last_mut().expect("pushed above");
            key.columns.push(from);
            key.parent_columns.push(to.unwrap_or_default());
        }
    }
    Ok(keys)
}

/// The primary key columns of a table in key order.
fn primary_key(c: &Connection, database: &str, table: &str) -> Result<Vec<String>> {
    let mut statement = c
        .prepare("SELECT name FROM pragma_table_info(?1, ?2) WHERE pk > 0 ORDER BY pk")
        .map_err(map_err)?;
    let rows = statement
        .query_map([table, database], |row| row.get::<_, String>(0))
        .map_err(map_err)?;
    rows.collect::<std::result::Result<_, _>>().map_err(map_err)
}

fn constraint_name(key: &ForeignKey) -> String {
    format!("{}_fk{}", key.table, key.id + 1)
}

/// The foreign keys of other tables of `database` that reference `table`.
pub fn referencing(c: &Connection, database: &str, table: &str) -> Result<Vec<ReferencingKey>> {
    let listed = tables(c, database)?;
    let mut found = Vec::new();
    for key in foreign_keys(c, database, &listed)?
        .into_iter()
        .filter(|key| key.parent.eq_ignore_ascii_case(table))
    {
        let mut ref_columns = key.parent_columns.clone();
        if ref_columns.iter().any(String::is_empty) {
            ref_columns = primary_key(c, database, &key.parent)?;
        }
        found.push(ReferencingKey {
            name: Some(constraint_name(&key)),
            database: Some(database.to_owned()),
            schema: None,
            table: key.table.clone(),
            columns: key.columns.clone(),
            ref_columns,
        });
    }
    Ok(found)
}

/// Tables, columns and foreign keys of `database` for the diagram.
pub fn schema_graph(c: &Connection, database: &str) -> Result<SchemaGraph> {
    let listed = tables(c, database)?;
    let mut columns_statement = c
        .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_xinfo(?1, ?2) WHERE hidden IN (0, 2, 3)")
        .map_err(map_err)?;
    let mut tables_out = Vec::new();
    for (name, kind) in &listed {
        let rows = columns_statement
            .query_map([name.as_str(), database], |row| {
                Ok(SchemaColumn {
                    name: row.get(0)?,
                    column_type: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    nullable: row.get::<_, i64>(2)? == 0,
                    primary: row.get::<_, i64>(3)? > 0,
                })
            })
            .map_err(map_err)?;
        tables_out.push(SchemaTable {
            name: name.clone(),
            kind: kind.clone(),
            rows: None,
            comment: String::new(),
            columns: rows.collect::<std::result::Result<_, _>>().map_err(map_err)?,
        });
    }
    let mut relations = Vec::new();
    for key in foreign_keys(c, database, &listed)? {
        let mut parent_columns = key.parent_columns.clone();
        if parent_columns.iter().any(String::is_empty) {
            parent_columns = primary_key(c, database, &key.parent)?;
        }
        relations.push(Relation {
            constraint: constraint_name(&key),
            database: database.to_owned(),
            table: key.table.clone(),
            columns: key.columns.clone(),
            referenced_database: database.to_owned(),
            referenced_table: key.parent.clone(),
            referenced_columns: parent_columns,
        });
    }
    let total = tables_out.len();
    Ok(SchemaGraph::assemble(database, tables_out, total, relations))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE parent (id INTEGER PRIMARY KEY, code TEXT UNIQUE);
             CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent, code TEXT REFERENCES parent (code));
             CREATE VIEW v AS SELECT * FROM parent;",
        )
        .unwrap();
        c
    }

    #[test]
    fn referencing_keys_resolve_the_implicit_primary_key() {
        let c = database();
        let found = referencing(&c, "main", "parent").unwrap();
        assert_eq!(found.len(), 2);
        let by_primary = found.iter().find(|key| key.columns == ["parent_id"]).unwrap();
        assert_eq!(by_primary.ref_columns, ["id"]);
        assert_eq!(by_primary.table, "child");
        let by_code = found.iter().find(|key| key.columns == ["code"]).unwrap();
        assert_eq!(by_code.ref_columns, ["code"]);
        assert!(referencing(&c, "main", "child").unwrap().is_empty());
    }

    #[test]
    fn the_diagram_lists_tables_views_and_relations() {
        let graph = schema_graph(&database(), "main").unwrap();
        let names: Vec<_> = graph
            .tables
            .iter()
            .map(|table| (table.name.as_str(), table.kind.as_str()))
            .collect();
        assert_eq!(names, [("child", "table"), ("parent", "table"), ("v", "view")]);
        assert!(graph.tables[1].columns.iter().any(|column| column.name == "id" && column.primary));
        assert_eq!(graph.relations.len(), 2);
        assert!(graph.relations.iter().all(|relation| relation.referenced_table == "parent"));
    }
}
