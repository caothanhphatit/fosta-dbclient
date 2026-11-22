//! The schema compare snapshot of one attached SQLite database (`main` or an `ATTACH`ed alias). SQLite is a local
//! file, so the reader walks the tables one by one with the `pragma_*` table-valued functions:
//!
//! - `pragma_table_list` (kind, `WITHOUT ROWID`, `STRICT`) and `sqlite_schema` (the stored CREATE text),
//! - `pragma_table_xinfo` for columns (declared type, `NOT NULL`, default text, primary key position, generated),
//! - `pragma_index_list` / `pragma_index_xinfo` for unique constraints and indexes (expressions, `DESC`, partial),
//! - `pragma_foreign_key_list` for foreign keys.
//!
//! What the pragmas leave out is read from the CREATE text: CHECK constraints, column collations, generated column
//! expressions, `AUTOINCREMENT` and the names of constraints (`CONSTRAINT x ...`). The text is kept as written (raw);
//! the dialect normalizes it at diff time.
//!
//! A table's `native_ddl` is its CREATE TABLE statement followed by the CREATE TRIGGER statements of its triggers, each
//! ending with `;`: a rebuild drops the table with its triggers and creates them again from there. Triggers themselves
//! are listed as not compared.

use std::collections::HashMap;

use rusqlite::Connection;

use super::{map_err, SqliteDialect};
use crate::{
    contract::{
        CheckDef, ColumnDef, ForeignKeyDef, GeneratedDef, IdentityDef, IdentityKind, IndexDef, IndexPart, KeyDef, SchemaSnapshot,
        SkippedObject, SnapshotScope, TableDef, ViewDef,
    },
    engines::common::{
        split::{self, Dialect, Token, TokenKind},
        sqlbuild::SqlDialect,
        EngineError, Result,
    },
};

const NOT_COMPARED: &str = "Not compared in this version";

