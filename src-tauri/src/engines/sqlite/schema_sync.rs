//! The SQLite schema compare dialect: normalizes the CREATE text SQLite keeps, rates column type changes by type
//! affinity, writes canonical DDL and renders migration steps.
//!
//! SQLite's ALTER TABLE adds, drops and renames columns and nothing else, so every other change of a table (a column's
//! type, nullability, default or collation, keys, checks, foreign keys, `STRICT` / `WITHOUT ROWID`, column order) is a
//! rebuild, the procedure the SQLite documentation gives for it:
//!
//! 1. `PRAGMA foreign_keys = OFF` (outside the transaction: SQLite ignores it inside one),
//! 2. `BEGIN`, then per table: create `_fosta_new_<table>` as the source defines it, copy the common columns, drop the
//!    old table (with its indexes and triggers), rename the new one, create its indexes and the old triggers again,
//! 3. a foreign key check that stops the script (integer overflow) before `COMMIT` when a row breaks a key,
//! 4. `COMMIT`, `PRAGMA foreign_keys = ON`.
//!
//! The planner drops and recreates the views that read a rebuilt table (a rename fails while a view is broken).
//! Foreign keys live in the table (`DialectFeatures::inline_foreign_keys`): `CREATE TABLE` carries them, and the keys of
//! other tables keep pointing at a rebuilt table by its name. The gateway classifies `PRAGMA foreign_keys = ...` as an
//! admin statement, so running a script with rebuilds or drops asks for that confirmation.

use super::schema_snapshot::unquote;
use crate::{
    contract::{
        replace_schema_qualifier, strip_schema_qualifier, Change, ChangeKind, ColumnDef, ConstraintDef, DialectFeatures, DiffItem,
        ForeignKeyDef, IdentityKind, IndexDef, IndexPart, KeyDef, MigrationStep, RenderContext, RenderedStatement, Risk, SchemaDialect,
        TableDef, ViewDef,
    },
    engines::common::split::{self, Dialect, Token, TokenKind},
};

pub struct SqliteSchemaDialect;

pub static SQLITE_SCHEMA_DIALECT: SqliteSchemaDialect = SqliteSchemaDialect;

/// The prefix of the table a rebuild creates before it takes the old table's name.
pub const REBUILD_PREFIX: &str = "_fosta_new_";

/// Stops the script with "integer overflow" when a row of the scope breaks a foreign key (`{schema}` is a literal).
const FOREIGN_KEY_GUARD: &str =
    "SELECT CASE WHEN EXISTS (SELECT 1 FROM pragma_foreign_key_check(NULL, {schema})) THEN abs(-9223372036854775808) END";

// --- Text helpers -------------------------------------------------------------------------------------------------------

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn qualified(schema: Option<&str>, name: &str) -> String {
    match schema {
        Some(schema) => format!("{}.{}", ident(schema), ident(name)),
        None => ident(name),
    }
}

fn idents(names: &[String]) -> String {
    names.iter().map(|n| ident(n)).collect::<Vec<_>>().join(", ")
}

fn wordish(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Word | TokenKind::QuotedIdent | TokenKind::Number | TokenKind::Str)
}

/// A canonical spelling for comparison: identifiers unquoted, words in one case (`upper` for types), string literals
/// as written, whitespace only between two words, comments dropped, and redundant outer parentheses removed.
fn canonical(text: &str, upper: bool) -> String {
    let tokens = split::tokenize(Dialect::Sqlite, text);
    let tokens = strip_outer_parens(&tokens);
    let mut out = String::with_capacity(text.len());
    let mut previous: Option<TokenKind> = None;
    for token in tokens {
        let raw = token.text(text);
        if previous.is_some_and(wordish) && wordish(token.kind) {
            out.push(' ');
        }
        match token.kind {
            TokenKind::Str => out.push_str(raw),
            TokenKind::QuotedIdent | TokenKind::Word => {
                let word = if token.kind == TokenKind::QuotedIdent {
                    unquote(raw)
                } else {
                    raw.to_owned()
                };
                out.push_str(&if upper { word.to_uppercase() } else { word.to_lowercase() });
            }
            _ => out.push_str(raw),
        }
        previous = Some(token.kind);
    }
    out
}

