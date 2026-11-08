//! Everything an ER diagram needs for one database: tables with their
//! columns and the foreign keys between them. SQL engines answer with a few
//! bulk catalog queries ([`Driver::schema_bulk`]); other drivers fall back to
//! the generic per-table [`Driver`] calls. MongoDB has no foreign keys; its
//! columns come from sampled documents and the UI may infer links from field names.

use std::collections::{HashMap, HashSet};

use futures_util::{stream, StreamExt};

use crate::{
    drivers::{relations::Relation, types::TableInfo, Driver},
    error::AppResult,
};

/// Large schemas are capped so the diagram stays usable; the UI says so.
const MAX_TABLES: usize = 400;
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

fn from_bulk(bulk: BulkSchema, database: &str) -> SchemaGraph {
    let BulkSchema {
        tables: listed,
        mut columns,
        relations,
    } = bulk;
    let total_tables = listed.len();
    let shown: Vec<TableInfo> = listed.into_iter().take(MAX_TABLES).collect();
    let mut outgoing: HashMap<String, Vec<Relation>> = HashMap::new();
    for relation in relations {
        if relation.database.is_empty() || relation.database == database {
            outgoing.entry(relation.table.clone()).or_default().push(relation);
        }
    }
    let names: HashSet<&str> = shown.iter().map(|info| info.name.as_str()).collect();
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for info in &shown {
        if info.kind == "view" {
            continue;
        }
        for relation in outgoing.remove(&info.name).unwrap_or_default() {
            keep_relation(relation, database, &names, &mut seen, &mut links);
        }
    }
    let tables = shown
        .iter()
        .map(|info| SchemaTable {
            name: info.name.clone(),
            kind: info.kind.clone(),
            rows: info.rows,
            comment: info.comment.clone(),
            columns: columns.remove(&info.name).unwrap_or_default(),
        })
        .collect();
    SchemaGraph {
        tables,
        relations: links,
        total_tables,
    }
}

fn keep_relation(
    relation: Relation,
    database: &str,
    names: &HashSet<&str>,
    seen: &mut HashSet<(String, String)>,
    links: &mut Vec<Relation>,
) {
    let same_database = relation.referenced_database.is_empty() || relation.referenced_database == database;
    let key = (relation.table.clone(), relation.constraint.clone());
    if same_database && names.contains(relation.referenced_table.as_str()) && seen.insert(key) {
        links.push(relation);
    }
}

/// The generic path: one describe + relations call per table.
pub(crate) async fn schema_graph_per_table(driver: &dyn Driver, database: &str) -> AppResult<SchemaGraph> {
    let listed = driver.list_tables(database).await?;
    let total_tables = listed.len();
    let shown: Vec<_> = listed.into_iter().take(MAX_TABLES).collect();
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

    let names: HashSet<&str> = shown.iter().map(|info| info.name.as_str()).collect();
    let mut seen = HashSet::new();
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
        tables.push(SchemaTable {
            name: info.name.clone(),
            kind: info.kind.clone(),
            rows: info.rows,
            comment: info.comment.clone(),
            columns,
        });
        for relation in links.map(|links| links.outgoing).unwrap_or_default() {
            keep_relation(relation, database, &names, &mut seen, &mut relations);
        }
    }
    Ok(SchemaGraph {
        tables,
        relations,
        total_tables,
    })
}