/// Reads the snapshot of the attached database `scope.database` (`main` when empty).
pub fn snapshot(c: &Connection, scope: &SnapshotScope, version: &str) -> Result<SchemaSnapshot> {
    let wanted = Some(scope.database.trim()).filter(|d| !d.is_empty()).unwrap_or("main");
    let databases: Vec<String> = query(c, "SELECT name FROM pragma_database_list ORDER BY seq", &[], |r| r.get(0))?;
    let schema = databases
        .into_iter()
        .find(|name| name.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| EngineError::input(format!("There is no attached database named '{wanted}'.")))?;
    let quoted = SqliteDialect.quote_ident(&schema);

    let mut skipped: Vec<SkippedObject> = Vec::new();
    let skip = |skipped: &mut Vec<SkippedObject>, kind: &str, name: &str, reason: &str| {
        skipped.push(SkippedObject {
            kind: kind.to_owned(),
            name: name.to_owned(),
            reason: reason.to_owned(),
        })
    };

    // Kinds and flags of the tables (`table`, `view`, `virtual`, `shadow`).
    let listed: Vec<(String, String, bool, bool)> = query(
        c,
        "SELECT name, type, wr, strict FROM pragma_table_list WHERE schema = ?1 ORDER BY name",
        &[&schema],
        |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? == 1, r.get::<_, i64>(3)? == 1)),
    )?;
    let kinds: HashMap<String, (String, bool, bool)> = listed
        .into_iter()
        .map(|(name, kind, without_rowid, strict)| (name, (kind, without_rowid, strict)))
        .collect();

    let objects: Vec<(String, String, String, Option<String>)> = query(
        c,
        &format!(
            "SELECT type, name, tbl_name, sql FROM {quoted}.sqlite_schema \
             WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name"
        ),
        &[],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut triggers: HashMap<String, Vec<String>> = HashMap::new();
    for (kind, name, table, sql) in &objects {
        if kind == "trigger" {
            skip(&mut skipped, "trigger", name, NOT_COMPARED);
            if let Some(sql) = sql {
                triggers.entry(table.to_lowercase()).or_default().push(sql.clone());
            }
        }
    }

    let mut tables = Vec::new();
    let mut views = Vec::new();
    for (kind, name, _, sql) in &objects {
        let sql = sql.clone().unwrap_or_default();
        match kind.as_str() {
            "table" => {
                let (list_kind, without_rowid, strict) = kinds.get(name).cloned().unwrap_or_else(|| ("table".into(), false, false));
                match list_kind.as_str() {
                    "virtual" => skip(&mut skipped, "virtual table", name, NOT_COMPARED),
                    // The storage of a virtual table, created and dropped with it.
                    "shadow" => {}
                    _ => {
                        let mut table = read_table(c, &schema, name, &sql)?;
                        if without_rowid {
                            table.options.push(("withoutRowid".to_owned(), "true".to_owned()));
                        }
                        if strict {
                            table.options.push(("strict".to_owned(), "true".to_owned()));
                        }
                        let mut native = format!("{};", sql.trim().trim_end_matches(';'));
                        for trigger in triggers.get(&name.to_lowercase()).into_iter().flatten() {
                            native.push('\n');
                            native.push_str(trigger.trim().trim_end_matches(';'));
                            native.push(';');
                        }
                        table.native_ddl = Some(native);
                        tables.push(table);
                    }
                }
            }
            "view" => views.push(ViewDef {
                name: name.clone(),
                definition: view_body(&sql),
                ..ViewDef::default()
            }),
            _ => {}
        }
    }

    // A foreign key that names no parent columns refers to the parent's primary key.
    let primary_keys: HashMap<String, Vec<String>> = tables
        .iter()
        .filter_map(|t| t.primary_key.as_ref().map(|k| (t.name.to_lowercase(), k.columns.clone())))
        .collect();
    for table in &mut tables {
        for key in &mut table.foreign_keys {
            if key.ref_columns.iter().all(String::is_empty) {
                if let Some(columns) = primary_keys.get(&key.ref_table.to_lowercase()) {
                    key.ref_columns = columns.clone();
                }
            }
        }
    }

    let mut names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
    names.extend(views.iter().map(|v| v.name.clone()));
    for view in &mut views {
        view.depends_on = view_dependencies(&view.definition, &names, &view.name);
    }

    Ok(SchemaSnapshot {
        engine: super::descriptor::ID.to_owned(),
        server_version: version.to_owned(),
        // The attached database is the scope; naming it as the schema too lets the dialect qualify statements with it.
        scope: SnapshotScope {
            database: schema.clone(),
            schema: Some(schema),
        },
        case_insensitive_names: true,
        tables,
        views,
        skipped,
        read_at: chrono::Utc::now().to_rfc3339(),
    })
}

fn query<T>(
    c: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let mut statement = c.prepare(sql).map_err(map_err)?;
    let rows = statement.query_map(params, map).map_err(map_err)?;
    rows.collect::<std::result::Result<_, _>>().map_err(map_err)
}