/// The tokens without parentheses that wrap all of them.
fn strip_outer_parens(mut tokens: &[Token]) -> &[Token] {
    while tokens.len() >= 2 && tokens[0].kind == TokenKind::Punct('(') && tokens[tokens.len() - 1].kind == TokenKind::Punct(')') {
        let mut depth = 0;
        let wraps = tokens.iter().enumerate().all(|(at, token)| {
            match token.kind {
                TokenKind::Punct('(') => depth += 1,
                TokenKind::Punct(')') => depth -= 1,
                _ => {}
            }
            depth > 0 || at == tokens.len() - 1
        });
        if !wraps {
            break;
        }
        tokens = &tokens[1..tokens.len() - 1];
    }
    tokens
}

/// Whether an expression names `column` (outside string literals).
fn mentions(expression: &str, column: &str) -> bool {
    split::tokenize(Dialect::Sqlite, expression).iter().any(|token| {
        matches!(token.kind, TokenKind::Word | TokenKind::QuotedIdent) && unquote(token.text(expression)).eq_ignore_ascii_case(column)
    })
}

/// Source-scope references pointed at the target scope (view bodies, expressions).
fn map_text(text: &str, ctx: &RenderContext) -> String {
    match (&ctx.source_schema, &ctx.target_schema) {
        (Some(source), Some(target)) if !source.eq_ignore_ascii_case(target) => {
            replace_schema_qualifier(text, source, Some(&ident(target)))
        }
        _ => text.to_owned(),
    }
}

// --- Types --------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Affinity {
    Integer,
    Text,
    Blob,
    Real,
    Numeric,
}

/// SQLite's type affinity rules (section 3.1 of "Datatypes In SQLite"), in their order.
fn affinity(declared: &str) -> Affinity {
    let upper = declared.to_ascii_uppercase();
    if upper.contains("INT") {
        Affinity::Integer
    } else if upper.contains("CHAR") || upper.contains("CLOB") || upper.contains("TEXT") {
        Affinity::Text
    } else if upper.contains("BLOB") || upper.trim().is_empty() {
        Affinity::Blob
    } else if upper.contains("REAL") || upper.contains("FLOA") || upper.contains("DOUB") {
        Affinity::Real
    } else {
        Affinity::Numeric
    }
}

// --- Rendering ----------------------------------------------------------------------------------------------------------

fn statement(sql: String) -> RenderedStatement {
    RenderedStatement::new(sql)
}

fn warned(sql: String, risk: Risk, warning: impl Into<String>) -> RenderedStatement {
    RenderedStatement {
        sql,
        risk,
        warnings: vec![warning.into()],
    }
}

fn manual(reason: &str, hint: Option<&str>) -> RenderedStatement {
    let mut sql = format!("-- MANUAL STEP: {}", reason.split_whitespace().collect::<Vec<_>>().join(" "));
    if let Some(hint) = hint {
        for line in hint.lines() {
            sql.push_str(&format!("\n--   {line}"));
        }
    }
    warned(sql, Risk::Review, format!("Manual step: {reason}"))
}

/// Whether a default may go after `DEFAULT` without parentheses: a literal, a signed number, NULL/TRUE/FALSE or a
/// current-time keyword.
fn bare_default(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    let number = value.strip_prefix(['+', '-']).unwrap_or(value);
    value.starts_with('\'')
        || (upper.starts_with("X'") && value.ends_with('\''))
        || (number.starts_with(|c: char| c.is_ascii_digit() || c == '.') && number.parse::<f64>().is_ok())
        || matches!(
            upper.as_str(),
            "NULL" | "TRUE" | "FALSE" | "CURRENT_TIME" | "CURRENT_DATE" | "CURRENT_TIMESTAMP"
        )
}

/// Whether ALTER TABLE ... ADD COLUMN accepts the default: a constant (no current-time keyword, no expression).
fn constant_default(value: &str) -> bool {
    bare_default(value) && !value.to_ascii_uppercase().starts_with("CURRENT_")
}

