//! The schema compare snapshot of one MariaDB/MySQL database: a handful of bulk `information_schema` reads (tables,
//! columns, indexes, foreign keys, checks, views, objects not compared), never one round trip per table.
//!
//! A database is the scope: the snapshot names it as both the database and the schema, so the dialect drops its
//! `` `db`. `` qualifier (views are stored fully qualified) and maps it to the target database to render.
//!
//! The catalog text is turned into SQL the dialect can render back where only the reader knows what it means:
//! MySQL reports a string default unquoted and an expression default with `DEFAULT_GENERATED`, MariaDB quotes its
//! literals and spells "no default" as `NULL`; `ON UPDATE CURRENT_TIMESTAMP` (from `EXTRA`) rides on the default; a
//! MariaDB JSON column (LONGTEXT with a `json_valid` check named after it) reads as `json`.

use std::collections::HashMap;

use mysql_async::Conn;

use super::{
    catalog::{text_rows, TextRow},
    connect::Server,
};
use crate::{
    contract::{
        CheckDef, ColumnDef, ForeignKeyDef, GeneratedDef, IdentityDef, IdentityKind, IndexDef, IndexPart, KeyDef, SchemaSnapshot,
        SkippedObject, SnapshotScope, TableDef, ViewDef,
    },
    error::{AppError, AppResult},
};

const NOT_COMPARED: &str = "Not compared in this version";

/// Numeric types whose defaults the catalog reports as bare numbers.
const NUMERIC_TYPES: [&str; 12] = [
    "tinyint",
    "smallint",
    "mediumint",
    "int",
    "integer",
    "bigint",
    "decimal",
    "numeric",
    "float",
    "double",
    "real",
    "bit",
];