/// One table from the pragmas and its CREATE text.
fn read_table(c: &Connection, schema: &str, name: &str, sql: &str) -> Result<TableDef> {
    let parsed = parse_create_table(sql);
    let mut table = TableDef::new(name);

    let columns: Vec<(String, String, bool, Option<String>, i64, i64)> = query(
        c,
        "SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?1, ?2) ORDER BY cid",
        &[&name, &schema],
        |r| {
            Ok((
                r.get(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                r.get::<_, i64>(2)? == 1,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        },
    )?;
    let mut primary: Vec<(i64, String)> = Vec::new();
    for (column, type_name, not_null, default, pk, hidden) in columns {
        if hidden == 1 {
            continue;
        }
        let found = parsed.column(&column);
        if pk > 0 {
            primary.push((pk, column.clone()));
        }
        let generated = match (hidden, found.and_then(|p| p.generated.clone())) {
            (2 | 3, Some(expression)) => Some(GeneratedDef {
                expression,
                stored: hidden == 3,
            }),
            _ => None,
        };
        table.columns.push(ColumnDef {
            type_name: declared_type(&type_name),
            default: default.filter(|d| !d.eq_ignore_ascii_case("NULL")),
            identity: found.filter(|p| p.autoincrement).map(|_| IdentityDef {
                kind: IdentityKind::AutoIncrement,
                start: None,
                increment: None,
            }),
            generated,
            collation: found.and_then(|p| p.collation.clone()),
            ..ColumnDef::new(&column, "", !not_null)
        });
    }
    primary.sort();
    if !primary.is_empty() {
        table.primary_key = Some(KeyDef {
            system_named: parsed.primary_key_name.is_none(),
            name: parsed.primary_key_name.clone(),
            columns: primary.into_iter().map(|(_, column)| column).collect(),
        });
    }

    let indexes: Vec<(String, bool, String, bool)> = query(
        c,
        "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1, ?2) ORDER BY seq DESC",
        &[&name, &schema],
        |r| Ok((r.get(0)?, r.get::<_, i64>(1)? == 1, r.get(2)?, r.get::<_, i64>(3)? == 1)),
    )?;
    let mut uniques_used = vec![false; parsed.uniques.len()];
    for (index, unique, origin, partial) in indexes {
        let keys: Vec<(i64, Option<String>, bool)> = query(
            c,
            "SELECT cid, name, \"desc\" FROM pragma_index_xinfo(?1, ?2) WHERE key = 1 ORDER BY seqno",
            &[&index, &schema],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? == 1)),
        )?;
        match origin.as_str() {
            "pk" => {}
            "u" => {
                let columns: Vec<String> = keys.iter().map(|(_, column, _)| column.clone().unwrap_or_default()).collect();
                let found = parsed
                    .uniques
                    .iter()
                    .enumerate()
                    .position(|(at, (_, parsed_columns))| !uniques_used[at] && same_names(parsed_columns, &columns));
                let name = found.and_then(|at| {
                    uniques_used[at] = true;
                    parsed.uniques[at].0.clone()
                });
                table.uniques.push(KeyDef {
                    system_named: name.is_none(),
                    name,
                    columns,
                });
            }
            _ => {
                let index_sql: Option<String> = c
                    .query_row(
                        &format!(
                            "SELECT sql FROM {}.sqlite_schema WHERE type = 'index' AND name = ?1",
                            SqliteDialect.quote_ident(schema)
                        ),
                        [&index],
                        |r| r.get(0),
                    )
                    .map_err(map_err)?;
                let written = index_sql.as_deref().map(parse_create_index).unwrap_or_default();
                let parts = keys
                    .iter()
                    .enumerate()
                    .map(|(at, (cid, column, descending))| match (cid, column) {
                        (-2, _) => IndexPart {
                            column: None,
                            expression: Some(written.parts.get(at).cloned().unwrap_or_default()),
                            descending: *descending,
                        },
                        (_, Some(column)) => IndexPart {
                            descending: *descending,
                            ..IndexPart::column(column)
                        },
                        _ => IndexPart {
                            descending: *descending,
                            ..IndexPart::column("rowid")
                        },
                    })
                    .collect();
                table.indexes.push(IndexDef {
                    name: index,
                    parts,
                    unique,
                    method: None,
                    predicate: if partial { written.predicate } else { None },
                    include: Vec::new(),
                    system_named: false,
                });
            }
        }
    }
    table.indexes.sort_by_key(|index| index.name.to_lowercase());

    // Foreign keys come in reverse order of declaration: put them back in order to pair them with their names.
    let rows: Vec<(i64, String, String, Option<String>, String, String)> = query(
        c,
        "SELECT id, \"table\", \"from\", \"to\", on_update, on_delete FROM pragma_foreign_key_list(?1, ?2) ORDER BY id DESC, seq",
        &[&name, &schema],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    )?;
    let mut keys_used = vec![false; parsed.foreign_keys.len()];
    let mut last = None;
    for (id, parent, from, to, on_update, on_delete) in rows {
        if last != Some(id) {
            last = Some(id);
            let action = |rule: String| Some(rule).filter(|r| !r.eq_ignore_ascii_case("NO ACTION"));
            table.foreign_keys.push(ForeignKeyDef {
                ref_table: parent,
                on_update: action(on_update),
                on_delete: action(on_delete),
                ..ForeignKeyDef::default()
            });
        }
        let key = table.foreign_keys.last_mut().expect("pushed above");
        key.columns.push(from);
        key.ref_columns.push(to.unwrap_or_default());
    }
    for key in &mut table.foreign_keys {
        let found = parsed.foreign_keys.iter().enumerate().position(|(at, (_, columns, parent))| {
            !keys_used[at] && same_names(columns, &key.columns) && parent.eq_ignore_ascii_case(&key.ref_table)
        });
        if let Some(at) = found {
            keys_used[at] = true;
            key.name = parsed.foreign_keys[at].0.clone();
        }
        key.system_named = key.name.is_none();
    }

    table.checks = parsed
        .checks
        .iter()
        .map(|(name, expression)| CheckDef {
            name: name.clone(),
            expression: expression.clone(),
            system_named: name.is_none(),
        })
        .collect();
    Ok(table)
}

fn same_names(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// The declared type as `pragma_table_xinfo` reports it, without the `GENERATED ALWAYS` some versions leave on a
/// generated column's type.
fn declared_type(raw: &str) -> String {
    let trimmed = raw.trim();
    let upper = trimmed.to_ascii_uppercase();
    match upper.strip_suffix("GENERATED ALWAYS") {
        Some(rest) => trimmed[..rest.len()].trim_end().to_owned(),
        None => trimmed.to_owned(),
    }
}

// --- CREATE text --------------------------------------------------------------------------------------------------------

/// What the CREATE TABLE text says beyond the pragmas.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ParsedTable {
    pub columns: Vec<ParsedColumn>,
    pub primary_key_name: Option<String>,
    /// (name, columns) in the order written.
    pub uniques: Vec<(Option<String>, Vec<String>)>,
    /// (name, expression) of the column and table CHECKs, in the order written.
    pub checks: Vec<(Option<String>, String)>,
    /// (name, columns, parent table) in the order written.
    pub foreign_keys: Vec<(Option<String>, Vec<String>, String)>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ParsedColumn {
    pub name: String,
    pub collation: Option<String>,
    pub generated: Option<String>,
    pub autoincrement: bool,
}

impl ParsedTable {
    fn column(&self, name: &str) -> Option<&ParsedColumn> {
        self.columns.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }
}

/// An identifier without its quotes (`"a""b"`, `[a]`, `` `a` ``, `'a'`).
pub(super) fn unquote(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    let (Some(first), Some(last)) = (chars.next(), text.chars().last()) else {
        return text.to_owned();
    };
    match (first, last) {
        ('"', '"') | ('`', '`') | ('\'', '\'') if text.len() >= 2 => {
            let inner = &text[1..text.len() - 1];
            inner.replace(&format!("{first}{first}"), &first.to_string())
        }
        ('[', ']') if text.len() >= 2 => text[1..text.len() - 1].to_owned(),
        _ => text.to_owned(),
    }
}

/// The index of the token that closes the parenthesis opened at `open`.
fn closing(tokens: &[Token], open: usize) -> usize {
    let mut depth = 0;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        match token.kind {
            TokenKind::Punct('(') => depth += 1,
            TokenKind::Punct(')') => {
                depth -= 1;
                if depth == 0 {
                    return at;
                }
            }
            _ => {}
        }
    }
    tokens.len().saturating_sub(1)
}

/// The text between the parenthesis at `open` and its closing one.
fn inside(src: &str, tokens: &[Token], open: usize) -> (String, usize) {
    let close = closing(tokens, open);
    let start = tokens[open].end;
    let end = if close > open { tokens[close].start } else { src.len() };
    (src[start..end.max(start)].trim().to_owned(), close)
}

/// Splits `tokens[from..to]` at the commas outside parentheses.
fn split_top_level(tokens: &[Token], from: usize, to: usize) -> Vec<(usize, usize)> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0, from);
    for (at, token) in tokens.iter().enumerate().take(to).skip(from) {
        match token.kind {
            TokenKind::Punct('(') => depth += 1,
            TokenKind::Punct(')') => depth -= 1,
            TokenKind::Punct(',') if depth == 0 => {
                parts.push((start, at));
                start = at + 1;
            }
            _ => {}
        }
    }
    if start < to {
        parts.push((start, to));
    }
    parts
}

