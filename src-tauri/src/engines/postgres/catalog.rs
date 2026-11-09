//! PostgreSQL catalog: the object tree from pg_catalog, object descriptions, and DDL built from the catalogs and the
//! `pg_get_*def` functions (PostgreSQL has no server-side "SHOW CREATE TABLE").

use super::{map_err, types};
use crate::engines::common::{
    CatalogNode, ColumnInfo, EngineError, ForeignKeyInfo, IndexInfo, KeyInfo, NodeKind, ObjectDescription, ObjectRef, Result, ValueKind,
};
use tokio_postgres::Client;

/// The `extra` fact that names what kind of relation a description is; browsing reads it to decide whether `xmin` is a
/// usable row version (ordinary tables only).
pub const RELATION_KIND: &str = "Relation kind";

/// `pg_class.relkind` as a sentence-case name.
pub fn relation_kind(relkind: &str) -> &'static str {
    match relkind {
        "r" => "table",
        "p" => "partitioned table",
        "v" => "view",
        "m" => "materialized view",
        "f" => "foreign table",
        _ => "relation",
    }
}

/// Column types selected as text in browse because their binary format has no decoder here.
pub const TEXT_CAST: &[&str] = &[
    "tsvector",
    "tsquery",
    "pg_lsn",
    "pg_snapshot",
    "txid_snapshot",
    "int4multirange",
    "int8multirange",
    "nummultirange",
    "datemultirange",
    "tsmultirange",
    "tstzmultirange",
    "geometry",
    "geography",
    // pgvector: `[1,2,3]` as text.
    "vector",
    "halfvec",
    "sparsevec",
];

pub(super) const USER_SCHEMAS: &str = "n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'";

const RESERVED: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "column",
    "constraint",
    "create",
    "default",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "for",
    "foreign",
    "from",
    "grant",
    "group",
    "having",
    "in",
    "into",
    "is",
    "join",
    "leading",
    "limit",
    "not",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "primary",
    "references",
    "select",
    "some",
    "table",
    "then",
    "to",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "when",
    "where",
    "with",
];

/// Quotes an identifier only when PostgreSQL needs it (for readable DDL).
pub fn ident(name: &str) -> String {
    let simple = name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '$');
    if simple && !RESERVED.contains(&name) {
        name.to_owned()
    } else {
        crate::engines::common::sqlbuild::quote_with(name, '"')
    }
}

fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", ident(schema), ident(name))
}

fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The current database: schemas with tables, views, materialized views, foreign tables, functions, procedures, sequences and
/// types, plus the installed extensions. Objects owned by extensions are left out.
pub async fn tree(c: &Client, database: &str) -> Result<CatalogNode> {
    let db = Some(database);
    let schemas = c
        .query(
            &format!("SELECT n.nspname::text FROM pg_namespace n WHERE {USER_SCHEMAS} ORDER BY 1"),
            &[],
        )
        .await
        .map_err(map_err)?;
    let mut nodes: Vec<(String, [Vec<CatalogNode>; 8])> = schemas.iter().map(|r| (r.get::<_, String>(0), Default::default())).collect();
    let slot = |nodes: &mut Vec<(String, [Vec<CatalogNode>; 8])>, schema: &str, i: usize, node: CatalogNode| {
        if let Some((_, f)) = nodes.iter_mut().find(|(s, _)| s == schema) {
            f[i].push(node);
        }
    };

    let rels = c
        .query(
            &format!(
                "SELECT n.nspname::text, c.relname::text, c.relkind::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE c.relkind IN ('r','p','v','m','f','S') AND NOT c.relispartition AND {USER_SCHEMAS} ORDER BY 1, 2"
            ),
            &[],
        )
        .await
        .map_err(map_err)?;
    for r in &rels {
        let (schema, name, relkind): (String, String, String) = (r.get(0), r.get(1), r.get(2));
        let (i, kind, detail) = match relkind.as_str() {
            "r" => (0, NodeKind::Table, None),
            "p" => (0, NodeKind::Table, Some("partitioned".to_owned())),
            "v" => (1, NodeKind::View, None),
            "m" => (2, NodeKind::MaterializedView, None),
            "f" => (3, NodeKind::ForeignTable, None),
            _ => (6, NodeKind::Sequence, None),
        };
        slot(
            &mut nodes,
            &schema,
            i,
            CatalogNode::object(ObjectRef::new(db, Some(&schema), &name, kind), detail),
        );
    }

    let procs = c
        .query(
            &format!(
                "SELECT n.nspname::text, p.proname::text, p.prokind::text, pg_get_function_identity_arguments(p.oid) \
                 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE {USER_SCHEMAS} \
                 AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e') \
                 ORDER BY 1, 2, 4"
            ),
            &[],
        )
        .await
        .map_err(map_err)?;
    for r in &procs {
        let (schema, name, prokind, args): (String, String, String, String) = (r.get(0), r.get(1), r.get(2), r.get(3));
        let (i, kind) = if prokind == "p" {
            (5, NodeKind::Procedure)
        } else {
            (4, NodeKind::Function)
        };
        let detail = Some(format!("({args}){}", if prokind == "a" { " aggregate" } else { "" }));
        slot(
            &mut nodes,
            &schema,
            i,
            CatalogNode::object(ObjectRef::new(db, Some(&schema), &name, kind), detail),
        );
    }

    let typs = c
        .query(
            &format!(
                "SELECT n.nspname::text, t.typname::text, t.typtype::text FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
                 WHERE t.typtype IN ('e','d','r','c') AND {USER_SCHEMAS} \
                 AND (t.typtype <> 'c' OR (SELECT c.relkind FROM pg_class c WHERE c.oid = t.typrelid) = 'c') \
                 AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e') \
                 ORDER BY 1, 2"
            ),
            &[],
        )
        .await
        .map_err(map_err)?;
    for r in &typs {
        let (schema, name, typtype): (String, String, String) = (r.get(0), r.get(1), r.get(2));
        let detail = match typtype.as_str() {
            "e" => "enum",
            "d" => "domain",
            "r" => "range",
            _ => "composite",
        };
        slot(
            &mut nodes,
            &schema,
            7,
            CatalogNode::object(ObjectRef::new(db, Some(&schema), &name, NodeKind::Type), Some(detail.into())),
        );
    }

    const LABELS: [&str; 8] = [
        "Tables",
        "Views",
        "Materialized Views",
        "Foreign Tables",
        "Functions",
        "Procedures",
        "Sequences",
        "Types",
    ];
    let mut children: Vec<CatalogNode> = nodes
        .into_iter()
        .map(|(schema, folders)| {
            let kids = folders
                .into_iter()
                .zip(LABELS)
                .map(|(items, label)| CatalogNode::container(NodeKind::Folder, label, items))
                .collect();
            let mut node = CatalogNode::container(NodeKind::Schema, &schema, kids);
            node.object = Some(ObjectRef::new(db, Some(&schema), &schema, NodeKind::Schema));
            node
        })
        .collect();

    let exts = c
        .query("SELECT extname::text, extversion FROM pg_extension ORDER BY 1", &[])
        .await
        .map_err(map_err)?;
    let ext_nodes = exts
        .iter()
        .map(|r| {
            CatalogNode::object(
                ObjectRef::new(db, None, &r.get::<_, String>(0), NodeKind::Extension),
                Some(r.get(1)),
            )
        })
        .collect();
    children.push(CatalogNode::container(NodeKind::Folder, "Extensions", ext_nodes));
    let mut root = CatalogNode::container(NodeKind::Database, database, children);
    root.object = Some(ObjectRef::new(db, None, database, NodeKind::Database));
    Ok(root)
}

/// Describes a relation, function, procedure, sequence, type or extension.
pub async fn describe(c: &Client, obj: &ObjectRef) -> Result<ObjectDescription> {
    let schema = obj.schema.clone().unwrap_or_else(|| "public".into());
    match obj.kind {
        NodeKind::Function | NodeKind::Procedure => describe_routine(c, obj, &schema).await,
        NodeKind::Type => describe_type(c, obj, &schema).await,
        NodeKind::Extension => describe_extension(c, obj).await,
        _ => describe_relation(c, obj, &schema).await,
    }
}