/// Reads the snapshot of `scope.database` (MySQL has no schemas below a database).
pub async fn snapshot(conn: &mut Conn, server: &Server, engine: &str, scope: &SnapshotScope) -> AppResult<SchemaSnapshot> {
    let database = scope.database.trim().to_owned();
    let param = || (database.clone(),);
    let exists = text_rows(
        conn,
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
        param(),
    )
    .await?;
    if exists.is_empty() {
        return Err(AppError::new(format!("There is no database named '{database}'")));
    }
    let settings = text_rows(conn, "SELECT @@lower_case_table_names", ()).await?;
    let case_insensitive_names = settings.first().and_then(|row| row.number(0)).unwrap_or(0) != 0;

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

    // Tables and views, with each table's default collation (a column with that collation reads as "no collation").
    let relations = text_rows(
        conn,
        "SELECT TABLE_NAME, TABLE_TYPE, ENGINE, TABLE_COLLATION, TABLE_COMMENT, CREATE_OPTIONS \
         FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
        param(),
    )
    .await?;
    let mut table_collation: HashMap<String, String> = HashMap::new();
    for row in &relations {
        let name = row.text(0);
        match row.text(1).to_ascii_uppercase().as_str() {
            "BASE TABLE" => {
                let mut options = Vec::new();
                if let Some(engine) = row.opt(2).filter(|e| !e.is_empty()) {
                    options.push(("engine".to_owned(), engine));
                }
                if let Some(collation) = row.opt(3).filter(|c| !c.is_empty()) {
                    table_collation.insert(name.clone(), collation.clone());
                    options.push(("collation".to_owned(), collation));
                }
                if row.text(5).to_ascii_lowercase().contains("partitioned") {
                    skip(
                        &mut skipped,
                        "partitioning",
                        name.clone(),
                        "Partitioning is not compared in this version",
                    );
                }
                tables.push(TableDef {
                    comment: row.opt(4).filter(|c| !c.is_empty()),
                    options,
                    ..TableDef::new(&name)
                });
            }
            // Unreadable until `VIEWS` shows its definition.
            "VIEW" => views.push(ViewDef {
                name,
                unreadable: true,
                ..ViewDef::default()
            }),
            "SEQUENCE" => skip(&mut skipped, "sequence", name, NOT_COMPARED),
            "SYSTEM VERSIONED" => skip(&mut skipped, "system-versioned table", name, NOT_COMPARED),
            other => skip(&mut skipped, &other.to_ascii_lowercase(), name, NOT_COMPARED),
        }
    }
    let table_at: HashMap<String, usize> = tables.iter().enumerate().map(|(i, t)| (t.name.clone(), i)).collect();

    let columns = text_rows(
        conn,
        "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, COLLATION_NAME, \
                COLUMN_COMMENT, GENERATION_EXPRESSION \
         FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, ORDINAL_POSITION",
        param(),
    )
    .await?;
    for row in &columns {
        let table = row.text(0);
        let Some(&at) = table_at.get(&table) else { continue };
        let column = column_def(row, server.mariadb, table_collation.get(&table).map(String::as_str));
        tables[at].columns.push(column);
    }

    let checks = if server.mariadb {
        text_rows(
            conn,
            "SELECT TABLE_NAME, CONSTRAINT_NAME, CHECK_CLAUSE FROM information_schema.CHECK_CONSTRAINTS \
             WHERE CONSTRAINT_SCHEMA = ? ORDER BY TABLE_NAME, CONSTRAINT_NAME",
            param(),
        )
        .await?
    } else if server.version >= (8, 0, 16) {
        text_rows(
            conn,
            "SELECT tc.TABLE_NAME, cc.CONSTRAINT_NAME, cc.CHECK_CLAUSE FROM information_schema.TABLE_CONSTRAINTS tc \
             JOIN information_schema.CHECK_CONSTRAINTS cc \
               ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
             WHERE tc.TABLE_SCHEMA = ? AND tc.CONSTRAINT_TYPE = 'CHECK' ORDER BY tc.TABLE_NAME, cc.CONSTRAINT_NAME",
            param(),
        )
        .await?
    } else {
        Vec::new()
    };
    for row in &checks {
        let Some(&at) = table_at.get(&row.text(0)) else { continue };
        let (name, clause) = (row.text(1), unescape_catalog_text(&row.text(2)));
        let table = &mut tables[at];
        // MariaDB's JSON: a LONGTEXT column with a `json_valid` check named after it.
        if server.mariadb && is_json_check(&name, &clause) {
            if let Some(column) = table
                .columns
                .iter_mut()
                .find(|c| c.name == name && c.type_name.eq_ignore_ascii_case("longtext"))
            {
                column.type_name = "json".to_owned();
                continue;
            }
        }
        let system_named = system_named_check(&name, &table.name, server.mariadb);
        table.checks.push(CheckDef {
            name: Some(name),
            expression: clause,
            system_named,
        });
    }

    // MySQL 8.0.13+ has functional key parts (EXPRESSION); MariaDB has none.
    let expression = if !server.mariadb && server.version >= (8, 0, 13) {
        "EXPRESSION"
    } else {
        "NULL"
    };
    let indexes = text_rows(
        conn,
        &format!(
            "SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE, COLUMN_NAME, SUB_PART, COLLATION, INDEX_TYPE, {expression} \
             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX"
        ),
        param(),
    )
    .await?;
    for row in &indexes {
        let Some(&at) = table_at.get(&row.text(0)) else { continue };
        let table = &mut tables[at];
        let name = row.text(1);
        let part = index_part(row);
        if name == "PRIMARY" {
            let key = table.primary_key.get_or_insert_with(|| KeyDef {
                name: Some(name.clone()),
                columns: Vec::new(),
                system_named: true,
            });
            key.columns.push(part.column.unwrap_or_default());
            continue;
        }
        match table.indexes.last_mut() {
            Some(last) if last.name == name => last.parts.push(part),
            _ => {
                let method = row.text(6).to_ascii_uppercase();
                table.indexes.push(IndexDef {
                    name,
                    parts: vec![part],
                    unique: row.text(2) == "0",
                    method: (!method.is_empty() && method != "BTREE").then_some(method),
                    predicate: None,
                    include: Vec::new(),
                    system_named: false,
                });
            }
        }
    }

    let keys = text_rows(
        conn,
        "SELECT k.TABLE_NAME, k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, \
                k.REFERENCED_COLUMN_NAME, r.UPDATE_RULE, r.DELETE_RULE \
         FROM information_schema.KEY_COLUMN_USAGE k \
         JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
           ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME \
         WHERE k.TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL \
         ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
        param(),
    )
    .await?;
    for row in &keys {
        let Some(&at) = table_at.get(&row.text(0)) else { continue };
        let table = &mut tables[at];
        let name = row.text(1);
        match table.foreign_keys.last_mut() {
            Some(last) if last.name.as_deref() == Some(name.as_str()) => {
                last.columns.push(row.text(2));
                last.ref_columns.push(row.text(5));
            }
            _ => {
                let action = |rule: Option<String>| rule.filter(|r| !r.eq_ignore_ascii_case("NO ACTION"));
                table.foreign_keys.push(ForeignKeyDef {
                    name: Some(name),
                    columns: vec![row.text(2)],
                    ref_schema: row.opt(3).filter(|schema| *schema != database),
                    ref_table: row.text(4),
                    ref_columns: vec![row.text(5)],
                    on_update: action(row.opt(6)),
                    on_delete: action(row.opt(7)),
                    system_named: false,
                });
            }
        }
    }

    let definitions = text_rows(
        conn,
        "SELECT TABLE_NAME, VIEW_DEFINITION, SECURITY_TYPE, CHECK_OPTION, DEFINER FROM information_schema.VIEWS \
         WHERE TABLE_SCHEMA = ? ORDER BY TABLE_NAME",
        param(),
    )
    .await?;
    let mut names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
    names.extend(views.iter().map(|v| v.name.clone()));
    for row in &definitions {
        let name = row.text(0);
        if let Some(view) = views.iter_mut().find(|v| v.name == name) {
            view.options = view_options(row.opt(2), row.opt(3), row.opt(4));
            // Empty without the SHOW VIEW privilege (unless the session's user is the definer).
            let definition = row.text(1);
            if definition.trim().is_empty() {
                continue;
            }
            view.unreadable = false;
            view.definition = definition;
            view.depends_on = view_dependencies(&view.definition, &database, &names, &name);
        }
    }

    for (sql, kind) in [
        (
            "SELECT CONCAT(EVENT_OBJECT_TABLE, '.', TRIGGER_NAME) FROM information_schema.TRIGGERS \
             WHERE TRIGGER_SCHEMA = ? ORDER BY 1",
            Some("trigger"),
        ),
        (
            "SELECT ROUTINE_NAME, LOWER(ROUTINE_TYPE) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? ORDER BY 1",
            None,
        ),
        (
            "SELECT EVENT_NAME FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? ORDER BY 1",
            Some("event"),
        ),
    ] {
        for row in text_rows(conn, sql, param()).await? {
            let kind = kind.map(str::to_owned).unwrap_or_else(|| row.text(1));
            skip(&mut skipped, &kind, row.text(0), NOT_COMPARED);
        }
    }

    let (major, minor, patch) = server.version;
    Ok(SchemaSnapshot {
        engine: engine.to_owned(),
        server_version: format!("{major}.{minor}.{patch}"),
        scope: SnapshotScope {
            database: database.clone(),
            schema: Some(database),
        },
        case_insensitive_names,
        tables,
        views,
        skipped,
        read_at: chrono::Utc::now().to_rfc3339(),
    })
}

