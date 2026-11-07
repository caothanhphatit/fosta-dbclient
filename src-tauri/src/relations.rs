use mysql_async::Conn;
use serde::Serialize;

use crate::{
    db::{text_rows, TextRow},
    error::AppResult,
};

#[derive(Clone, Debug, Serialize)]
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

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableRelations {
    pub outgoing: Vec<Relation>,
    pub incoming: Vec<Relation>,
}

pub(crate) async fn table_relations(conn: &mut Conn, database: &str, table: &str) -> AppResult<TableRelations> {
    let rows = text_rows(
        conn,
        "SELECT CONSTRAINT_NAME, TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, \
                REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
         FROM information_schema.KEY_COLUMN_USAGE \
         WHERE REFERENCED_TABLE_NAME IS NOT NULL \
           AND ((TABLE_SCHEMA = ? AND TABLE_NAME = ?) OR (REFERENCED_TABLE_SCHEMA = ? AND REFERENCED_TABLE_NAME = ?)) \
         ORDER BY TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
        (database.to_owned(), table.to_owned(), database.to_owned(), table.to_owned()),
    )
    .await?;
    Ok(split_relations(group_relations(&rows), database, table))
}

/// Sorts grouped foreign keys into those leaving `table` and those pointing at it.
pub(crate) fn split_relations(relations: Vec<Relation>, database: &str, table: &str) -> TableRelations {
    let (mut outgoing, mut incoming) = (Vec::new(), Vec::new());
    for relation in relations {
        let is_outgoing = relation.database == database && relation.table == table;
        let is_incoming = relation.referenced_database == database && relation.referenced_table == table;
        if is_outgoing && is_incoming {
            incoming.push(relation.clone());
        }
        if is_outgoing {
            outgoing.push(relation);
        } else if is_incoming {
            incoming.push(relation);
        }
    }
    TableRelations { outgoing, incoming }
}

/// Rows of (constraint, schema, table, column, referenced schema, referenced table, referenced column),
/// ordered by constraint, folded into one relation per constraint.
pub(crate) fn group_relations(rows: &[TextRow]) -> Vec<Relation> {
    let mut relations: Vec<Relation> = Vec::new();
    for row in rows {
        let identity = (row.text(1), row.text(2), row.text(0));
        match relations.last_mut() {
            Some(last) if (last.database.clone(), last.table.clone(), last.constraint.clone()) == identity => {
                last.columns.push(row.text(3));
                last.referenced_columns.push(row.text(6));
            }
            _ => relations.push(Relation {
                constraint: identity.2,
                database: identity.0,
                table: identity.1,
                columns: vec![row.text(3)],
                referenced_database: row.text(4),
                referenced_table: row.text(5),
                referenced_columns: vec![row.text(6)],
            }),
        }
    }
    relations
}