async fn columns_of(c: &Client, relid: u32) -> Result<Vec<ColumnInfo>> {
    let rows = c
        .query(
            "SELECT a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull, pg_get_expr(d.adbin, d.adrelid), \
                    col_description(a.attrelid, a.attnum), a.attidentity::text, a.attgenerated::text, \
                    bt.typname::text, bt.typtype::text, bt.typcategory::text, et.typname::text, et.typtype::text, et.typcategory::text \
             FROM pg_attribute a \
             JOIN pg_type t ON t.oid = a.atttypid \
             JOIN pg_type bt ON bt.oid = CASE WHEN t.typtype = 'd' THEN t.typbasetype ELSE t.oid END \
             LEFT JOIN pg_type et ON et.oid = bt.typelem AND bt.typcategory = 'A' \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            &[&relid],
        )
        .await
        .map_err(map_err)?;
    Ok(rows
        .iter()
        .map(|r| {
            let expr: Option<String> = r.get(3);
            let identity: String = r.get(5);
            let generated: String = r.get(6);
            let (base, base_type, base_cat): (String, String, String) = (r.get(7), r.get(8), r.get(9));
            let elem: Option<(String, String, String)> = r.get::<_, Option<String>>(10).map(|n| (n, r.get(11), r.get(12)));
            let kind = match elem {
                Some((n, t, cat)) => ValueKind::Array(Box::new(kind_for(&n, &t, &cat))),
                None => kind_for(&base, &base_type, &base_cat),
            };
            let is_generated = generated == "s";
            let type_name: String = r.get(1);
            ColumnInfo {
                hints: crate::engines::common::ColumnHints {
                    vector_dims: vector_dims(&base, &type_name),
                    ..Default::default()
                },
                name: r.get(0),
                type_name,
                kind,
                nullable: !r.get::<_, bool>(2),
                identity: match identity.as_str() {
                    "a" => Some("ALWAYS".into()),
                    "d" => Some("BY DEFAULT".into()),
                    _ if expr.as_deref().is_some_and(|e| e.starts_with("nextval(")) => Some("serial".into()),
                    _ => None,
                },
                generated: if is_generated { expr.clone() } else { None },
                default: if is_generated { None } else { expr },
                comment: r.get(4),
            }
        })
        .collect())
}

/// The dimensions of a pgvector column (`vector(4)`, `halfvec(768)`), so the grid offers the vector tools on it.
fn vector_dims(base: &str, type_name: &str) -> Option<u32> {
    if !matches!(base, "vector" | "halfvec") {
        return None;
    }
    type_name.split_once('(')?.1.strip_suffix(')')?.trim().parse().ok()
}

fn kind_for(name: &str, typtype: &str, category: &str) -> ValueKind {
    match typtype {
        "e" => ValueKind::Text,
        "r" | "c" | "m" => ValueKind::Other,
        _ => types::kind_by_name(name, category),
    }
}

fn fk_action(code: &str) -> Option<String> {
    Some(
        match code {
            "a" => "NO ACTION",
            "r" => "RESTRICT",
            "c" => "CASCADE",
            "n" => "SET NULL",
            "d" => "SET DEFAULT",
            _ => return None,
        }
        .to_owned(),
    )
}

