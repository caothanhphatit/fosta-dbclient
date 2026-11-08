//! Relations: the ER diagram of one schema (two bulk queries) and the foreign keys that point at one table.

use std::collections::HashMap;

use oracle::Connection;

use super::{strings, tabular::app};
use crate::{
    contract::{
        describe::ReferencingKey,
        schema::{Relation, SchemaColumn, SchemaGraph, SchemaTable},
    },
    error::AppResult,
};

fn split(list: &Option<String>) -> Vec<String> {
    list.as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Tables, views and materialized views of `owner` with their columns, and the foreign keys between them. Keys to other
/// schemas are named `SCHEMA.TABLE` and dropped by `SchemaGraph::assemble` (the diagram shows one schema).
pub fn schema_graph(c: &Connection, owner: &str) -> AppResult<SchemaGraph> {
    let rows = strings(
        c,
        "SELECT o.object_name, o.object_type, TO_CHAR(t.num_rows), tc.comments, col.column_name, col.data_type, col.nullable, \
                CASE WHEN EXISTS (SELECT 1 FROM all_constraints k JOIN all_cons_columns kc ON kc.owner = k.owner \
                     AND kc.constraint_name = k.constraint_name WHERE k.owner = o.owner AND k.table_name = o.object_name \
                     AND k.constraint_type = 'P' AND kc.column_name = col.column_name) THEN 'Y' ELSE 'N' END \
         FROM all_objects o \
         JOIN all_tab_columns col ON col.owner = o.owner AND col.table_name = o.object_name \
         LEFT JOIN all_tables t ON t.owner = o.owner AND t.table_name = o.object_name \
         LEFT JOIN all_tab_comments tc ON tc.owner = o.owner AND tc.table_name = o.object_name \
         WHERE o.owner = :1 AND o.object_type IN ('TABLE', 'VIEW', 'MATERIALIZED VIEW') AND o.object_name NOT LIKE 'BIN$%' \
         AND o.generated = 'N' \
         AND NOT (o.object_type = 'TABLE' AND EXISTS (SELECT 1 FROM all_mviews m WHERE m.owner = o.owner AND m.mview_name = o.object_name)) \
         ORDER BY o.object_name, col.column_id",
        &[&owner],
    )
    .map_err(app)?;
    let mut tables: Vec<SchemaTable> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for row in &rows {
        let g = |i: usize| row[i].clone().unwrap_or_default();
        let name = g(0);
        let at = *index.entry(name.clone()).or_insert_with(|| {
            tables.push(SchemaTable {
                name,
                kind: if g(1) == "TABLE" { "table" } else { "view" }.to_owned(),
                rows: row[2].as_deref().and_then(|n| n.parse().ok()),
                comment: g(3),
                columns: Vec::new(),
            });
            tables.len() - 1
        });
        tables[at].columns.push(SchemaColumn {
            name: g(4),
            column_type: g(5),
            nullable: g(6) == "Y",
            primary: g(7) == "Y",
        });
    }
    let relations = strings(
        c,
        "SELECT k.constraint_name, k.table_name, \
                (SELECT LISTAGG(cc.column_name, ',') WITHIN GROUP (ORDER BY cc.position) FROM all_cons_columns cc \
                  WHERE cc.owner = k.owner AND cc.constraint_name = k.constraint_name), \
                r.owner, r.table_name, \
                (SELECT LISTAGG(rc.column_name, ',') WITHIN GROUP (ORDER BY rc.position) FROM all_cons_columns rc \
                  WHERE rc.owner = r.owner AND rc.constraint_name = r.constraint_name) \
         FROM all_constraints k JOIN all_constraints r ON r.owner = k.r_owner AND r.constraint_name = k.r_constraint_name \
         WHERE k.owner = :1 AND k.constraint_type = 'R' ORDER BY k.table_name, k.constraint_name",
        &[&owner],
    )
    .map_err(app)?
    .into_iter()
    .map(|row| {
        let referenced_owner = row[3].clone().unwrap_or_default();
        Relation {
            constraint: row[0].clone().unwrap_or_default(),
            database: owner.to_owned(),
            table: row[1].clone().unwrap_or_default(),
            columns: split(&row[2]),
            referenced_database: referenced_owner,
            referenced_table: row[4].clone().unwrap_or_default(),
            referenced_columns: split(&row[5]),
        }
    })
    .collect();
    let total = tables.len();
    Ok(SchemaGraph::assemble(owner, tables, total, relations))
}

/// The foreign keys of other tables (in any schema the user can see) that reference `owner.table`.
pub fn referencing(c: &Connection, owner: &str, table: &str) -> AppResult<Vec<ReferencingKey>> {
    let rows = strings(
        c,
        "SELECT k.constraint_name, k.owner, k.table_name, \
                (SELECT LISTAGG(cc.column_name, ',') WITHIN GROUP (ORDER BY cc.position) FROM all_cons_columns cc \
                  WHERE cc.owner = k.owner AND cc.constraint_name = k.constraint_name), \
                (SELECT LISTAGG(rc.column_name, ',') WITHIN GROUP (ORDER BY rc.position) FROM all_cons_columns rc \
                  WHERE rc.owner = r.owner AND rc.constraint_name = r.constraint_name) \
         FROM all_constraints k JOIN all_constraints r ON r.owner = k.r_owner AND r.constraint_name = k.r_constraint_name \
         WHERE k.constraint_type = 'R' AND r.owner = :1 AND r.table_name = :2 ORDER BY k.owner, k.table_name, k.constraint_name",
        &[&owner, &table],
    )
    .map_err(app)?;
    Ok(rows
        .into_iter()
        .map(|row| ReferencingKey {
            name: row[0].clone(),
            // The owner is the "database" on the contract side (see nodes.rs).
            database: row[1].clone(),
            schema: None,
            table: row[2].clone().unwrap_or_default(),
            columns: split(&row[3]),
            ref_columns: split(&row[4]),
        })
        .collect())
}