/// The names in a parenthesized list (`(a, "b" DESC, c COLLATE x)`): the first token of each entry.
fn name_list(src: &str, tokens: &[Token], open: usize) -> (Vec<String>, usize) {
    let close = closing(tokens, open);
    let names = split_top_level(tokens, open + 1, close)
        .into_iter()
        .map(|(from, _)| unquote(tokens[from].text(src)))
        .collect();
    (names, close)
}

const CONSTRAINT_WORDS: [&str; 11] = [
    "CONSTRAINT",
    "PRIMARY",
    "NOT",
    "NULL",
    "UNIQUE",
    "CHECK",
    "DEFAULT",
    "COLLATE",
    "REFERENCES",
    "GENERATED",
    "AS",
];

/// Reads the column and table constraints of a CREATE TABLE statement. Unknown syntax is skipped, never an error.
pub(super) fn parse_create_table(sql: &str) -> ParsedTable {
    let tokens = split::tokenize(Dialect::Sqlite, sql);
    let mut parsed = ParsedTable::default();
    let Some(open) = tokens.iter().position(|t| t.kind == TokenKind::Punct('(')) else {
        return parsed;
    };
    let close = closing(&tokens, open);
    for (from, to) in split_top_level(&tokens, open + 1, close) {
        let first = &tokens[from];
        let mut at = from;
        let mut name: Option<String> = None;
        if first.is_word(sql, "CONSTRAINT") && from + 1 < to {
            name = Some(unquote(tokens[from + 1].text(sql)));
            at = from + 2;
        }
        let Some(head) = tokens.get(at).filter(|_| at < to) else { continue };
        let keyword = if head.kind == TokenKind::Word {
            head.text(sql).to_ascii_uppercase()
        } else {
            String::new()
        };
        match keyword.as_str() {
            "PRIMARY" => parsed.primary_key_name = name,
            "UNIQUE" => {
                if let Some(open) = (at..to).find(|&i| tokens[i].kind == TokenKind::Punct('(')) {
                    parsed.uniques.push((name, name_list(sql, &tokens, open).0));
                }
            }
            "CHECK" => {
                if let Some(open) = (at..to).find(|&i| tokens[i].kind == TokenKind::Punct('(')) {
                    parsed.checks.push((name, inside(sql, &tokens, open).0));
                }
            }
            "FOREIGN" => {
                if let Some(open) = (at..to).find(|&i| tokens[i].kind == TokenKind::Punct('(')) {
                    let (columns, close) = name_list(sql, &tokens, open);
                    let parent = (close..to)
                        .find(|&i| tokens[i].is_word(sql, "REFERENCES"))
                        .and_then(|i| tokens.get(i + 1))
                        .map(|t| unquote(t.text(sql)))
                        .unwrap_or_default();
                    parsed.foreign_keys.push((name, columns, parent));
                }
            }
            _ => parse_column(sql, &tokens, from, to, &mut parsed),
        }
    }
    parsed
}