async fn describe_relation(c: &Client, obj: &ObjectRef, schema: &str) -> Result<ObjectDescription> {
    let rel = c
        .query_opt(
            "SELECT c.oid, c.relkind::text, c.reltuples::float8, obj_description(c.oid, 'pg_class'), \
                    CASE WHEN c.relkind = 'p' THEN pg_get_partkeydef(c.oid) END, \
                    CASE WHEN c.relkind IN ('v','m') THEN pg_get_viewdef(c.oid, true) END, \
                    (SELECT s.srvname::text FROM pg_foreign_table ft JOIN pg_foreign_server s ON s.oid = ft.ftserver WHERE ft.ftrelid = c.oid) \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2",
            &[&schema, &obj.name],
        )
        .await
        .map_err(map_err)?
        .ok_or_else(|| EngineError::input(format!("There is no table or view named '{}' in schema '{schema}'.", obj.name)))?;
    let oid: u32 = rel.get(0);
    let relkind: String = rel.get(1);
    let kind = match relkind.as_str() {
        "v" => NodeKind::View,
        "m" => NodeKind::MaterializedView,
        "f" => NodeKind::ForeignTable,
        "S" => return describe_sequence(c, obj, schema).await,
        _ => NodeKind::Table,
    };
    let mut d = ObjectDescription::new(ObjectRef::new(obj.database.as_deref(), Some(schema), &obj.name, kind));
    d.comment = rel.get(3);
    d.extra.push((RELATION_KIND.into(), relation_kind(&relkind).into()));
    let tuples: f64 = rel.get(2);
    d.row_estimate = (tuples >= 0.0).then_some(tuples as i64);
    d.columns = columns_of(c, oid).await?;

    let cons = c
        .query(
            "SELECT con.conname::text, con.contype::text, pg_get_constraintdef(con.oid, true), \
                    ARRAY(SELECT a.attname::text FROM unnest(con.conkey) WITH ORDINALITY k(n, o) \
                          JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.o), \
                    fn.nspname::text, fc.relname::text, \
                    ARRAY(SELECT a.attname::text FROM unnest(con.confkey) WITH ORDINALITY k(n, o) \
                          JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n ORDER BY k.o), \
                    con.confupdtype::text, con.confdeltype::text \
             FROM pg_constraint con LEFT JOIN pg_class fc ON fc.oid = con.confrelid LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
             WHERE con.conrelid = $1 ORDER BY CASE con.contype WHEN 'p' THEN 0 WHEN 'u' THEN 1 WHEN 'f' THEN 2 ELSE 3 END, con.conname",
            &[&oid],
        )
        .await
        .map_err(map_err)?;
    let mut constraint_ddl = Vec::new();
    for r in &cons {
        let (name, contype, def, cols): (String, String, String, Vec<String>) = (r.get(0), r.get(1), r.get(2), r.get(3));
        constraint_ddl.push(format!("CONSTRAINT {} {def}", ident(&name)));
        match contype.as_str() {
            "p" => {
                d.primary_key = Some(KeyInfo {
                    name: Some(name),
                    columns: cols,
                })
            }
            "u" => d.unique_keys.push(KeyInfo {
                name: Some(name),
                columns: cols,
            }),
            "f" => d.foreign_keys.push(ForeignKeyInfo {
                ref_database: None,
                name: Some(name),
                columns: cols,
                ref_schema: r.get(4),
                ref_table: r.get::<_, Option<String>>(5).unwrap_or_default(),
                ref_columns: r.get(6),
                on_update: fk_action(&r.get::<_, String>(7)),
                on_delete: fk_action(&r.get::<_, String>(8)),
            }),
            _ => {}
        }
    }

    let idx = c
        .query(
            "SELECT i.relname::text, ix.indisunique, ix.indisprimary, am.amname::text, pg_get_indexdef(ix.indexrelid), \
                    ARRAY(SELECT pg_get_indexdef(ix.indexrelid, k, true) || CASE WHEN ix.indoption[k - 1] & 1 = 1 THEN ' DESC' ELSE '' END FROM generate_series(1, ix.indnkeyatts::int) k ORDER BY k), \
                    EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = ix.indexrelid AND con.contype IN ('p','u','x')) \
             FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid JOIN pg_am am ON am.oid = i.relam \
             WHERE ix.indrelid = $1 ORDER BY i.relname",
            &[&oid],
        )
        .await
        .map_err(map_err)?;
    let mut index_ddl = Vec::new();
    for r in &idx {
        let def: String = r.get(4);
        if !r.get::<_, bool>(6) {
            index_ddl.push(format!("{def};"));
        }
        d.indexes.push(IndexInfo {
            name: r.get(0),
            unique: r.get(1),
            primary: r.get(2),
            method: Some(r.get(3)),
            columns: r.get(5),
            definition: Some(def),
        });
    }

    let name = qualified(schema, &obj.name);
    let mut ddl = match kind {
        NodeKind::View => format!(
            "CREATE OR REPLACE VIEW {name} AS\n{};",
            rel.get::<_, Option<String>>(5).unwrap_or_default().trim_end_matches(';')
        ),
        NodeKind::MaterializedView => {
            format!(
                "CREATE MATERIALIZED VIEW {name} AS\n{}\nWITH DATA;",
                rel.get::<_, Option<String>>(5).unwrap_or_default().trim_end().trim_end_matches(';')
            )
        }
        _ => {
            let mut lines: Vec<String> = d
                .columns
                .iter()
                .map(|col| {
                    let mut l = format!("    {} {}", ident(&col.name), col.type_name);
                    match (&col.identity, &col.generated) {
                        (_, Some(g)) => l.push_str(&format!(" GENERATED ALWAYS AS ({g}) STORED")),
                        (Some(i), _) if i != "serial" => l.push_str(&format!(" GENERATED {i} AS IDENTITY")),
                        _ => {
                            if let Some(def) = &col.default {
                                l.push_str(&format!(" DEFAULT {def}"));
                            }
                        }
                    }
                    if !col.nullable {
                        l.push_str(" NOT NULL");
                    }
                    l
                })
                .collect();
            lines.extend(constraint_ddl.iter().map(|c| format!("    {c}")));
            let mut s = format!(
                "CREATE {}TABLE {name} (\n{}\n)",
                if kind == NodeKind::ForeignTable { "FOREIGN " } else { "" },
                lines.join(",\n")
            );
            if let Some(pk) = rel.get::<_, Option<String>>(4) {
                s.push_str(&format!(" PARTITION BY {pk}"));
            }
            if let Some(server) = rel.get::<_, Option<String>>(6) {
                s.push_str(&format!(" SERVER {}", ident(&server)));
            }
            s.push(';');
            s
        }
    };
    for i in index_ddl {
        ddl.push_str("\n\n");
        ddl.push_str(&i);
    }
    let what = match kind {
        NodeKind::View => "VIEW",
        NodeKind::MaterializedView => "MATERIALIZED VIEW",
        NodeKind::ForeignTable => "FOREIGN TABLE",
        _ => "TABLE",
    };
    if let Some(cm) = &d.comment {
        ddl.push_str(&format!("\n\nCOMMENT ON {what} {name} IS {};", literal(cm)));
    }
    for col in &d.columns {
        if let Some(cm) = &col.comment {
            ddl.push_str(&format!("\n\nCOMMENT ON COLUMN {name}.{} IS {};", ident(&col.name), literal(cm)));
        }
    }
    d.ddl = Some(ddl);
    Ok(d)
}

