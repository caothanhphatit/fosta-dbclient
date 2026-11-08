//! What the ER diagram needs for one database: tables with their columns and the foreign keys between them.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaColumn {
    pub name: String,
    pub column_type: String,
    pub nullable: bool,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaTable {
    pub name: String,
    pub kind: String,
    pub rows: Option<u64>,
    pub comment: String,
    pub columns: Vec<SchemaColumn>,
}

/// One foreign key, possibly over several columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub constraint: String,
    pub database: String,
    pub table: String,
    pub columns: Vec<String>,
    pub referenced_database: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaGraph {
    pub tables: Vec<SchemaTable>,
    /// Foreign keys whose both ends are in `tables`.
    pub relations: Vec<Relation>,
    /// Total tables in the database; more than `tables.len()` when capped.
    pub total_tables: usize,
}

/// Large schemas are capped so the diagram stays usable; the UI says so.
pub const MAX_DIAGRAM_TABLES: usize = 400;

impl SchemaGraph {
    /// The graph of one database from its tables (in listing order) and the foreign keys read for them: the first
    /// [`MAX_DIAGRAM_TABLES`] tables and the keys between two of those (views have none). Keys to other databases and
    /// duplicates are dropped.
    pub fn assemble(database: &str, tables: Vec<SchemaTable>, total_tables: usize, relations: Vec<Relation>) -> Self {
        let tables: Vec<SchemaTable> = tables.into_iter().take(MAX_DIAGRAM_TABLES).collect();
        let mut outgoing: HashMap<String, Vec<Relation>> = HashMap::new();
        for relation in relations {
            if relation.database.is_empty() || relation.database == database {
                outgoing.entry(relation.table.clone()).or_default().push(relation);
            }
        }
        let names: HashSet<&str> = tables.iter().map(|table| table.name.as_str()).collect();
        let mut seen = HashSet::new();
        let mut links = Vec::new();
        for table in tables.iter().filter(|table| table.kind != "view") {
            for relation in outgoing.remove(&table.name).unwrap_or_default() {
                let same_database = relation.referenced_database.is_empty() || relation.referenced_database == database;
                let key = (relation.table.clone(), relation.constraint.clone());
                if same_database && names.contains(relation.referenced_table.as_str()) && seen.insert(key) {
                    links.push(relation);
                }
            }
        }
        Self {
            tables,
            relations: links,
            total_tables,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str, kind: &str) -> SchemaTable {
        SchemaTable {
            name: name.into(),
            kind: kind.into(),
            rows: None,
            comment: String::new(),
            columns: Vec::new(),
        }
    }

    fn relation(table: &str, referenced_database: &str, referenced: &str) -> Relation {
        Relation {
            constraint: format!("fk_{table}_{referenced}"),
            database: "shop".into(),
            table: table.into(),
            columns: vec!["x".into()],
            referenced_database: referenced_database.into(),
            referenced_table: referenced.into(),
            referenced_columns: vec!["id".into()],
        }
    }

    #[test]
    fn graphs_keep_keys_between_shown_tables_of_the_database() {
        let graph = SchemaGraph::assemble(
            "shop",
            vec![table("orders", "table"), table("customers", "table"), table("v", "view")],
            3,
            vec![
                relation("orders", "shop", "customers"),
                relation("orders", "shop", "customers"),
                relation("orders", "crm", "customers"),
                relation("orders", "shop", "missing"),
                relation("v", "shop", "orders"),
            ],
        );
        assert_eq!(graph.relations.len(), 1);
        assert_eq!(graph.relations[0].referenced_table, "customers");
        assert_eq!(graph.total_tables, 3);
    }
}
