//! The schema compare snapshot of one schema: a handful of bulk catalog reads (relations, columns, constraints,
//! indexes, view dependencies, objects not compared), never one round trip per table.
//!
//! The reads run in a read-only transaction with `search_path` set to `pg_catalog` only, so the server qualifies every
//! user object in the text it returns (`format_type`, `pg_get_expr`, `pg_get_constraintdef`, `pg_get_viewdef`, regclass
//! literals) the same way for every scope. The dialect drops the scope's qualifier to compare and maps it to the target
//! schema to render.

use std::collections::HashMap;

use super::{
    catalog::fk_action,
    graph::{KEY_COLUMNS, REFERENCED_COLUMNS},
    map_err, PgEngine,
};
use crate::{
    contract::{
        CheckDef, ColumnDef, ForeignKeyDef, GeneratedDef, IdentityDef, IdentityKind, IndexDef, IndexPart, KeyDef, SchemaSnapshot,
        SkippedObject, SnapshotScope, TableDef, ViewDef,
    },
    error::AppResult,
};

/// Relations of the schema that are not owned by an extension.
const NOT_EXTENSION: &str =
    "NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_class'::regclass AND e.objid = c.oid AND e.deptype = 'e')";

const NOT_COMPARED: &str = "Not compared in this version";

impl PgEngine {
    pub(super) async fn schema_snapshot(&self, scope: &SnapshotScope) -> AppResult<SchemaSnapshot> {
        let schema = scope
            .schema
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "public".to_owned());
        let mut client = self.client.lock().await;
        let tx = client.transaction().await.map_err(map_err)?;
        tx.batch_execute("SET TRANSACTION READ ONLY; SET LOCAL search_path = pg_catalog")
            .await
            .map_err(map_err)?;
        let exists = tx
            .query_opt("SELECT 1 FROM pg_namespace WHERE nspname = $1", &[&schema])
            .await
            .map_err(map_err)?;
        if exists.is_none() {
            return Err(crate::error::AppError::new(format!(
                "There is no schema named '{schema}' in database '{}'",
                scope.database
            )));
        }

        let mut tables: Vec<TableDef> = Vec::new();
        let mut views: Vec<ViewDef> = Vec::new();
        let mut skipped: Vec<SkippedObject> = Vec::new();
        let skip = |skipped: &mut Vec<SkippedObject>, kind: &str, name: String, reason: &str| {
            skipped.push(SkippedObject {
                kind: kind.to_owned(),
                name,
                reason: reason.to_owned(),
            })
        };

        let relations = tx
            .query(
                &format!(
                    "SELECT c.relname::text, c.relkind::text, c.relispartition, obj_description(c.oid, 'pg_class'), \
                            CASE WHEN c.relkind IN ('v','m') THEN pg_get_viewdef(c.oid, true) END \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = $1 AND c.relkind IN ('r','p','v','m','f') AND {NOT_EXTENSION} ORDER BY c.relname"
                ),
                &[&schema],
            )
            .await
            .map_err(map_err)?;
        for row in &relations {
            let (name, relkind, partition): (String, String, bool) = (row.get(0), row.get(1), row.get(2));
            let comment: Option<String> = row.get(3);
            match relkind.as_str() {
                _ if partition => skip(&mut skipped, "partition", name, NOT_COMPARED),
                "r" => tables.push(TableDef {
                    comment,
                    ..TableDef::new(&name)
                }),
                "p" => skip(&mut skipped, "partitioned table", name, NOT_COMPARED),
                "f" => skip(&mut skipped, "foreign table", name, NOT_COMPARED),
                kind => views.push(ViewDef {
                    name,
                    definition: row.get::<_, Option<String>>(4).unwrap_or_default(),
                    materialized: kind == "m",
                    depends_on: Vec::new(),
                    comment,
                }),
            }
        }
        let table_at: HashMap<String, usize> = tables.iter().enumerate().map(|(i, t)| (t.name.clone(), i)).collect();
        let only_tables = "c.relkind = 'r' AND NOT c.relispartition";