fn parse_column(sql: &str, tokens: &[Token], from: usize, to: usize, parsed: &mut ParsedTable) {
    let column = unquote(tokens[from].text(sql));
    let mut out = ParsedColumn {
        name: column.clone(),
        ..ParsedColumn::default()
    };
    let mut name: Option<String> = None;
    let mut at = from + 1;
    // The type: words and parenthesized sizes up to the first constraint keyword.
    while at < to {
        let token = &tokens[at];
        if token.kind == TokenKind::Word && CONSTRAINT_WORDS.contains(&token.text(sql).to_ascii_uppercase().as_str()) {
            break;
        }
        at = if token.kind == TokenKind::Punct('(') {
            closing(tokens, at) + 1
        } else {
            at + 1
        };
    }
    while at < to {
        let token = &tokens[at];
        let word = if token.kind == TokenKind::Word {
            token.text(sql).to_ascii_uppercase()
        } else {
            String::new()
        };
        let next_open = (at + 1 < to && tokens[at + 1].kind == TokenKind::Punct('(')).then_some(at + 1);
        match word.as_str() {
            "CONSTRAINT" if at + 1 < to => {
                name = Some(unquote(tokens[at + 1].text(sql)));
                at += 2;
                continue;
            }
            "PRIMARY" => parsed.primary_key_name = name.take(),
            "AUTOINCREMENT" => out.autoincrement = true,
            "UNIQUE" => parsed.uniques.push((name.take(), vec![column.clone()])),
            "CHECK" => {
                if let Some(open) = next_open {
                    let (expression, close) = inside(sql, tokens, open);
                    parsed.checks.push((name.take(), expression));
                    at = close + 1;
                    continue;
                }
            }
            "COLLATE" if at + 1 < to => {
                out.collation = Some(unquote(tokens[at + 1].text(sql)));
                name = None;
                at += 2;
                continue;
            }
            "REFERENCES" if at + 1 < to => {
                parsed
                    .foreign_keys
                    .push((name.take(), vec![column.clone()], unquote(tokens[at + 1].text(sql))));
                at += 2;
                continue;
            }
            "AS" => {
                if let Some(open) = next_open {
                    let (expression, close) = inside(sql, tokens, open);
                    out.generated = Some(expression);
                    name = None;
                    at = close + 1;
                    continue;
                }
            }
            "NOT" | "NULL" | "DEFAULT" | "GENERATED" => name = None,
            _ => {}
        }
        at = if token.kind == TokenKind::Punct('(') {
            closing(tokens, at) + 1
        } else {
            at + 1
        };
    }
    parsed.columns.push(out);
}

