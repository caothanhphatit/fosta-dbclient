//! Relations: the ER diagram of a database (two bulk queries over every user schema) and the foreign keys that point at
//! one table.

use std::collections::HashMap;

use super::{catalog::USER_SCHEMAS, map_err, PgEngine};
use crate::{
    contract::{
        describe::ReferencingKey,
        schema::{Relation, SchemaColumn, SchemaGraph, SchemaTable},
    },
    error::{AppError, AppResult},
};

/// `public` tables keep their plain name in the diagram; other schemas are shown as `schema.table`.
fn diagram_name(schema: &str, table: &str) -> String {
    if schema == "public" {
        table.to_owned()
    } else {
        format!("{schema}.{table}")
    }
}

const KEY_COLUMNS: &str = "ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, o) \
                           JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.o)";
const REFERENCED_COLUMNS: &str = "ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY k(n, o) \
                                  JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n ORDER BY k.o)";

impl PgEngine {
    pub(super) async fn graph(&self) -> AppResult<SchemaGraph> {
        let client = self.client.lock().await;
        let database = self.database().to_owned();
        let rows = client
            .query(
                &format!(
                    "SELECT n.nspname::text, c.relname::text, c.relkind::text, c.reltuples::float8, \
                            COALESCE(obj_description(c.oid, 'pg_class'), ''), a.attname::text, format_type(a.atttypid, a.atttypmod), \
                            a.attnotnull, \
                            EXISTS (SELECT 1 FROM pg_index i WHERE i.indrelid = c.oid AND i.indisprimary AND a.attnum = ANY (i.indkey)) \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
                     WHERE c.relkind IN ('r','p','v','m','f') AND NOT c.relispartition AND {USER_SCHEMAS} \
                     ORDER BY 1, 2, a.attnum"
                ),
                &[],
            )
            .await
            .map_err(map_err)?;
        let mut tables: Vec<SchemaTable> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for row in &rows {
            let (schema, table, relkind): (String, String, String) = (row.get(0), row.get(1), row.get(2));
            let name = diagram_name(&schema, &table);
            let at = *index.entry(name.clone()).or_insert_with(|| {
                let tuples: f64 = row.get(3);
                tables.push(SchemaTable {
                    name,
                    kind: if matches!(relkind.as_str(), "v" | "m") { "view" } else { "table" }.to_owned(),
                    rows: (tuples >= 0.0).then_some(tuples as u64),
                    comment: row.get(4),
                    columns: Vec::new(),
                });
                tables.len() - 1
            });
            tables[at].columns.push(SchemaColumn {
                name: row.get(5),
                column_type: row.get(6),
                nullable: !row.get::<_, bool>(7),
                primary: row.get(8),
            });
        }
        let relations = client
            .query(
                &format!(
                    "SELECT con.conname::text, n.nspname::text, c.relname::text, {KEY_COLUMNS}, fn.nspname::text, fc.relname::text, \
                            {REFERENCED_COLUMNS} \
                     FROM pg_constraint con JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_class fc ON fc.oid = con.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                     WHERE con.contype = 'f' AND {USER_SCHEMAS} ORDER BY 2, 3, 1"
                ),
                &[],
            )
            .await
            .map_err(map_err)?
            .iter()
            .map(|row| Relation {
                constraint: row.get(0),
                database: database.clone(),
                table: diagram_name(&row.get::<_, String>(1), &row.get::<_, String>(2)),
                columns: row.get(3),
                referenced_database: database.clone(),
                referenced_table: diagram_name(&row.get::<_, String>(4), &row.get::<_, String>(5)),
                referenced_columns: row.get(6),
            })
            .collect();
        let total = tables.len();
        Ok(SchemaGraph::assemble(&database, tables, total, relations))
    }

    /// The foreign keys of other tables that reference `schema.table`.
    pub(super) async fn referencing(&self, schema: &str, table: &str) -> AppResult<Vec<ReferencingKey>> {
        let client = self.client.lock().await;
        let rows = client
            .query(
                &format!(
                    "SELECT con.conname::text, n.nspname::text, c.relname::text, {KEY_COLUMNS}, {REFERENCED_COLUMNS} \
                     FROM pg_constraint con JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE con.contype = 'f' AND con.confrelid = \
                           (SELECT t.oid FROM pg_class t JOIN pg_namespace tn ON tn.oid = t.relnamespace WHERE tn.nspname = $1 AND t.relname = $2) \
                     ORDER BY 2, 3, 1"
                ),
                &[&schema, &table],
            )
            .await
            .map_err(|error| AppError::from(map_err(error)))?;
        Ok(rows
            .iter()
            .map(|row| ReferencingKey {
                name: Some(row.get(0)),
                database: Some(self.database().to_owned()),
                schema: Some(row.get(1)),
                table: row.get(2),
                columns: row.get(3),
                ref_columns: row.get(4),
            })
            .collect())
    }
}