fn default_sql(default: &str) -> String {
    let value = default.trim();
    let tokens = split::tokenize(Dialect::Sqlite, value);
    let wrapped = !tokens.is_empty() && strip_outer_parens(&tokens).len() < tokens.len();
    if bare_default(value) || wrapped {
        value.to_owned()
    } else {
        format!("({value})")
    }
}

/// `"name" TYPE [PRIMARY KEY AUTOINCREMENT] [NOT NULL] [DEFAULT ...] [COLLATE ...] [GENERATED ALWAYS AS (...) ...]`.
fn column_sql(column: &ColumnDef, primary_key: Option<&KeyDef>) -> String {
    let mut sql = ident(&column.name);
    if !column.type_name.trim().is_empty() {
        sql.push(' ');
        sql.push_str(column.type_name.trim());
    }
    if let Some(key) = primary_key {
        if let Some(name) = key.name.as_deref().filter(|_| !key.system_named) {
            sql.push_str(&format!(" CONSTRAINT {}", ident(name)));
        }
        sql.push_str(" PRIMARY KEY AUTOINCREMENT");
    }
    if !column.nullable {
        sql.push_str(" NOT NULL");
    }
    if column.generated.is_none() {
        if let Some(default) = column.default.as_deref().filter(|d| !d.trim().is_empty()) {
            sql.push_str(&format!(" DEFAULT {}", default_sql(default)));
        }
    }
    if let Some(collation) = column.collation.as_deref().filter(|c| !c.is_empty()) {
        sql.push_str(&format!(" COLLATE {}", ident(collation)));
    }
    if let Some(generated) = &column.generated {
        let kind = if generated.stored { "STORED" } else { "VIRTUAL" };
        sql.push_str(&format!(" GENERATED ALWAYS AS ({}) {kind}", generated.expression.trim()));
    }
    sql
}

fn constraint_name(name: &Option<String>, system_named: bool) -> String {
    match name {
        Some(name) if !system_named => format!("CONSTRAINT {} ", ident(name)),
        _ => String::new(),
    }
}

fn foreign_key_clause(key: &ForeignKeyDef) -> String {
    let mut sql = format!(
        "{}FOREIGN KEY ({}) REFERENCES {}",
        constraint_name(&key.name, key.system_named),
        idents(&key.columns),
        ident(&key.ref_table)
    );
    if !key.ref_columns.iter().all(String::is_empty) {
        sql.push_str(&format!(" ({})", idents(&key.ref_columns)));
    }
    for (clause, action) in [("ON UPDATE", &key.on_update), ("ON DELETE", &key.on_delete)] {
        if let Some(action) = action.as_deref().filter(|a| !a.eq_ignore_ascii_case("NO ACTION")) {
            sql.push_str(&format!(" {clause} {}", action.to_uppercase()));
        }
    }
    sql
}

/// The single column of an `AUTOINCREMENT` primary key, written on the column (SQLite allows it nowhere else).
fn autoincrement_key(table: &TableDef) -> Option<(&str, &KeyDef)> {
    let key = table.primary_key.as_ref()?;
    let [column] = key.columns.as_slice() else { return None };
    table
        .columns
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(column))
        .filter(|c| c.identity.as_ref().is_some_and(|i| i.kind == IdentityKind::AutoIncrement))
        .map(|c| (c.name.as_str(), key))
}

fn option(table: &TableDef, name: &str) -> bool {
    table
        .options
        .iter()
        .any(|(key, value)| key.eq_ignore_ascii_case(name) && value.eq_ignore_ascii_case("true"))
}

