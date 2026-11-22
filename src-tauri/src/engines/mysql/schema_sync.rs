//! The MariaDB/MySQL schema compare dialect: normalizes catalog text for comparison, rates column type changes, writes
//! canonical DDL and renders migration steps. One dialect per flavour; they differ only where the servers do (dropping
//! a check constraint).
//!
//! Normalization makes MySQL 8.4 and MariaDB 11 spellings compare equal: integer display widths go (`int(11)` → `int`,
//! `tinyint(1)` and `zerofill` widths stay), identifiers lose their backticks, string literals lose their charset
//! introducer (`_utf8mb4'a'`), `current_timestamp()` / `now()` read as `current_timestamp`, and the scope database's
//! qualifier is dropped from view bodies.
//!
//! MySQL DDL is not transactional (every statement commits implicitly), so a script is never wrapped in a transaction;
//! the script header says it is not atomic. A column is changed with `MODIFY COLUMN` and its whole definition, built from
//! the target column with only the compared changes applied, so ignored attributes and the comment stay as they are.

use super::catalog::quote_ident as ident;
use crate::contract::{
    replace_schema_qualifier, strip_schema_qualifier, ColumnChanges, ColumnDef, CommentTarget, ConstraintDef, ConstraintKind,
    DialectFeatures, ForeignKeyDef, IdentityKind, IndexDef, IndexPart, KeyDef, MigrationStep, RenderContext, RenderedStatement, Risk,
    SchemaDialect, TableDef, ViewDef,
};

pub struct MySqlSchemaDialect {
    mariadb: bool,
}

pub static MYSQL_SCHEMA_DIALECT: MySqlSchemaDialect = MySqlSchemaDialect { mariadb: false };
pub static MARIADB_SCHEMA_DIALECT: MySqlSchemaDialect = MySqlSchemaDialect { mariadb: true };

// --- Text helpers -------------------------------------------------------------------------------------------------------

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The content of the quoted run starting at `text[0]` and its end (exclusive): a doubled quote or a backslash escapes.
fn read_quoted(text: &str, quote: char) -> (String, usize) {
    let mut content = String::new();
    let mut chars = text.char_indices().skip(1).peekable();
    while let Some((at, c)) = chars.next() {
        if c == '\\' && quote != '`' {
            if let Some((_, next)) = chars.next() {
                content.push(match next {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '0' => '\0',
                    other => other,
                });
            }
            continue;
        }
        if c == quote {
            if chars.peek().is_some_and(|(_, next)| *next == quote) {
                chars.next();
                content.push(quote);
                continue;
            }
            return (content, at + c.len_utf8());
        }
        content.push(c);
    }
    (content, text.len())
}

/// A canonical spelling of an expression, a default or a view body for comparison: lowercase outside string literals,
/// identifiers without backticks, literals re-quoted without charset introducers, whitespace kept only between two
/// words, and redundant outer parentheses removed.
fn canonical(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text.trim();
    let mut space = false;
    let push_word_space = |out: &mut String, space: &mut bool, next: char| {
        if *space && out.chars().last().is_some_and(is_word) && is_word(next) {
            out.push(' ');
        }
        *space = false;
    };
    while let Some(c) = rest.chars().next() {
        if c.is_whitespace() {
            space = true;
            rest = &rest[c.len_utf8()..];
            continue;
        }
        match c {
            '\'' | '"' => {
                // A charset introducer (`_utf8mb4'a'`) says nothing the comparison needs.
                let at = out
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| !is_word(*ch))
                    .map_or(0, |(at, ch)| at + ch.len_utf8());
                if out[at..].starts_with('_') && !space {
                    out.truncate(at);
                }
                let (content, end) = read_quoted(rest, c);
                if space && out.chars().last().is_some_and(is_word) {
                    out.push(' ');
                }
                space = false;
                out.push('\'');
                out.push_str(&content.replace('\'', "''"));
                out.push('\'');
                rest = &rest[end..];
            }
            '`' => {
                let (content, end) = read_quoted(rest, '`');
                let name = content.to_lowercase();
                push_word_space(&mut out, &mut space, name.chars().next().unwrap_or('x'));
                out.push_str(&name);
                rest = &rest[end..];
            }
            _ => {
                push_word_space(&mut out, &mut space, c);
                out.extend(c.to_lowercase());
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    strip_outer_parens(&out).to_owned()
}

/// `((a > 0))` → `a > 0`, while the parentheses wrap the whole text.
fn strip_outer_parens(text: &str) -> &str {
    let mut text = text.trim();
    while text.starts_with('(') && text.ends_with(')') && wraps_whole(text) {
        text = text[1..text.len() - 1].trim();
    }
    text
}

/// Whether the first `(` of `text` closes at its last character (outside literals).
fn wraps_whole(text: &str) -> bool {
    let mut depth = 0i32;
    let mut rest = text;
    let mut at = 0;
    while let Some(c) = rest.chars().next() {
        let width = if c == '\'' || c == '"' || c == '`' {
            read_quoted(rest, c).1
        } else {
            c.len_utf8()
        };
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && at + width < text.len() {
                    return false;
                }
            }
            _ => {}
        }
        at += width;
        rest = &rest[width..];
    }
    depth == 0
}