async fn describe_sequence(c: &Client, obj: &ObjectRef, schema: &str) -> Result<ObjectDescription> {
    let r = c
        .query_opt(
            "SELECT data_type::text, start_value, min_value, max_value, increment_by, cycle, cache_size, last_value \
             FROM pg_sequences WHERE schemaname = $1 AND sequencename = $2",
            &[&schema, &obj.name],
        )
        .await
        .map_err(map_err)?
        .ok_or_else(|| EngineError::input(format!("There is no sequence named '{}'.", obj.name)))?;
    let mut d = ObjectDescription::new(ObjectRef::new(obj.database.as_deref(), Some(schema), &obj.name, NodeKind::Sequence));
    let (ty, start, min, max, inc, cycle, cache): (String, i64, i64, i64, i64, bool, i64) =
        (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5), r.get(6));
    let last: Option<i64> = r.get(7);
    d.extra.push((
        "lastValue".into(),
        last.map(|v| v.to_string()).unwrap_or_else(|| "not used yet".into()),
    ));
    d.ddl = Some(format!(
        "CREATE SEQUENCE {} AS {ty} INCREMENT BY {inc} MINVALUE {min} MAXVALUE {max} START WITH {start} CACHE {cache}{};",
        qualified(schema, &obj.name),
        if cycle { " CYCLE" } else { " NO CYCLE" }
    ));
    Ok(d)
}

async fn describe_routine(c: &Client, obj: &ObjectRef, schema: &str) -> Result<ObjectDescription> {
    let rows = c
        .query(
            "SELECT pg_get_functiondef(p.oid), pg_get_function_identity_arguments(p.oid), obj_description(p.oid, 'pg_proc'), \
                    pg_get_function_result(p.oid), p.prokind::text \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = $1 AND p.proname = $2 AND p.prokind IN ('f','p','w') ORDER BY 2",
            &[&schema, &obj.name],
        )
        .await
        .map_err(map_err)?;
    if rows.is_empty() {
        return Err(EngineError::input(format!(
            "There is no function or procedure named '{}' in schema '{schema}'.",
            obj.name
        )));
    }
    let kind = if rows[0].get::<_, String>(4) == "p" {
        NodeKind::Procedure
    } else {
        NodeKind::Function
    };
    let mut d = ObjectDescription::new(ObjectRef::new(obj.database.as_deref(), Some(schema), &obj.name, kind));
    d.comment = rows[0].get(2);
    let mut ddl = Vec::new();
    for r in &rows {
        let def: String = r.get(0);
        ddl.push(format!("{};", def.trim_end()));
        let args: String = r.get(1);
        let result: Option<String> = r.get(3);
        d.extra.push((
            "signature".into(),
            format!("({args}){}", result.map(|r| format!(" RETURNS {r}")).unwrap_or_default()),
        ));
    }
    d.ddl = Some(ddl.join("\n\n"));
    Ok(d)
}