/// The CREATE TABLE statement (no trailing `;`): columns, keys, checks and foreign keys, then `WITHOUT ROWID` and
/// `STRICT`.
fn create_table_sql(schema: Option<&str>, table: &TableDef) -> String {
    let inline = autoincrement_key(table);
    let mut lines: Vec<String> = table
        .columns
        .iter()
        .map(|column| {
            let key = inline
                .filter(|(name, _)| name.eq_ignore_ascii_case(&column.name))
                .map(|(_, key)| key);
            column_sql(column, key)
        })
        .collect();
    if let (Some(key), None) = (&table.primary_key, inline) {
        lines.push(format!(
            "{}PRIMARY KEY ({})",
            constraint_name(&key.name, key.system_named),
            idents(&key.columns)
        ));
    }
    lines.extend(
        table
            .uniques
            .iter()
            .map(|key| format!("{}UNIQUE ({})", constraint_name(&key.name, key.system_named), idents(&key.columns))),
    );
    lines.extend(table.checks.iter().map(|check| {
        format!(
            "{}CHECK ({})",
            constraint_name(&check.name, check.system_named),
            check.expression.trim()
        )
    }));
    lines.extend(table.foreign_keys.iter().map(foreign_key_clause));
    let mut sql = format!(
        "CREATE TABLE {} (\n    {}\n)",
        qualified(schema, &table.name),
        lines.join(",\n    ")
    );
    let options: Vec<&str> = [("withoutRowid", "WITHOUT ROWID"), ("strict", "STRICT")]
        .into_iter()
        .filter(|(name, _)| option(table, name))
        .map(|(_, sql)| sql)
        .collect();
    if !options.is_empty() {
        sql.push(' ');
        sql.push_str(&options.join(", "));
    }
    sql
}

fn index_part_sql(part: &IndexPart) -> String {
    let key = match (&part.column, &part.expression) {
        (Some(column), _) => ident(column),
        (None, Some(expression)) => expression.trim().to_owned(),
        (None, None) => String::new(),
    };
    if part.descending {
        format!("{key} DESC")
    } else {
        key
    }
}

/// `CREATE [UNIQUE] INDEX "schema"."name" ON "table" (...) [WHERE ...]`: SQLite names the schema on the index only.
fn index_sql(schema: Option<&str>, table: &str, index: &IndexDef) -> String {
    let mut sql = format!(
        "CREATE {}INDEX {} ON {} ({})",
        if index.unique { "UNIQUE " } else { "" },
        qualified(schema, &index.name),
        ident(table),
        index.parts.iter().map(index_part_sql).collect::<Vec<_>>().join(", ")
    );
    if let Some(predicate) = index.predicate.as_deref().filter(|p| !p.trim().is_empty()) {
        sql.push_str(&format!(" WHERE {}", predicate.trim()));
    }
    sql
}

/// The CREATE TRIGGER statements of a table's `native_ddl`, with the trigger qualified by `schema` (so it lands in
/// that database whatever the main database holds).
fn triggers_of(table: &TableDef, schema: Option<&str>) -> Vec<(String, String)> {
    let Some(native) = table.native_ddl.as_deref() else {
        return Vec::new();
    };
    split::split(Dialect::Sqlite, native)
        .into_iter()
        .filter_map(|statement| {
            let text = statement.text.trim().trim_end_matches(';').trim_end().to_owned();
            let tokens = split::tokenize(Dialect::Sqlite, &text);
            let at = tokens.iter().position(|t| t.is_word(&text, "TRIGGER"))?;
            if !tokens.first()?.is_word(&text, "CREATE") || at > 3 {
                return None;
            }
            let mut name_at = at + 1;
            if tokens.get(name_at).is_some_and(|t| t.is_word(&text, "IF")) {
                name_at += 3; // IF NOT EXISTS
            }
            let mut name_end = name_at;
            if tokens.get(name_at + 1).is_some_and(|t| t.kind == TokenKind::Punct('.')) {
                name_end = name_at + 2; // already qualified
            }
            let (first, last) = (tokens.get(name_at)?, tokens.get(name_end)?);
            let name = unquote(last.text(&text));
            let sql = format!("{}{}{}", &text[..first.start], qualified(schema, &name), &text[last.end..]);
            Some((name, sql))
        })
        .collect()
}