/// One column from a `COLUMNS` row (table, name, data type, column type, nullable, default, extra, collation, comment,
/// generation expression).
fn column_def(row: &TextRow, mariadb: bool, table_collation: Option<&str>) -> ColumnDef {
    let extra = row.text(6);
    let lower = extra.to_ascii_lowercase();
    let generated = row
        .opt(9)
        .filter(|e| !e.is_empty())
        .filter(|_| lower.contains("generated") || lower.contains("virtual") || lower.contains("stored") || lower.contains("persistent"))
        .map(|expression| GeneratedDef {
            expression: unescape_catalog_text(&expression),
            stored: lower.contains("stored") || lower.contains("persistent"),
        });
    let default = if generated.is_some() {
        None
    } else {
        column_default(row.opt(5), &extra, &row.text(2), mariadb)
    };
    ColumnDef {
        name: row.text(1),
        type_name: row.text(3),
        nullable: row.text(4).eq_ignore_ascii_case("YES"),
        default,
        default_name: None,
        identity: lower.contains("auto_increment").then_some(IdentityDef {
            kind: IdentityKind::AutoIncrement,
            start: None,
            increment: None,
        }),
        generated,
        collation: row.opt(7).filter(|c| !c.is_empty() && Some(c.as_str()) != table_collation),
        comment: row.opt(8).filter(|c| !c.is_empty()),
    }
}