/// Applies `map` to the parts of canonical text outside string literals.
fn outside_literals(text: &str, map: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\'') {
        out.push_str(&map(&rest[..at]));
        let (_, end) = read_quoted(&rest[at..], '\'');
        out.push_str(&rest[at..at + end]);
        rest = &rest[at + end..];
    }
    out.push_str(&map(rest));
    out
}

/// `now()`, `current_timestamp()`, `localtimestamp` … as `current_timestamp` (with a precision where there is one).
fn canonical_now(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let names = ["current_timestamp", "localtimestamp", "localtime", "now"];
    while !rest.is_empty() {
        let boundary = out.chars().last().is_none_or(|c| !is_word(c));
        let found = boundary
            .then(|| {
                names
                    .iter()
                    .find(|name| rest.starts_with(**name) && !rest[name.len()..].chars().next().is_some_and(is_word))
            })
            .flatten();
        match found {
            Some(name) => {
                rest = &rest[name.len()..];
                out.push_str("current_timestamp");
                if let Some(args) = rest.strip_prefix("()") {
                    rest = args;
                    if rest.chars().next().is_some_and(is_word) {
                        out.push(' ');
                    }
                }
            }
            None => {
                let c = rest.chars().next().unwrap_or(' ');
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
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

/// Source-scope references pointed at the target scope (view bodies, expressions).
fn map_text(text: &str, ctx: &RenderContext) -> String {
    match (&ctx.source_schema, &ctx.target_schema) {
        (Some(source), Some(target)) if source != target => replace_schema_qualifier(text, source, Some(&ident(target))),
        _ => text.to_owned(),
    }
}

// --- Types --------------------------------------------------------------------------------------------------------------

/// Integer types whose display width is only a display hint.
const INTEGER_TYPES: [&str; 7] = ["tinyint", "smallint", "mediumint", "int", "integer", "bigint", "year"];

/// Lowercase outside literals, whitespace collapsed, display widths dropped (`int(11) unsigned` → `int unsigned`) except
/// `tinyint(1)` (boolean) and zerofill columns, `integer` → `int`, `bool` → `tinyint(1)`.
fn canonical_type(raw: &str) -> String {
    let mut text = String::new();
    let mut rest = raw.trim();
    let mut space = false;
    while let Some(c) = rest.chars().next() {
        if c.is_whitespace() {
            space = true;
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if space && !text.is_empty() {
            text.push(' ');
        }
        space = false;
        if c == '\'' || c == '"' {
            let (content, end) = read_quoted(rest, c);
            text.push('\'');
            text.push_str(&content.replace('\'', "''"));
            text.push('\'');
            rest = &rest[end..];
        } else {
            text.extend(c.to_lowercase());
            rest = &rest[c.len_utf8()..];
        }
    }
    let text = text.replace(" (", "(");
    let base_end = text.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(text.len());
    let base = &text[..base_end];
    match base {
        "bool" | "boolean" => return "tinyint(1)".to_owned(),
        "integer" => return canonical_type(&format!("int{}", &text[base_end..])),
        _ => {}
    }
    if INTEGER_TYPES.contains(&base) && !text.contains("zerofill") && text[base_end..].starts_with('(') {
        let close = text[base_end..].find(')').map_or(text.len(), |at| base_end + at + 1);
        let width = &text[base_end + 1..close - 1];
        if !(base == "tinyint" && width == "1") {
            return format!("{base}{}", &text[close..]);
        }
    }
    text
}

/// A normalized type split into its base name, numeric modifiers and signedness.
struct ParsedType {
    base: String,
    args: Vec<i64>,
    unsigned: bool,
    /// The values of an `enum` or `set`.
    values: Vec<String>,
}

fn parse_type(normalized: &str) -> ParsedType {
    let base_end = normalized.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(normalized.len());
    let rest = &normalized[base_end..];
    let args = match rest.strip_prefix('(').and_then(|r| r.find(')').map(|at| &r[..at])) {
        Some(inner) if !inner.contains('\'') => inner.split(',').filter_map(|a| a.trim().parse::<i64>().ok()).collect(),
        _ => Vec::new(),
    };
    ParsedType {
        base: normalized[..base_end].to_owned(),
        args,
        unsigned: rest.contains("unsigned"),
        values: literals(rest),
    }
}

/// The string literals of `text` (the values of `enum('a','b')`).
fn literals(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('\'') {
        let (value, end) = read_quoted(&rest[at..], '\'');
        values.push(value);
        rest = &rest[at + end..];
    }
    values
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    /// Bytes.
    Integer(u8),
    Decimal,
    /// Bytes.
    Float(u8),
    /// Maximum length in characters (bytes for blobs): `None` = from the modifier.
    Text(Option<i64>),
    Temporal,
    Other,
}

fn family(base: &str) -> Family {
    match base {
        "tinyint" => Family::Integer(1),
        "smallint" => Family::Integer(2),
        "mediumint" => Family::Integer(3),
        "int" => Family::Integer(4),
        "bigint" => Family::Integer(8),
        "decimal" | "numeric" => Family::Decimal,
        "float" => Family::Float(4),
        "double" | "real" => Family::Float(8),
        "char" | "varchar" => Family::Text(None),
        "tinytext" => Family::Text(Some(255)),
        "text" => Family::Text(Some(65_535)),
        "mediumtext" => Family::Text(Some(16_777_215)),
        "longtext" | "json" => Family::Text(Some(4_294_967_295)),
        "datetime" | "timestamp" | "time" => Family::Temporal,
        _ => Family::Other,
    }
}

fn type_risk(from: &ParsedType, to: &ParsedType) -> Risk {
    let (a, b) = (family(&from.base), family(&to.base));
    match (a, b) {
        (Family::Integer(f), Family::Integer(t)) => {
            if t < f || (from.unsigned != to.unsigned && t <= f) {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Decimal, Family::Decimal) => {
            let precision = |args: &[i64]| (args.first().copied().unwrap_or(10), args.get(1).copied().unwrap_or(0));
            let ((fp, fs), (tp, ts)) = (precision(&from.args), precision(&to.args));
            if ts < fs || tp - ts < fp - fs {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Integer(bytes), Family::Decimal) => {
            let digits = match bytes {
                1 => 3,
                2 => 5,
                3 => 8,
                4 => 10,
                _ => 20,
            };
            let (p, s) = (to.args.first().copied().unwrap_or(10), to.args.get(1).copied().unwrap_or(0));
            if p - s >= digits {
                Risk::None
            } else {
                Risk::DataLoss
            }
        }
        (Family::Float(f), Family::Float(t)) => {
            if t < f {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Integer(_) | Family::Decimal, Family::Float(_)) | (Family::Float(_), Family::Decimal) => Risk::Review,
        (Family::Decimal | Family::Float(_), Family::Integer(_)) => Risk::DataLoss,
        (Family::Text(f), Family::Text(t)) => {
            let capacity = |fixed: Option<i64>, args: &[i64]| fixed.or_else(|| args.first().copied()).unwrap_or(0);
            if capacity(t, &to.args) < capacity(f, &from.args) {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Temporal, Family::Temporal) if from.base == to.base => {
            if to.args.first().copied().unwrap_or(0) < from.args.first().copied().unwrap_or(0) {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Integer(_) | Family::Decimal | Family::Float(_), Family::Text(Some(_))) => Risk::None,
        _ if from.base == to.base && matches!(from.base.as_str(), "enum" | "set") => {
            if from.values.iter().all(|value| to.values.contains(value)) {
                Risk::None
            } else {
                Risk::DataLoss
            }
        }
        _ if from.base == to.base => {
            if to.args.len() == from.args.len() && to.args.iter().zip(&from.args).all(|(t, f)| t >= f) {
                Risk::Review
            } else {
                Risk::DataLoss
            }
        }
        _ => Risk::Review,
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

/// A default as it goes after `DEFAULT`: literals and the current-time functions bare, other expressions in
/// parentheses (MySQL 8 needs them), and `ON UPDATE …` kept after it.
fn default_sql(default: &str) -> String {
    let upper = default.to_ascii_uppercase();
    let (value, on_update) = match upper.rfind(" ON UPDATE ") {
        Some(at) => (default[..at].trim(), Some(default[at + " ON UPDATE ".len()..].trim())),
        None => (default.trim(), None),
    };
    let lower = value.to_ascii_lowercase();
    let bare = value.starts_with('\'')
        || value.starts_with('"')
        || value.starts_with('(')
        || lower.starts_with("b'")
        || lower.starts_with("x'")
        || lower.starts_with("0x")
        || matches!(lower.as_str(), "null" | "true" | "false")
        || value.parse::<f64>().is_ok()
        || ["current_timestamp", "localtimestamp", "localtime", "now"]
            .iter()
            .any(|name| lower.starts_with(name) && lower[name.len()..].chars().all(|c| c == '(' || c == ')' || c.is_ascii_digit()));
    let mut sql = if bare { value.to_owned() } else { format!("({value})") };
    if let Some(on_update) = on_update {
        sql.push_str(&format!(" ON UPDATE {on_update}"));
    }
    sql
}

/// `` `name` type [COLLATE c] [AS (expr) STORED|VIRTUAL] [NOT NULL] [DEFAULT …] [AUTO_INCREMENT] [COMMENT '…'] ``.
fn column_sql(column: &ColumnDef) -> String {
    let mut sql = format!("{} {}", ident(&column.name), column.type_name);
    if let Some(collation) = &column.collation {
        sql.push_str(&format!(" COLLATE {collation}"));
    }
    if let Some(generated) = &column.generated {
        let kind = if generated.stored { "STORED" } else { "VIRTUAL" };
        sql.push_str(&format!(" GENERATED ALWAYS AS ({}) {kind}", generated.expression));
    }
    if !column.nullable {
        sql.push_str(" NOT NULL");
    }
    if column.generated.is_none() {
        if let Some(default) = column.default.as_deref().filter(|d| !d.trim().is_empty()) {
            sql.push_str(&format!(" DEFAULT {}", default_sql(default)));
        }
    }
    if column.identity.as_ref().is_some_and(|i| i.kind == IdentityKind::AutoIncrement) {
        sql.push_str(" AUTO_INCREMENT");
    }
    if let Some(comment) = column.comment.as_deref().filter(|c| !c.is_empty()) {
        sql.push_str(&format!(" COMMENT {}", literal(comment)));
    }
    sql
}

/// `CONSTRAINT name ` for a name the user chose; the server names the rest again.
fn constraint_name(name: &Option<String>, system_named: bool) -> String {
    match name {
        Some(name) if !system_named => format!("CONSTRAINT {} ", ident(name)),
        _ => String::new(),
    }
}

fn unique_clause(key: &KeyDef) -> String {
    format!("{}UNIQUE ({})", constraint_name(&key.name, key.system_named), idents(&key.columns))
}

fn check_clause(name: &Option<String>, system_named: bool, expression: &str) -> String {
    format!("{}CHECK ({})", constraint_name(name, system_named), expression.trim())
}

fn foreign_key_clause(schema: Option<&str>, key: &ForeignKeyDef) -> String {
    let ref_schema = key.ref_schema.as_deref().or(schema);
    let mut sql = format!(
        "{}FOREIGN KEY ({}) REFERENCES {} ({})",
        constraint_name(&key.name, key.system_named),
        idents(&key.columns),
        qualified(ref_schema, &key.ref_table),
        idents(&key.ref_columns)
    );
    for (clause, action) in [("ON UPDATE", &key.on_update), ("ON DELETE", &key.on_delete)] {
        if let Some(action) = action.as_deref().filter(|a| !a.eq_ignore_ascii_case("NO ACTION")) {
            sql.push_str(&format!(" {clause} {}", action.to_uppercase()));
        }
    }
    sql
}

/// A key part as written: a column, a column prefix (`` `name`(10) ``, read from the catalog as an expression) or a
/// functional key part in its own parentheses.
fn index_part_sql(part: &IndexPart) -> String {
    let key = match (&part.column, &part.expression) {
        (Some(column), _) => ident(column),
        (None, Some(expression)) if is_prefix_part(expression) => expression.clone(),
        (None, Some(expression)) => format!("({})", expression.trim()),
        (None, None) => String::new(),
    };
    if part.descending {
        format!("{key} DESC")
    } else {
        key
    }
}

/// `` `name`(10) ``: a column prefix.
fn is_prefix_part(expression: &str) -> bool {
    let text = expression.trim();
    if !text.starts_with('`') {
        return false;
    }
    let (_, end) = read_quoted(text, '`');
    text[end..]
        .strip_prefix('(')
        .and_then(|r| r.strip_suffix(')'))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

fn index_sql(schema: Option<&str>, table: &str, index: &IndexDef) -> String {
    let method = index.method.as_deref().map(str::to_ascii_uppercase);
    let parts = index.parts.iter().map(index_part_sql).collect::<Vec<_>>().join(", ");
    // `CREATE FULLTEXT INDEX` is valid MySQL, but the classifier's parser only knows the ALTER TABLE form.
    if let Some(kind @ ("FULLTEXT" | "SPATIAL")) = method.as_deref() {
        return format!(
            "ALTER TABLE {} ADD {kind} INDEX {} ({parts})",
            qualified(schema, table),
            ident(&index.name)
        );
    }
    let using = match method.as_deref() {
        Some("HASH") => " USING HASH",
        _ => "",
    };
    format!(
        "CREATE {}INDEX {} ON {} ({parts}){using}",
        if index.unique { "UNIQUE " } else { "" },
        ident(&index.name),
        qualified(schema, table),
    )
}

fn option<'t>(table: &'t TableDef, name: &str) -> Option<&'t str> {
    table
        .options
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// The character set of a collation (`utf8mb4_0900_ai_ci` → `utf8mb4`).
fn charset_of(collation: &str) -> &str {
    collation.split('_').next().unwrap_or(collation)
}

/// The `CREATE TABLE` statement alone (no trailing `;`): columns, keys, checks, foreign keys and table options.
fn create_table_sql(schema: Option<&str>, table: &TableDef) -> String {
    let mut lines: Vec<String> = table.columns.iter().map(column_sql).collect();
    if let Some(key) = &table.primary_key {
        lines.push(format!("PRIMARY KEY ({})", idents(&key.columns)));
    }
    lines.extend(table.uniques.iter().map(unique_clause));
    lines.extend(
        table
            .checks
            .iter()
            .map(|check| check_clause(&check.name, check.system_named, &check.expression)),
    );
    lines.extend(table.foreign_keys.iter().map(|key| foreign_key_clause(schema, key)));
    let mut sql = format!(
        "CREATE TABLE {} (\n    {}\n)",
        qualified(schema, &table.name),
        lines.join(",\n    ")
    );
    if let Some(engine) = option(table, "engine") {
        sql.push_str(&format!(" ENGINE={engine}"));
    }
    if let Some(collation) = option(table, "collation") {
        sql.push_str(&format!(" DEFAULT CHARSET={} COLLATE={collation}", charset_of(collation)));
    }
    if let Some(comment) = table.comment.as_deref().filter(|c| !c.is_empty()) {
        sql.push_str(&format!(" COMMENT={}", literal(comment)));
    }
    sql
}

impl MySqlSchemaDialect {
    fn label(&self) -> &'static str {
        if self.mariadb {
            "MariaDB"
        } else {
            "MySQL"
        }
    }

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
        for key in &mut table.foreign_keys {
            *key = self.map_foreign_key(key, ctx);
        }
        table
    }

    fn map_index(&self, index: &IndexDef, ctx: &RenderContext) -> IndexDef {
        let mut index = index.clone();
        for part in &mut index.parts {
            part.expression = part.expression.as_deref().map(|e| map_text(e, ctx));
        }
        index
    }

    fn map_foreign_key(&self, key: &ForeignKeyDef, ctx: &RenderContext) -> ForeignKeyDef {
        let mut key = key.clone();
        if key.ref_schema.is_some() && key.ref_schema == ctx.source_schema {
            key.ref_schema = None;
        }
        key
    }

    fn alter_column(
        &self,
        ctx: &RenderContext,
        table: &str,
        from: &ColumnDef,
        to: &ColumnDef,
        changes: ColumnChanges,
    ) -> Vec<RenderedStatement> {
        let to = self.map_column(to, ctx);
        let column = changes.apply(from, &to);
        let table_name = self.table_name(ctx, table);
        if changes.generated && to.generated.is_some() {
            // A column becomes generated or gets another expression: drop it and add it again (MySQL cannot turn a
            // stored column into a virtual one in place).
            let risk = if from.generated.is_some() { Risk::Review } else { Risk::DataLoss };
            let mut out = vec![warned(
                format!("ALTER TABLE {table_name} DROP COLUMN {}", ident(&from.name)),
                risk,
                format!(
                    "The generated column {} is dropped and added again{}",
                    to.name,
                    if risk == Risk::DataLoss { "; its values are lost" } else { "" }
                ),
            )];
            out.push(statement(format!("ALTER TABLE {table_name} ADD COLUMN {}", column_sql(&column))));
            return out;
        }
        let mut sql = format!("ALTER TABLE {table_name} MODIFY COLUMN {}", column_sql(&column));
        let mut warnings = Vec::new();
        let mut risk = Risk::None;
        if changes.type_name || changes.collation {
            let type_risk = self.type_change_risk(from, &to);
            risk = risk.max(type_risk);
            if type_risk == Risk::DataLoss {
                warnings.push(format!(
                    "Changing {} from {} to {} may lose data",
                    to.name, from.type_name, to.type_name
                ));
            } else if type_risk == Risk::Review {
                warnings.push(format!(
                    "Converts {} from {} to {}; review how the values convert",
                    to.name, from.type_name, to.type_name
                ));
            }
        }
        if changes.nullability && !column.nullable {
            sql = format!(
                "-- UPDATE {table_name} SET {} = ... WHERE {} IS NULL;\n{sql}",
                ident(&column.name),
                ident(&column.name)
            );
            risk = risk.max(Risk::Review);
            warnings.push(format!(
                "Fails if {table}.{} has NULLs; fill them first (see the UPDATE stub)",
                column.name
            ));
        }
        if changes.generated && from.generated.is_some() {
            risk = risk.max(Risk::Review);
            warnings.push(format!("{} stops being generated and keeps its current values", column.name));
        }
        vec![RenderedStatement { sql, risk, warnings }]
    }

    fn comment(&self, ctx: &RenderContext, target: &CommentTarget, comment: Option<&str>) -> RenderedStatement {
        let name = self.table_name(ctx, &target.name);
        match (&target.column, &target.definition, target.kind.as_str()) {
            (Some(_), Some(definition), _) => {
                let column = ColumnDef {
                    comment: comment.map(str::to_owned),
                    ..self.map_column(definition, ctx)
                };
                statement(format!("ALTER TABLE {name} MODIFY COLUMN {}", column_sql(&column)))
            }
            (Some(column), None, _) => manual(
                &format!(
                    "Set the comment of {}.{column} with ALTER TABLE … MODIFY COLUMN and its whole definition",
                    target.name
                ),
                None,
            ),
            // A manual step: the gateway's parser cannot read ALTER TABLE … COMMENT (see SetTableOption).
            (None, _, "table") => manual(
                &format!("Set the comment of table {}", target.name),
                Some(&format!("ALTER TABLE {name} COMMENT = {};", literal(comment.unwrap_or_default()))),
            ),
            (None, _, kind) => manual(&format!("{} has no comments on a {kind} ({})", self.label(), target.name), None),
        }
    }
}

impl SchemaDialect for MySqlSchemaDialect {
    fn normalize_type(&self, raw: &str, _schema: Option<&str>) -> String {
        canonical_type(raw)
    }

    fn normalize_default(&self, raw: &str, schema: Option<&str>) -> String {
        let text = match schema {
            Some(schema) => strip_schema_qualifier(raw, schema),
            None => raw.to_owned(),
        };
        let text = canonical(&text);
        if text == "null" {
            return String::new();
        }
        outside_literals(&text, canonical_now)
    }

    fn normalize_expression(&self, raw: &str, schema: Option<&str>) -> String {
        let text = match schema {
            Some(schema) => strip_schema_qualifier(raw, schema),
            None => raw.to_owned(),
        };
        let text = canonical(text.trim().trim_end_matches(';'));
        outside_literals(&text, canonical_now)
    }

    fn type_change_risk(&self, from: &ColumnDef, to: &ColumnDef) -> Risk {
        let (from, to) = (canonical_type(&from.type_name), canonical_type(&to.type_name));
        if from == to {
            return Risk::None;
        }
        type_risk(&parse_type(&from), &parse_type(&to))
    }

    fn features(&self) -> DialectFeatures {
        DialectFeatures {
            transactional_ddl: false,
            alter_column_type: true,
            drop_column: true,
            restrict_equals_no_action: true,
            batch_separator: None,
            inline_comments: true,
            columns_held_by_dependents: false,
            inline_foreign_keys: false,
        }
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
        match step {
            MigrationStep::DropForeignKey { table, name } => {
                vec![statement(format!(
                    "ALTER TABLE {} DROP FOREIGN KEY {}",
                    table_name(table),
                    ident(name)
                ))]
            }
            MigrationStep::DropView { name, .. } => vec![statement(format!("DROP VIEW {}", table_name(name)))],
            MigrationStep::DropIndex { table, name } => {
                vec![statement(format!("DROP INDEX {} ON {}", ident(name), table_name(table)))]
            }
            MigrationStep::DropConstraint { table, kind, name } => match (kind, name) {
                (ConstraintKind::PrimaryKey, _) => {
                    vec![statement(format!("ALTER TABLE {} DROP PRIMARY KEY", table_name(table)))]
                }
                (ConstraintKind::Unique, Some(name)) => {
                    vec![statement(format!("DROP INDEX {} ON {}", ident(name), table_name(table)))]
                }
                (ConstraintKind::Check, Some(name)) => {
                    let what = if self.mariadb { "CONSTRAINT" } else { "CHECK" };
                    vec![statement(format!("ALTER TABLE {} DROP {what} {}", table_name(table), ident(name)))]
                }
                (_, None) => vec![manual(&format!("Drop the unnamed constraint of {table}"), None)],
            },
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
                let sql = format!("ALTER TABLE {} ADD COLUMN {}", table_name(table), column_sql(&column));
                let required = !column.nullable && column.default.is_none() && column.identity.is_none() && column.generated.is_none();
                if required {
                    vec![warned(
                        sql,
                        Risk::Review,
                        format!(
                            "{table}.{} is NOT NULL without a default: existing rows get the type's implicit default",
                            column.name
                        ),
                    )]
                } else {
                    vec![statement(sql)]
                }
            }
            MigrationStep::AlterColumn { table, from, to, changes } => self.alter_column(ctx, table, from, to, *changes),
            MigrationStep::DropColumn { table, column } => vec![warned(
                format!("ALTER TABLE {} DROP COLUMN {}", table_name(table), ident(column)),
                Risk::DataLoss,
                format!("Drops column {table}.{column} and its data"),
            )],
            MigrationStep::RebuildTable { target_table, .. } => vec![manual(&format!("Rebuild table {} by hand", target_table.name), None)],
            MigrationStep::AddPrimaryKey { table, key } => vec![warned(
                format!("ALTER TABLE {} ADD PRIMARY KEY ({})", table_name(table), idents(&key.columns)),
                Risk::Review,
                format!("Adding the primary key of {table} fails on duplicate or NULL keys"),
            )],
            MigrationStep::AddConstraint { table, constraint } => {
                let (clause, what) = match constraint {
                    ConstraintDef::Unique { key } => (unique_clause(key), "unique constraint"),
                    ConstraintDef::Check { check } => (
                        check_clause(&check.name, check.system_named, &map_text(&check.expression, ctx)),
                        "check constraint",
                    ),
                };
                vec![warned(
                    format!("ALTER TABLE {} ADD {clause}", table_name(table)),
                    Risk::Review,
                    format!("The new {what} on {table} fails if existing rows violate it"),
                )]
            }
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
            MigrationStep::CreateView { view, replace } => {
                let body = view.definition.trim().trim_end_matches(';').trim_end();
                let mapped = map_text(body, ctx);
                let name = table_name(&view.name);
                let sql = if *replace {
                    format!("CREATE OR REPLACE VIEW {name} AS\n{mapped}")
                } else {
                    format!("CREATE VIEW {name} AS\n{mapped}")
                };
                let mut rendered = statement(sql);
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
            MigrationStep::AddForeignKey { table, key } => {
                let key = self.map_foreign_key(key, ctx);
                vec![warned(
                    format!(
                        "ALTER TABLE {} ADD {}",
                        table_name(table),
                        foreign_key_clause(ctx.target_schema.as_deref(), &key)
                    ),
                    Risk::Review,
                    format!(
                        "The foreign key {} on {table} fails if existing rows have no match",
                        key.name.as_deref().unwrap_or("(unnamed)")
                    ),
                )]
            }
            MigrationStep::SetComment { target, comment } => vec![self.comment(ctx, target, comment.as_deref())],
            // ALTER TABLE … ENGINE / DEFAULT CHARACTER SET / COMMENT are valid, but the gateway's parser cannot read them:
            // one unreadable statement would make it classify the whole script as unparsed text (and lose the table
            // names of its DROPs). They are manual steps with the statement to run.
            MigrationStep::SetTableOption { table, name, value } => match name.to_ascii_lowercase().as_str() {
                "engine" => vec![manual(
                    &format!("Change the storage engine of {table} to {value} (this copies the whole table)"),
                    Some(&format!("ALTER TABLE {} ENGINE = {value};", table_name(table))),
                )],
                "collation" => vec![manual(
                    &format!(
                        "Change the default collation of {table} to {value}; existing columns keep theirs \
                         (CONVERT TO CHARACTER SET converts them)"
                    ),
                    Some(&format!(
                        "ALTER TABLE {} DEFAULT CHARACTER SET {} COLLATE {value};",
                        table_name(table),
                        charset_of(value)
                    )),
                )],
                _ => vec![manual(&format!("Set option {name} = {value} on {table} by hand"), None)],
            },
            MigrationStep::Manual { reason, ddl_hint } => vec![manual(reason, ddl_hint.as_deref())],
        }
    }

    fn wrap(&self, body: Vec<RenderedStatement>, _transactional: bool) -> Vec<RenderedStatement> {
        // DDL commits implicitly: a transaction around it would only pretend to be atomic.
        body
    }
}