/// The schema of a `DROP TABLE "schema"."name"` statement (`Some(None)` when it names none), `None` for other text.
fn dropped_table_schema(sql: &str) -> Option<Option<String>> {
    let tokens = split::tokenize(Dialect::Sqlite, sql);
    let drop = tokens.first()?.is_word(sql, "DROP") && tokens.get(1)?.is_word(sql, "TABLE");
    if !drop {
        return None;
    }
    let qualified = tokens.get(3).is_some_and(|t| t.kind == TokenKind::Punct('.'));
    Some(qualified.then(|| unquote(tokens[2].text(sql))))
}

impl SqliteSchemaDialect {
    fn table_name(&self, ctx: &RenderContext, name: &str) -> String {
        qualified(ctx.target_schema.as_deref(), name)
    }

    fn map_column(&self, column: &ColumnDef, ctx: &RenderContext) -> ColumnDef {
        let mut column = column.clone();
        column.default = column.default.map(|d| map_text(&d, ctx));
        if let Some(generated) = &mut column.generated {
            generated.expression = map_text(&generated.expression, ctx);
        }
        column
    }

    fn map_table(&self, table: &TableDef, ctx: &RenderContext) -> TableDef {
        let mut table = table.clone();
        for column in &mut table.columns {
            *column = self.map_column(column, ctx);
        }
        for check in &mut table.checks {
            check.expression = map_text(&check.expression, ctx);
        }
        for index in &mut table.indexes {
            *index = self.map_index(index, ctx);
        }
        for key in &mut table.foreign_keys {
            key.ref_schema = None;
        }
        table
    }

    fn map_index(&self, index: &IndexDef, ctx: &RenderContext) -> IndexDef {
        let mut index = index.clone();
        for part in &mut index.parts {
            part.expression = part.expression.as_deref().map(|e| map_text(e, ctx));
        }
        index.predicate = index.predicate.as_deref().map(|p| map_text(p, ctx));
        index
    }

    /// The rebuild of a table: the new table as the source defines it, the common columns copied, the old table
    /// dropped, the new one renamed, then its indexes and the old table's triggers.
    fn rebuild(
        &self,
        ctx: &RenderContext,
        source: &TableDef,
        target: &TableDef,
        column_map: &[(String, String)],
    ) -> Vec<RenderedStatement> {
        let schema = ctx.target_schema.as_deref();
        let name = &target.name;
        let new_name = format!("{REBUILD_PREFIX}{name}");
        let mut table = self.map_table(source, ctx);
        table.name = new_name.clone();
        let mut out = Vec::new();

        let mut create = warned(
            create_table_sql(schema, &table),
            Risk::Review,
            format!(
                "Rebuilds table {name} (SQLite cannot alter it in place): a new table, the rows copied, the old table \
                 dropped and the new one renamed; indexes and triggers are created again"
            ),
        );
        let copied: Vec<&(String, String)> = column_map
            .iter()
            .filter(|(column, _)| {
                // A generated column computes its own values.
                !source
                    .columns
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(column) && c.generated.is_some())
            })
            .collect();
        let lost: Vec<&str> = target
            .columns
            .iter()
            .filter(|c| !column_map.iter().any(|(_, t)| t.eq_ignore_ascii_case(&c.name)))
            .map(|c| c.name.as_str())
            .collect();
        if !lost.is_empty() {
            create.risk = Risk::DataLoss;
            create
                .warnings
                .push(format!("The rebuild of {name} drops column(s) {} and their data", lost.join(", ")));
        }
        for column in &source.columns {
            let mapped = column_map.iter().any(|(s, _)| s.eq_ignore_ascii_case(&column.name));
            let required = !column.nullable && column.default.is_none() && column.generated.is_none() && column.identity.is_none();
            if !mapped && required {
                create.warnings.push(format!(
                    "{name}.{} is NOT NULL without a default: copying the rows fails if the table has any",
                    column.name
                ));
            }
        }
        for (s, t) in column_map {
            let (Some(to), Some(from)) = (
                source.columns.iter().find(|c| c.name.eq_ignore_ascii_case(s)),
                target.columns.iter().find(|c| c.name.eq_ignore_ascii_case(t)),
            ) else {
                continue;
            };
            if self.type_change_risk(from, to) != Risk::None {
                create.warnings.push(format!(
                    "{name}.{} changes from {} to {}; review how the values convert",
                    to.name, from.type_name, to.type_name
                ));
            }
            if from.nullable && !to.nullable {
                create.warnings.push(format!(
                    "{name}.{} becomes NOT NULL: copying the rows fails if it has NULLs",
                    to.name
                ));
            }
        }
        out.push(create);