/// The default as SQL: MySQL reports literals unquoted (an expression is marked `DEFAULT_GENERATED` in `EXTRA`), MariaDB
/// reports SQL and spells "no default" as `NULL`. `ON UPDATE …` from `EXTRA` is appended.
pub(super) fn column_default(raw: Option<String>, extra: &str, data_type: &str, mariadb: bool) -> Option<String> {
    let lower = extra.to_ascii_lowercase();
    let default = match raw {
        None => None,
        Some(text) if mariadb => (text != "NULL").then_some(text),
        Some(text) if lower.contains("default_generated") => Some(unescape_catalog_text(&text)),
        Some(text) => {
            let data_type = data_type.trim().to_ascii_lowercase();
            if NUMERIC_TYPES.contains(&data_type.as_str()) || text.starts_with("b'") {
                Some(text)
            } else {
                Some(format!("'{}'", text.replace('\\', "\\\\").replace('\'', "''")))
            }
        }
    };
    let on_update = lower.find("on update ").map(|at| {
        extra[at + "on update ".len()..]
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned()
    });
    match on_update.filter(|text| !text.is_empty()) {
        Some(on_update) => Some(format!("{} ON UPDATE {on_update}", default.as_deref().unwrap_or("NULL"))),
        None => default,
    }
}

/// MySQL shows generation expressions (and some check clauses) with every quote escaped (`_utf8mb4\'a\'`); such text
/// is unescaped. Text with a bare quote is left as it is.
pub(super) fn unescape_catalog_text(text: &str) -> String {
    if !text.contains("\\'") {
        return text.to_owned();
    }
    let mut escaped_only = true;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '\'' => {
                escaped_only = false;
                break;
            }
            _ => {}
        }
    }
    if !escaped_only {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\'') => out.push('\''),
                Some('\\') => out.push_str("\\\\"),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// MariaDB's JSON check: `json_valid(`col`)` named after the column.
pub(super) fn is_json_check(name: &str, clause: &str) -> bool {
    let compact: String = clause.chars().filter(|c| !c.is_whitespace() && *c != '`').collect();
    compact.eq_ignore_ascii_case(&format!("json_valid({name})"))
}

/// A check the server named: MySQL `<table>_chk_<n>`, MariaDB `CONSTRAINT_<n>`.
pub(super) fn system_named_check(name: &str, table: &str, mariadb: bool) -> bool {
    let numbered = |prefix: &str| {
        name.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            && name.len() > prefix.len()
            && name[prefix.len()..].chars().all(|c| c.is_ascii_digit())
    };
    if mariadb {
        numbered("CONSTRAINT_")
    } else {
        numbered(&format!("{table}_chk_"))
    }
}

/// One key part from a `STATISTICS` row: a column, a prefix (`` `name`(10) ``, kept as an expression) or an expression.
fn index_part(row: &TextRow) -> IndexPart {
    let descending = row.text(5).eq_ignore_ascii_case("D");
    match (row.opt(3), row.number(4), row.opt(7).filter(|e| !e.is_empty())) {
        (Some(column), Some(length), _) => IndexPart {
            column: None,
            expression: Some(format!("{}({length})", super::catalog::quote_ident(&column))),
            descending,
        },
        (Some(column), None, _) => IndexPart {
            column: Some(column),
            expression: None,
            descending,
        },
        (None, _, expression) => IndexPart {
            column: None,
            expression: Some(expression.map(|e| unescape_catalog_text(&e)).unwrap_or_default()),
            descending,
        },
    }
}

/// A view's options from its `VIEWS` row: `sqlSecurity` (`DEFINER`, the default, or `INVOKER`), `checkOption`
/// (`CASCADED` or `LOCAL`; `NONE` is left out) and `definer` (`user@host`, shown but never compared or scripted).
pub(super) fn view_options(security: Option<String>, check: Option<String>, definer: Option<String>) -> Vec<(String, String)> {
    let mut options = Vec::new();
    if let Some(security) = security.map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty()) {
        options.push(("sqlSecurity".to_owned(), security));
    }
    if let Some(check) = check
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| !c.is_empty() && c != "NONE")
    {
        options.push(("checkOption".to_owned(), check));
    }
    if let Some(definer) = definer.filter(|d| !d.trim().is_empty()) {
        options.push(("definer".to_owned(), definer));
    }
    options
}