        let columns = tx
            .query(
                &format!(
                    "SELECT c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull, \
                            CASE WHEN a.attgenerated = '' THEN pg_get_expr(d.adbin, d.adrelid) END, \
                            CASE WHEN a.attgenerated <> '' THEN pg_get_expr(d.adbin, d.adrelid) END, \
                            a.attidentity::text, seq.seqstart, seq.seqincrement, \
                            CASE WHEN a.attcollation <> t.typcollation THEN co.collname::text END, \
                            col_description(c.oid, a.attnum), \
                            EXISTS (SELECT 1 FROM pg_depend own JOIN pg_class sc ON sc.oid = own.objid AND sc.relkind = 'S' \
                                    WHERE own.classid = 'pg_class'::regclass AND own.refobjid = c.oid \
                                      AND own.refobjsubid = a.attnum AND own.deptype = 'a') \
                     FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_type t ON t.oid = a.atttypid \
                     LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                     LEFT JOIN pg_collation co ON co.oid = a.attcollation \
                     LEFT JOIN LATERAL (SELECT s.seqstart, s.seqincrement FROM pg_depend dep JOIN pg_sequence s ON s.seqrelid = dep.objid \
                                        WHERE dep.classid = 'pg_class'::regclass AND dep.refobjid = c.oid AND dep.refobjsubid = a.attnum \
                                          AND dep.deptype = 'i' LIMIT 1) seq ON true \
                     WHERE n.nspname = $1 AND {only_tables} AND a.attnum > 0 AND NOT a.attisdropped \
                     ORDER BY c.relname, a.attnum"
                ),
                &[&schema],
            )
            .await
            .map_err(map_err)?;
        for row in &columns {
            let Some(&at) = table_at.get(&row.get::<_, String>(0)) else {
                continue;
            };
            let default: Option<String> = row.get(4);
            let generated: Option<String> = row.get(5);
            let identity_code: String = row.get(6);
            let identity = match identity_code.as_str() {
                "a" | "d" => Some(IdentityDef {
                    kind: if identity_code == "a" {
                        IdentityKind::Always
                    } else {
                        IdentityKind::ByDefault
                    },
                    start: row.get(7),
                    increment: row.get(8),
                }),
                // serial: a nextval default on a sequence the column owns (a shared sequence stays a plain default).
                _ if row.get::<_, bool>(11) && default.as_deref().is_some_and(|d| d.starts_with("nextval(")) => Some(IdentityDef {
                    kind: IdentityKind::Serial,
                    start: None,
                    increment: None,
                }),
                _ => None,
            };
            tables[at].columns.push(ColumnDef {
                name: row.get(1),
                type_name: row.get(2),
                nullable: !row.get::<_, bool>(3),
                default,
                default_name: None,
                identity,
                generated: generated.map(|expression| GeneratedDef { expression, stored: true }),
                collation: row.get(9),
                comment: row.get(10),
            });
        }

        let constraints = tx
            .query(
                &format!(
                    "SELECT c.relname::text, con.conname::text, con.contype::text, pg_get_constraintdef(con.oid, true), {KEY_COLUMNS}, \
                            fn.nspname::text, fc.relname::text, {REFERENCED_COLUMNS}, con.confupdtype::text, con.confdeltype::text \
                     FROM pg_constraint con JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                     LEFT JOIN pg_class fc ON fc.oid = con.confrelid LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                     WHERE n.nspname = $1 AND {only_tables} AND con.contype IN ('p','u','c','f','x') ORDER BY c.relname, con.conname"
                ),
                &[&schema],
            )
            .await
            .map_err(map_err)?;
        for row in &constraints {
            let table: String = row.get(0);
            let Some(&at) = table_at.get(&table) else { continue };
            let (name, contype, definition, columns): (String, String, String, Vec<String>) =
                (row.get(1), row.get(2), row.get(3), row.get(4));
            let key = || KeyDef {
                name: Some(name.clone()),
                columns: columns.clone(),
                system_named: false,
            };
            let table = &mut tables[at];
            match contype.as_str() {
                "p" => table.primary_key = Some(key()),
                "u" => table.uniques.push(key()),
                "c" => table.checks.push(CheckDef {
                    name: Some(name.clone()),
                    expression: check_expression(&definition),
                    system_named: false,
                }),
                "f" => {
                    let ref_schema: Option<String> = row.get(5);
                    let action = |code: String| fk_action(&code).filter(|a| a != "NO ACTION");
                    table.foreign_keys.push(ForeignKeyDef {
                        name: Some(name.clone()),
                        columns: columns.clone(),
                        ref_schema: ref_schema.filter(|s| *s != schema),
                        ref_table: row.get::<_, Option<String>>(6).unwrap_or_default(),
                        ref_columns: row.get(7),
                        on_update: action(row.get(8)),
                        on_delete: action(row.get(9)),
                        system_named: false,
                    });
                }
                _ => skip(&mut skipped, "exclusion constraint", format!("{}.{name}", table.name), NOT_COMPARED),
            }
        }