/// The key parts (as written, without `ASC`/`DESC` and `COLLATE`) and the `WHERE` predicate of a CREATE INDEX.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ParsedIndex {
    pub parts: Vec<String>,
    pub predicate: Option<String>,
}

pub(super) fn parse_create_index(sql: &str) -> ParsedIndex {
    let tokens = split::tokenize(Dialect::Sqlite, sql);
    let Some(on) = tokens.iter().position(|t| t.is_word(sql, "ON")) else {
        return ParsedIndex::default();
    };
    let Some(open) = tokens.iter().skip(on).position(|t| t.kind == TokenKind::Punct('(')).map(|i| i + on) else {
        return ParsedIndex::default();
    };
    let close = closing(&tokens, open);
    let parts = split_top_level(&tokens, open + 1, close)
        .into_iter()
        .map(|(from, to)| {
            // Trailing ASC/DESC and COLLATE x belong to the key, not to the expression.
            let mut end = to;
            if end > from + 1 && (tokens[end - 1].is_word(sql, "ASC") || tokens[end - 1].is_word(sql, "DESC")) {
                end -= 1;
            }
            if end > from + 2 && tokens[end - 2].is_word(sql, "COLLATE") {
                end -= 2;
            }
            sql[tokens[from].start..tokens[end - 1].end].trim().to_owned()
        })
        .collect();
    let predicate = tokens
        .iter()
        .skip(close)
        .position(|t| t.is_word(sql, "WHERE"))
        .map(|at| sql[tokens[close + at].end..].trim().trim_end_matches(';').trim().to_owned())
        .filter(|p| !p.is_empty());
    ParsedIndex { parts, predicate }
}

/// The body of a CREATE VIEW (after the first `AS` outside parentheses).
pub(super) fn view_body(sql: &str) -> String {
    let tokens = split::tokenize(Dialect::Sqlite, sql);
    let mut depth = 0;
    for token in &tokens {
        match token.kind {
            TokenKind::Punct('(') => depth += 1,
            TokenKind::Punct(')') => depth -= 1,
            _ if depth == 0 && token.is_word(sql, "AS") => {
                return sql[token.end..].trim().trim_end_matches(';').trim_end().to_owned();
            }
            _ => {}
        }
    }
    sql.trim().to_owned()
}

