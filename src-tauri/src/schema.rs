//! Everything an ER diagram needs for one database, from a pre-contract driver: tables with their columns and the
//! foreign keys between them. SQL drivers answer with a few bulk catalog queries ([`Driver::schema_bulk`]); other
//! drivers fall back to the generic per-table [`Driver`] calls. MongoDB has no foreign keys; its columns come from
//! sampled documents and the UI may infer links from field names. The graph itself is built by
//! [`SchemaGraph::assemble`], which the native engines use too.

use std::collections::HashMap;

use futures_util::{stream, StreamExt};

use crate::{
    contract::schema::MAX_DIAGRAM_TABLES,
    drivers::{relations::Relation, types::TableInfo, Driver},
    error::AppResult,
};

/// Describe calls in flight at once; keeps small servers responsive.
const CONCURRENCY: usize = 8;

pub use crate::contract::schema::{SchemaColumn, SchemaGraph, SchemaTable};

/// A whole database's catalog read in bulk by a driver.
#[derive(Debug, Default)]
pub struct BulkSchema {
    /// Same names and order as `list_tables`.
    pub tables: Vec<TableInfo>,
    /// Columns in ordinal order, keyed by table name as in `tables`.
    pub columns: HashMap<String, Vec<SchemaColumn>>,
    /// Every foreign key defined in the database (grouped per constraint).
    pub relations: Vec<Relation>,
}

pub async fn schema_graph(driver: &dyn Driver, database: &str) -> AppResult<SchemaGraph> {
    match driver.schema_bulk(database).await? {
        Some(bulk) => Ok(from_bulk(bulk, database)),
        None => schema_graph_per_table(driver, database).await,
    }
}

fn schema_table(info: &TableInfo, columns: Vec<SchemaColumn>) -> SchemaTable {
    SchemaTable {
        name: info.name.clone(),
        kind: info.kind.clone(),
        rows: info.rows,
        comment: info.comment.clone(),
        columns,
    }
}

fn from_bulk(bulk: BulkSchema, database: &str) -> SchemaGraph {
    let BulkSchema {
        tables: listed,
        mut columns,
        relations,
    } = bulk;
    let total_tables = listed.len();
    let tables = listed
        .iter()
        .take(MAX_DIAGRAM_TABLES)
        .map(|info| schema_table(info, columns.remove(&info.name).unwrap_or_default()))
        .collect();
    SchemaGraph::assemble(database, tables, total_tables, relations)
}

/// The generic path: one describe + relations call per table.
pub(crate) async fn schema_graph_per_table(driver: &dyn Driver, database: &str) -> AppResult<SchemaGraph> {
    let listed = driver.list_tables(database).await?;
    let total_tables = listed.len();
    let shown: Vec<_> = listed.into_iter().take(MAX_DIAGRAM_TABLES).collect();
    // Owned (name, is_view) pairs: a closure over borrowed items trips the Send bound on Tauri commands.
    let work: Vec<(String, bool)> = shown.iter().map(|info| (info.name.clone(), info.kind == "view")).collect();
    let described: Vec<_> = stream::iter(work)
        .map(|(name, view)| async move {
            let design = driver.describe_table(database, &name).await;
            let relations = if view {
                Ok(Default::default())
            } else {
                driver.table_relations(database, &name).await
            };
            (design, relations)
        })
        .buffered(CONCURRENCY)
        .collect()
        .await;

    let mut tables = Vec::with_capacity(shown.len());
    let mut relations = Vec::new();
    for (info, (design, links)) in shown.iter().zip(described) {
        // A table that cannot be described (permissions, a broken view) still shows, without columns.
        let columns = design
            .map(|design| {
                design
                    .columns
                    .into_iter()
                    .map(|column| SchemaColumn {
                        primary: column.key.eq_ignore_ascii_case("PRI"),
                        name: column.name,
                        column_type: column.column_type,
                        nullable: column.nullable,
                    })
                    .collect()
            })
            .unwrap_or_default();
        tables.push(schema_table(info, columns));
        relations.extend(links.map(|links| links.outgoing).unwrap_or_default());
    }
    Ok(SchemaGraph::assemble(database, tables, total_tables, relations))
}