        let new_table = qualified(schema, &new_name);
        let old_table = qualified(schema, name);
        if copied.is_empty() {
            out[0]
                .warnings
                .push(format!("No column of {name} is kept: the rebuilt table starts empty"));
        } else {
            let into: Vec<String> = copied.iter().map(|(s, _)| s.clone()).collect();
            let from: Vec<String> = copied.iter().map(|(_, t)| t.clone()).collect();
            out.push(statement(format!(
                "INSERT INTO {new_table} ({}) SELECT {} FROM {old_table}",
                idents(&into),
                idents(&from)
            )));
        }
        out.push(statement(format!("DROP TABLE {old_table}")));
        out.push(statement(format!("ALTER TABLE {new_table} RENAME TO {}", ident(name))));
        for index in &table.indexes {
            out.push(statement(index_sql(schema, name, index)));
        }
        for (trigger, sql) in triggers_of(target, schema) {
            out.push(warned(
                sql,
                Risk::Review,
                format!("Trigger {trigger} is created again as the target had it; check it still fits the new {name}"),
            ));
        }
        out
    }
}

fn names(columns: &[String], column: &str) -> bool {
    columns.iter().any(|c| c.eq_ignore_ascii_case(column))
}

/// Whether ALTER TABLE ... ADD COLUMN can add `column` of the source table: no key uses it, it is not a stored
/// generated column, and it is nullable or has a constant default.
fn addable(column: &ColumnDef, source: &TableDef) -> bool {
    let default_ok = match column.default.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(default) => constant_default(default) && !(default.eq_ignore_ascii_case("NULL") && !column.nullable),
        None => column.nullable || column.generated.is_some(),
    };
    default_ok
        && column.generated.as_ref().is_none_or(|g| !g.stored)
        && column.identity.is_none()
        && !source.primary_key.as_ref().is_some_and(|k| names(&k.columns, &column.name))
        && !source.uniques.iter().any(|k| names(&k.columns, &column.name))
        && !source.foreign_keys.iter().any(|k| names(&k.columns, &column.name))
}

/// Whether ALTER TABLE ... DROP COLUMN can drop `column` of the target table (no key, check or generated column uses it).
fn droppable(column: &ColumnDef, target: &TableDef) -> bool {
    let named = |columns: &Vec<String>| names(columns, &column.name);
    !target.primary_key.as_ref().is_some_and(|k| named(&k.columns))
        && !target.uniques.iter().any(|k| named(&k.columns))
        && !target.foreign_keys.iter().any(|k| named(&k.columns))
        && !target.checks.iter().any(|c| mentions(&c.expression, &column.name))
        && !target
            .columns
            .iter()
            .filter_map(|c| c.generated.as_ref())
            .any(|g| mentions(&g.expression, &column.name))
}

/// Whether one change of a table needs a rebuild.
fn change_needs_rebuild(change: &Change, source: &TableDef, target: &TableDef) -> bool {
    let column = |table: &'_ TableDef| {
        let found = table.columns.iter().find(|c| c.name.eq_ignore_ascii_case(&change.name));
        found.cloned()
    };
    match (change.part.as_str(), change.what) {
        ("column", ChangeKind::Added) => column(source).is_none_or(|c| !addable(&c, source)),
        ("column", ChangeKind::Removed) => column(target).is_none_or(|c| !droppable(&c, target)),
        ("index", _) => false,
        _ => true,
    }
}

impl SchemaDialect for SqliteSchemaDialect {
    fn normalize_type(&self, raw: &str, _schema: Option<&str>) -> String {
        canonical(raw, true)
    }