/// The tables and views of the database a view body names (bodies are stored fully qualified).
pub(super) fn view_dependencies(definition: &str, database: &str, names: &[String], view: &str) -> Vec<String> {
    let lower = definition.to_lowercase();
    let qualifier = format!("`{}`.", database.replace('`', "``")).to_lowercase();
    let mut found: Vec<String> = names
        .iter()
        .filter(|name| name.as_str() != view)
        .filter(|name| lower.contains(&format!("{qualifier}`{}`", name.replace('`', "``").to_lowercase())))
        .cloned()
        .collect();
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_become_sql_on_both_flavours() {
        let mysql = |raw: Option<&str>, extra: &str, data_type: &str| column_default(raw.map(str::to_owned), extra, data_type, false);
        let mariadb = |raw: Option<&str>, extra: &str, data_type: &str| column_default(raw.map(str::to_owned), extra, data_type, true);
        assert_eq!(mysql(Some("new"), "", "varchar").as_deref(), Some("'new'"));
        assert_eq!(mysql(Some("it's"), "", "varchar").as_deref(), Some("'it''s'"));
        assert_eq!(mysql(Some(""), "", "char").as_deref(), Some("''"));
        assert_eq!(mysql(Some("0.00"), "", "decimal").as_deref(), Some("0.00"));
        assert_eq!(mysql(Some("b'1'"), "", "bit").as_deref(), Some("b'1'"));
        assert_eq!(mysql(None, "", "varchar"), None);
        assert_eq!(
            mysql(Some("CURRENT_TIMESTAMP"), "DEFAULT_GENERATED", "timestamp").as_deref(),
            Some("CURRENT_TIMESTAMP")
        );
        assert_eq!(
            mysql(
                Some("CURRENT_TIMESTAMP"),
                "DEFAULT_GENERATED on update CURRENT_TIMESTAMP",
                "timestamp"
            )
            .as_deref(),
            Some("CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP")
        );
        assert_eq!(
            mysql(None, "on update CURRENT_TIMESTAMP(3)", "datetime").as_deref(),
            Some("NULL ON UPDATE CURRENT_TIMESTAMP(3)")
        );
        assert_eq!(mysql(Some("uuid()"), "DEFAULT_GENERATED", "char").as_deref(), Some("uuid()"));
        assert_eq!(mariadb(Some("'new'"), "", "varchar").as_deref(), Some("'new'"));
        assert_eq!(mariadb(Some("NULL"), "", "varchar"), None);
        assert_eq!(mariadb(Some("0"), "", "int").as_deref(), Some("0"));
        assert_eq!(
            mariadb(Some("current_timestamp()"), "on update current_timestamp()", "timestamp").as_deref(),
            Some("current_timestamp() ON UPDATE current_timestamp()")
        );
    }

    #[test]
    fn escaped_catalog_text_is_unescaped_only_when_every_quote_is_escaped() {
        assert_eq!(unescape_catalog_text("concat(_utf8mb4\\'a\\',`b`)"), "concat(_utf8mb4'a',`b`)");
        assert_eq!(unescape_catalog_text("`a` = 'x\\'y'"), "`a` = 'x\\'y'");
        assert_eq!(unescape_catalog_text("`qty` > 0"), "`qty` > 0");
    }

    #[test]
    fn json_checks_and_system_named_checks_are_recognized() {
        assert!(is_json_check("meta", "json_valid(`meta`)"));
        assert!(!is_json_check("meta", "json_valid(`other`)"));
        assert!(system_named_check("orders_chk_1", "orders", false));
        assert!(!system_named_check("orders_qty_check", "orders", false));
        assert!(system_named_check("CONSTRAINT_1", "orders", true));
        assert!(!system_named_check("orders_chk_1", "orders", true));
    }

    #[test]
    fn view_dependencies_come_from_qualified_names() {
        let names = vec!["orders".to_owned(), "customers".to_owned(), "v".to_owned(), "order".to_owned()];
        let body = "select `shop`.`orders`.`id` AS `id` from (`shop`.`orders` join `shop`.`customers`)";
        assert_eq!(view_dependencies(body, "shop", &names, "v"), ["customers", "orders"]);
    }
}