/// The tables and views of the scope a view body names (SQLite views read only their own database).
pub(super) fn view_dependencies(body: &str, names: &[String], view: &str) -> Vec<String> {
    let tokens = split::tokenize(Dialect::Sqlite, body);
    let mut found: Vec<String> = Vec::new();
    for token in &tokens {
        if !matches!(token.kind, TokenKind::Word | TokenKind::QuotedIdent) {
            continue;
        }
        let word = unquote(token.text(body));
        if let Some(name) = names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(&word) && !n.eq_ignore_ascii_case(view))
        {
            if !found.contains(name) {
                found.push(name.clone());
            }
        }
    }
    found.sort_by_key(|n| n.to_lowercase());
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constraints_come_from_the_create_text() {
        let parsed = parse_create_table(
            "CREATE TABLE \"orders\" (\n  id INTEGER CONSTRAINT pk_orders PRIMARY KEY AUTOINCREMENT,\n  \
             code VARCHAR(20) NOT NULL COLLATE NOCASE UNIQUE,\n  qty INT DEFAULT (1 + 1) CHECK (qty > 0),\n  \
             [total] REAL GENERATED ALWAYS AS (qty * 2.5) STORED,\n  half REAL AS (qty / 2),\n  \
             customer_id INTEGER CONSTRAINT fk_customer REFERENCES customers (id) ON DELETE CASCADE,\n  \
             CONSTRAINT uq_code_qty UNIQUE (code, \"qty\"),\n  CONSTRAINT ck_total CHECK (total >= 0 AND code <> 'x,(y'),\n  \
             FOREIGN KEY (code) REFERENCES \"codes\" (code)\n) STRICT",
        );
        assert_eq!(parsed.primary_key_name.as_deref(), Some("pk_orders"));
        let id = parsed.column("ID").unwrap();
        assert!(id.autoincrement);
        assert_eq!(parsed.column("code").unwrap().collation.as_deref(), Some("NOCASE"));
        assert_eq!(parsed.column("total").unwrap().generated.as_deref(), Some("qty * 2.5"));
        assert_eq!(parsed.column("half").unwrap().generated.as_deref(), Some("qty / 2"));
        assert_eq!(
            parsed.uniques,
            vec![
                (None, vec!["code".to_owned()]),
                (Some("uq_code_qty".to_owned()), vec!["code".to_owned(), "qty".to_owned()])
            ]
        );
        assert_eq!(
            parsed.checks,
            vec![
                (None, "qty > 0".to_owned()),
                (Some("ck_total".to_owned()), "total >= 0 AND code <> 'x,(y'".to_owned())
            ]
        );
        assert_eq!(
            parsed.foreign_keys,
            vec![
                (
                    Some("fk_customer".to_owned()),
                    vec!["customer_id".to_owned()],
                    "customers".to_owned()
                ),
                (None, vec!["code".to_owned()], "codes".to_owned())
            ]
        );
        assert_eq!(parsed.columns.len(), 6);
    }

    #[test]
    fn indexes_views_and_names_are_read_from_text() {
        let index =
            parse_create_index("CREATE UNIQUE INDEX ix ON t (lower(name) COLLATE NOCASE DESC, \"b\" ASC) WHERE b > 0 AND (c IS NOT NULL)");
        assert_eq!(index.parts, ["lower(name)", "\"b\""]);
        assert_eq!(index.predicate.as_deref(), Some("b > 0 AND (c IS NOT NULL)"));
        assert_eq!(parse_create_index("CREATE INDEX ix ON t (a)").predicate, None);
        assert_eq!(
            view_body("CREATE VIEW v (a, b) AS SELECT x AS a, y AS b FROM t;"),
            "SELECT x AS a, y AS b FROM t"
        );
        let names = vec!["Orders".to_owned(), "items".to_owned(), "v".to_owned()];
        assert_eq!(
            view_dependencies("SELECT o.id FROM orders o JOIN \"items\" i ON 'v' = i.x", &names, "v"),
            ["items", "Orders"]
        );
        assert_eq!(unquote("\"a\"\"b\""), "a\"b");
        assert_eq!(unquote("[x y]"), "x y");
        assert_eq!(unquote("`t`"), "t");
        assert_eq!(declared_type("INT GENERATED ALWAYS"), "INT");
        assert_eq!(declared_type("varchar(10)"), "varchar(10)");
    }

    #[test]
    fn a_snapshot_reads_tables_views_and_triggers_of_an_attached_database() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS shop;
             CREATE TABLE shop.customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL COLLATE NOCASE, CONSTRAINT uq_name UNIQUE (name));
             CREATE TABLE shop.orders (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               customer_id INTEGER NOT NULL REFERENCES customers ON DELETE CASCADE,
               qty INT NOT NULL DEFAULT 1 CHECK (qty > 0),
               total REAL GENERATED ALWAYS AS (qty * 2.5) VIRTUAL,
               CONSTRAINT fk_self FOREIGN KEY (id) REFERENCES orders (id)
             );
             CREATE INDEX shop.ix_orders_qty ON orders (qty DESC, lower(customer_id)) WHERE qty > 1;
             CREATE TABLE shop.tags (k TEXT PRIMARY KEY, v BLOB) WITHOUT ROWID, STRICT;
             CREATE VIEW shop.v_orders AS SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id;
             CREATE TRIGGER shop.tr_orders AFTER INSERT ON orders BEGIN UPDATE orders SET qty = qty WHERE id = new.id; END;",
        )
        .unwrap();
        let snap = snapshot(&c, &SnapshotScope::new("SHOP", None), "3.50.4").unwrap();
        assert_eq!(snap.scope, SnapshotScope::new("shop", Some("shop")));
        assert!(snap.case_insensitive_names);
        let names: Vec<&str> = snap.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["customers", "orders", "tags"]);
        let customers = &snap.tables[0];
        assert_eq!(customers.columns[1].collation.as_deref(), Some("NOCASE"));
        assert_eq!(customers.uniques[0].name.as_deref(), Some("uq_name"));
        assert!(customers.primary_key.as_ref().unwrap().system_named);
        let orders = &snap.tables[1];
        assert_eq!(
            orders.columns[0].identity.as_ref().map(|i| i.kind),
            Some(IdentityKind::AutoIncrement)
        );
        assert_eq!(orders.columns[2].default.as_deref(), Some("1"));
        assert_eq!(
            orders.columns[3].generated.as_ref().map(|g| (g.expression.as_str(), g.stored)),
            Some(("qty * 2.5", false))
        );
        assert_eq!(orders.columns[3].type_name, "REAL");
        assert_eq!(orders.checks[0].expression, "qty > 0");
        assert_eq!(orders.foreign_keys.len(), 2);
        assert_eq!(orders.foreign_keys[0].ref_table, "customers");
        assert_eq!(orders.foreign_keys[0].ref_columns, ["id"]);
        assert_eq!(orders.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
        assert_eq!(orders.foreign_keys[1].name.as_deref(), Some("fk_self"));
        let index = &orders.indexes[0];
        assert_eq!(index.name, "ix_orders_qty");
        assert!(index.parts[0].descending && index.parts[0].column.as_deref() == Some("qty"));
        assert_eq!(index.parts[1].expression.as_deref(), Some("lower(customer_id)"));
        assert_eq!(index.predicate.as_deref(), Some("qty > 1"));
        let native = orders.native_ddl.as_deref().unwrap();
        assert!(
            native.starts_with("CREATE TABLE orders") && native.contains("CREATE TRIGGER tr_orders"),
            "{native}"
        );
        assert_eq!(
            snap.tables[2].options,
            [
                ("withoutRowid".to_owned(), "true".to_owned()),
                ("strict".to_owned(), "true".to_owned())
            ]
        );
        assert_eq!(
            snap.views[0].definition,
            "SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id"
        );
        assert_eq!(snap.views[0].depends_on, ["customers", "orders"]);
        assert_eq!(snap.skipped[0].kind, "trigger");
        assert!(snapshot(&c, &SnapshotScope::new("nope", None), "x").is_err());
    }
}