    fn normalize_default(&self, raw: &str, schema: Option<&str>) -> String {
        let text = match schema {
            Some(schema) => strip_schema_qualifier(raw, schema),
            None => raw.to_owned(),
        };
        let text = canonical(&text, false);
        if text == "null" {
            String::new()
        } else {
            text
        }
    }

    fn normalize_expression(&self, raw: &str, schema: Option<&str>) -> String {
        let text = match schema {
            Some(schema) => strip_schema_qualifier(raw, schema),
            None => raw.to_owned(),
        };
        canonical(text.trim().trim_end_matches(';'), false)
    }

    /// Values keep their storage class where the affinity stays; a new affinity converts what it can (a rebuild copies
    /// the rows through it), which needs a look but loses nothing in an ordinary table.
    fn type_change_risk(&self, from: &ColumnDef, to: &ColumnDef) -> Risk {
        if canonical(&from.type_name, true) == canonical(&to.type_name, true) || affinity(&from.type_name) == affinity(&to.type_name) {
            Risk::None
        } else {
            Risk::Review
        }
    }

    fn features(&self) -> DialectFeatures {
        DialectFeatures {
            transactional_ddl: true,
            alter_column_type: false,
            drop_column: true,
            restrict_equals_no_action: false,
            batch_separator: None,
            inline_comments: false,
            columns_held_by_dependents: false,
            inline_foreign_keys: true,
        }
    }

    /// Anything beyond index changes, a plain ADD COLUMN (nullable or with a constant default, no key) and a plain
    /// DROP COLUMN (no key, check or generated column uses it).
    fn needs_rebuild(&self, item: &DiffItem, source: &TableDef, target: &TableDef) -> bool {
        item.changes.iter().any(|change| change_needs_rebuild(change, source, target))
    }

    fn quote_ident(&self, name: &str) -> String {
        ident(name)
    }

    fn table_ddl(&self, schema: Option<&str>, table: &TableDef) -> String {
        let mut parts = vec![format!("{};", create_table_sql(schema, table))];
        parts.extend(
            table
                .indexes
                .iter()
                .map(|index| format!("{};", index_sql(schema, &table.name, index))),
        );
        parts.join("\n\n")
    }

    fn view_ddl(&self, schema: Option<&str>, view: &ViewDef) -> String {
        let body = view.definition.trim().trim_end_matches(';').trim_end();
        format!("CREATE VIEW {} AS\n{body};", qualified(schema, &view.name))
    }