async fn describe_type(c: &Client, obj: &ObjectRef, schema: &str) -> Result<ObjectDescription> {
    let r = c
        .query_opt(
            "SELECT t.oid, t.typtype::text, format_type(t.typbasetype, t.typtypmod), t.typnotnull, t.typdefault, t.typrelid, \
                    ARRAY(SELECT e.enumlabel::text FROM pg_enum e WHERE e.enumtypid = t.oid ORDER BY e.enumsortorder), \
                    ARRAY(SELECT 'CONSTRAINT ' || quote_ident(con.conname) || ' ' || pg_get_constraintdef(con.oid, true) \
                          FROM pg_constraint con WHERE con.contypid = t.oid ORDER BY con.conname), \
                    (SELECT format_type(rg.rngsubtype, NULL) FROM pg_range rg WHERE rg.rngtypid = t.oid), \
                    obj_description(t.oid, 'pg_type') \
             FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typname = $2",
            &[&schema, &obj.name],
        )
        .await
        .map_err(map_err)?
        .ok_or_else(|| EngineError::input(format!("There is no type named '{}' in schema '{schema}'.", obj.name)))?;
    let mut d = ObjectDescription::new(ObjectRef::new(obj.database.as_deref(), Some(schema), &obj.name, NodeKind::Type));
    d.comment = r.get(9);
    let name = qualified(schema, &obj.name);
    let typtype: String = r.get(1);
    d.ddl = Some(match typtype.as_str() {
        "e" => {
            let labels: Vec<String> = r.get(6);
            d.extra.push(("kind".into(), "enum".into()));
            d.extra.push(("labels".into(), labels.join(", ")));
            format!(
                "CREATE TYPE {name} AS ENUM ({});",
                labels.iter().map(|l| literal(l)).collect::<Vec<_>>().join(", ")
            )
        }
        "d" => {
            d.extra.push(("kind".into(), "domain".into()));
            let mut s = format!("CREATE DOMAIN {name} AS {}", r.get::<_, String>(2));
            if let Some(def) = r.get::<_, Option<String>>(4) {
                s.push_str(&format!(" DEFAULT {def}"));
            }
            if r.get::<_, bool>(3) {
                s.push_str(" NOT NULL");
            }
            for con in r.get::<_, Vec<String>>(7) {
                s.push(' ');
                s.push_str(&con);
            }
            s.push(';');
            s
        }
        "r" => {
            d.extra.push(("kind".into(), "range".into()));
            format!(
                "CREATE TYPE {name} AS RANGE (SUBTYPE = {});",
                r.get::<_, Option<String>>(8).unwrap_or_default()
            )
        }
        _ => {
            d.extra.push(("kind".into(), "composite".into()));
            d.columns = columns_of(c, r.get(5)).await?;
            let fields: Vec<String> = d
                .columns
                .iter()
                .map(|col| format!("    {} {}", ident(&col.name), col.type_name))
                .collect();
            format!("CREATE TYPE {name} AS (\n{}\n);", fields.join(",\n"))
        }
    });
    Ok(d)
}

async fn describe_extension(c: &Client, obj: &ObjectRef) -> Result<ObjectDescription> {
    let r = c
        .query_opt(
            "SELECT e.extversion, n.nspname::text, x.comment FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace \
             LEFT JOIN pg_available_extensions x ON x.name = e.extname WHERE e.extname = $1",
            &[&obj.name],
        )
        .await
        .map_err(map_err)?
        .ok_or_else(|| EngineError::input(format!("The extension '{}' is not installed.", obj.name)))?;
    let mut d = ObjectDescription::new(ObjectRef::new(obj.database.as_deref(), None, &obj.name, NodeKind::Extension));
    let (version, schema): (String, String) = (r.get(0), r.get(1));
    d.comment = r.get(2);
    d.extra.push(("version".into(), version.clone()));
    d.ddl = Some(format!(
        "CREATE EXTENSION IF NOT EXISTS {} WITH SCHEMA {} VERSION {};",
        ident(&obj.name),
        ident(&schema),
        literal(&version)
    ));
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(ident("orders"), "orders");
        assert_eq!(ident("Orders"), "\"Orders\"");
        assert_eq!(ident("order"), "\"order\"");
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(qualified("sales", "targets"), "sales.targets");
        assert_eq!(literal("it's"), "'it''s'");
    }
}