        let indexes = tx
            .query(
                &format!(
                    "SELECT c.relname::text, i.relname::text, ix.indisunique, am.amname::text, \
                            ARRAY(SELECT (SELECT a.attname::text FROM pg_attribute a WHERE a.attrelid = ix.indrelid AND a.attnum = ix.indkey[k - 1]) \
                                  FROM generate_series(1, ix.indnkeyatts::int) k ORDER BY k), \
                            ARRAY(SELECT CASE WHEN ix.indkey[k - 1] = 0 THEN pg_get_indexdef(ix.indexrelid, k, true) END \
                                  FROM generate_series(1, ix.indnkeyatts::int) k ORDER BY k), \
                            ARRAY(SELECT (ix.indoption[k - 1] & 1) = 1 FROM generate_series(1, ix.indnkeyatts::int) k ORDER BY k), \
                            ARRAY(SELECT a.attname::text FROM generate_series(ix.indnkeyatts::int + 1, ix.indnatts::int) k \
                                  JOIN pg_attribute a ON a.attrelid = ix.indrelid AND a.attnum = ix.indkey[k - 1] ORDER BY k), \
                            pg_get_expr(ix.indpred, ix.indrelid, true) \
                     FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid JOIN pg_class c ON c.oid = ix.indrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace JOIN pg_am am ON am.oid = i.relam \
                     WHERE n.nspname = $1 AND {only_tables} \
                       AND NOT EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = ix.indexrelid AND con.conrelid = ix.indrelid \
                                       AND con.contype IN ('p','u','x')) \
                     ORDER BY c.relname, i.relname"
                ),
                &[&schema],
            )
            .await
            .map_err(map_err)?;
        for row in &indexes {
            let Some(&at) = table_at.get(&row.get::<_, String>(0)) else {
                continue;
            };
            let columns: Vec<Option<String>> = row.get(4);
            let expressions: Vec<Option<String>> = row.get(5);
            let descending: Vec<bool> = row.get(6);
            let parts = columns
                .into_iter()
                .zip(expressions)
                .zip(descending)
                .map(|((column, expression), descending)| IndexPart {
                    expression: if column.is_none() { expression } else { None },
                    column,
                    descending,
                })
                .collect();
            tables[at].indexes.push(IndexDef {
                name: row.get(1),
                parts,
                unique: row.get(2),
                method: Some(row.get(3)),
                predicate: row.get(8),
                include: row.get(7),
                system_named: false,
            });
        }

        let dependencies = tx
            .query(
                "SELECT DISTINCT v.relname::text, dc.relname::text \
                 FROM pg_class v JOIN pg_namespace n ON n.oid = v.relnamespace \
                 JOIN pg_rewrite r ON r.ev_class = v.oid \
                 JOIN pg_depend d ON d.classid = 'pg_rewrite'::regclass AND d.objid = r.oid AND d.refclassid = 'pg_class'::regclass \
                 JOIN pg_class dc ON dc.oid = d.refobjid \
                 WHERE n.nspname = $1 AND v.relkind IN ('v','m') AND dc.oid <> v.oid AND dc.relnamespace = v.relnamespace \
                 ORDER BY 1, 2",
                &[&schema],
            )
            .await
            .map_err(map_err)?;
        for row in &dependencies {
            let (view, dependency): (String, String) = (row.get(0), row.get(1));
            if let Some(view) = views.iter_mut().find(|v| v.name == view) {
                view.depends_on.push(dependency);
            }
        }