    fn render(&self, step: &MigrationStep, ctx: &RenderContext) -> Vec<RenderedStatement> {
        let table_name = |name: &str| self.table_name(ctx, name);
        let rebuild_needed = |what: &str| vec![manual(&format!("{what}: SQLite needs a table rebuild for it"), None)];
        match step {
            MigrationStep::DropView { name, .. } => vec![statement(format!("DROP VIEW {}", table_name(name)))],
            MigrationStep::DropIndex { name, .. } => vec![statement(format!("DROP INDEX {}", table_name(name)))],
            MigrationStep::DropTable { name } => vec![warned(
                format!("DROP TABLE {}", table_name(name)),
                Risk::DataLoss,
                format!("Drops table {name} and all its rows"),
            )],
            MigrationStep::CreateTable { table } => {
                let table = self.map_table(table, ctx);
                vec![statement(create_table_sql(ctx.target_schema.as_deref(), &table))]
            }
            MigrationStep::AddColumn { table, column } => {
                let column = self.map_column(column, ctx);
                vec![statement(format!(
                    "ALTER TABLE {} ADD COLUMN {}",
                    table_name(table),
                    column_sql(&column, None)
                ))]
            }
            MigrationStep::DropColumn { table, column } => vec![warned(
                format!("ALTER TABLE {} DROP COLUMN {}", table_name(table), ident(column)),
                Risk::DataLoss,
                format!("Drops column {table}.{column} and its data"),
            )],
            MigrationStep::RebuildTable {
                source_table,
                target_table,
                column_map,
            } => self.rebuild(ctx, source_table, target_table, column_map),
            MigrationStep::CreateIndex { table, index } => {
                let index = self.map_index(index, ctx);
                let sql = index_sql(ctx.target_schema.as_deref(), table, &index);
                if index.unique {
                    vec![warned(
                        sql,
                        Risk::Review,
                        format!("The unique index {} on {table} fails on duplicate keys", index.name),
                    )]
                } else {
                    vec![statement(sql)]
                }
            }
            MigrationStep::CreateView { view, .. } => {
                let body = view.definition.trim().trim_end_matches(';').trim_end();
                let mapped = map_text(body, ctx);
                let mut rendered = statement(format!("CREATE VIEW {} AS\n{mapped}", table_name(&view.name)));
                if mapped != body {
                    rendered.risk = Risk::Review;
                    rendered.warnings.push(format!(
                        "Database references in the body of {} were changed from {} to {}; review them",
                        view.name,
                        ctx.source_schema.as_deref().unwrap_or_default(),
                        ctx.target_schema.as_deref().unwrap_or_default()
                    ));
                }
                vec![rendered]
            }
            // The planner rebuilds tables for these (foreign keys and constraints live in the table).
            MigrationStep::DropForeignKey { table, name } => rebuild_needed(&format!("Drop foreign key {name} of {table}")),
            MigrationStep::AddForeignKey { table, key } => rebuild_needed(&format!(
                "Add foreign key {} to {table}",
                key.name.as_deref().unwrap_or("(unnamed)")
            )),
            MigrationStep::DropConstraint { table, .. } => rebuild_needed(&format!("Drop a constraint of {table}")),
            MigrationStep::AddPrimaryKey { table, .. } => rebuild_needed(&format!("Add the primary key of {table}")),
            MigrationStep::AddConstraint { table, constraint } => {
                let what = match constraint {
                    ConstraintDef::Unique { .. } => "unique constraint",
                    ConstraintDef::Check { .. } => "check constraint",
                };
                rebuild_needed(&format!("Add a {what} to {table}"))
            }
            MigrationStep::AlterColumn { table, to, .. } => rebuild_needed(&format!("Change column {table}.{}", to.name)),
            MigrationStep::SetTableOption { table, name, value } => rebuild_needed(&format!("Set {name} = {value} on {table}")),
            MigrationStep::SetComment { target, .. } => {
                vec![manual(&format!("SQLite has no comments ({} {})", target.kind, target.name), None)]
            }
            MigrationStep::Manual { reason, ddl_hint } => vec![manual(reason, ddl_hint.as_deref())],
        }
    }

    /// `BEGIN` ... `COMMIT`; where the script drops or rebuilds tables, foreign keys are switched off around it (SQLite
    /// ignores the PRAGMA inside a transaction, and a dropped parent would otherwise cascade or fail) and checked before
    /// the commit.
    fn wrap(&self, body: Vec<RenderedStatement>, transactional: bool) -> Vec<RenderedStatement> {
        let drops = body.iter().find_map(|statement| dropped_table_schema(&statement.sql));
        let mut out = Vec::with_capacity(body.len() + 5);
        if drops.is_some() {
            out.push(warned(
                "PRAGMA foreign_keys = OFF".to_owned(),
                Risk::Review,
                "Foreign keys are not enforced while tables are dropped and rebuilt (the gateway asks for an admin \
                 confirmation for PRAGMA); SQLite ignores this PRAGMA inside a transaction, so run the script outside one",
            ));
        }
        if transactional {
            out.push(statement("BEGIN".to_owned()));
        }
        out.extend(body);
        if let Some(schema) = drops.clone() {
            let schema = schema.map_or_else(|| "NULL".to_owned(), |s| literal(&s));
            out.push(warned(
                FOREIGN_KEY_GUARD.replace("{schema}", &schema),
                Risk::Review,
                "Stops the script with an \"integer overflow\" error when a row breaks a foreign key; then run ROLLBACK \
                 and PRAGMA foreign_keys = ON",
            ));
        }
        if transactional {
            out.push(statement("COMMIT".to_owned()));
        }
        if drops.is_some() {
            out.push(statement("PRAGMA foreign_keys = ON".to_owned()));
        }
        out
    }
}