        let others = tx
            .query(
                &format!(
                    "SELECT 'sequence', c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = $1 AND c.relkind = 'S' AND {NOT_EXTENSION} \
                       AND NOT EXISTS (SELECT 1 FROM pg_depend o WHERE o.classid = 'pg_class'::regclass AND o.objid = c.oid AND o.deptype IN ('a','i')) \
                     UNION ALL \
                     SELECT CASE p.prokind WHEN 'p' THEN 'procedure' WHEN 'a' THEN 'aggregate' ELSE 'function' END, \
                            p.proname::text || '(' || pg_get_function_identity_arguments(p.oid) || ')' \
                     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                     WHERE n.nspname = $1 AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_proc'::regclass AND e.objid = p.oid AND e.deptype = 'e') \
                     UNION ALL \
                     SELECT CASE t.typtype WHEN 'e' THEN 'enum' WHEN 'd' THEN 'domain' WHEN 'r' THEN 'range' ELSE 'type' END, t.typname::text \
                     FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
                     WHERE n.nspname = $1 AND (t.typtype IN ('e','d','r') OR (t.typtype = 'c' AND (SELECT r.relkind FROM pg_class r WHERE r.oid = t.typrelid) = 'c')) \
                       AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_type'::regclass AND e.objid = t.oid AND e.deptype = 'e') \
                     UNION ALL \
                     SELECT 'trigger', c.relname::text || '.' || tg.tgname::text FROM pg_trigger tg JOIN pg_class c ON c.oid = tg.tgrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND NOT tg.tgisinternal \
                     UNION ALL \
                     SELECT 'policy', c.relname::text || '.' || pol.polname::text FROM pg_policy pol JOIN pg_class c ON c.oid = pol.polrelid \
                     JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 \
                     UNION ALL \
                     SELECT 'index', c.relname::text || '.' || i.relname::text FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid \
                     JOIN pg_class c ON c.oid = ix.indrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = $1 AND c.relkind = 'm' \
                     ORDER BY 1, 2"
                ),
                &[&schema],
            )
            .await
            .map_err(map_err)?;
        for row in &others {
            let kind: String = row.get(0);
            let reason = if kind == "index" {
                "Indexes of materialized views are not compared in this version"
            } else {
                NOT_COMPARED
            };
            skip(&mut skipped, &kind, row.get(1), reason);
        }
        drop(tx);

        Ok(SchemaSnapshot {
            engine: super::descriptor::ID.to_owned(),
            server_version: self.server_version().to_owned(),
            scope: SnapshotScope {
                database: scope.database.clone(),
                schema: Some(schema),
            },
            case_insensitive_names: false,
            tables,
            views,
            skipped,
            read_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}

/// `CHECK ((qty > 0)) NOT VALID` → `(qty > 0)`: the expression the dialect renders back inside `CHECK (...)`.
pub(super) fn check_expression(definition: &str) -> String {
    let text = definition.trim();
    let text = text.strip_suffix("NOT VALID").map(str::trim_end).unwrap_or(text);
    let text = text.strip_suffix("NO INHERIT").map(str::trim_end).unwrap_or(text);
    let body = text.strip_prefix("CHECK").map(str::trim_start).unwrap_or(text);
    match body.strip_prefix('(').and_then(|inner| inner.strip_suffix(')')) {
        Some(inner) if balanced(inner) => inner.trim().to_owned(),
        _ => body.to_owned(),
    }
}

/// Whether the parentheses of `text` (outside string literals) balance without going negative.
fn balanced(text: &str) -> bool {
    let mut depth = 0i32;
    let mut in_literal = false;
    for c in text.chars() {
        match c {
            '\'' => in_literal = !in_literal,
            '(' if !in_literal => depth += 1,
            ')' if !in_literal => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    depth == 0
}
