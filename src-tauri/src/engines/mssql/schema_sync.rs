//! The SQL Server schema compare dialect: normalizes catalog text for comparison, rates column type changes, writes
//! canonical DDL and renders migration steps as T-SQL.
//!
//! Normalization: types as the catalog spells them (`nvarchar(max)`, `datetime2` for the default precision 7,
//! `decimal(18,0)`), defaults and expressions lower-case outside string literals, without brackets, comments, `N`
//! prefixes, redundant outer parentheses (`((0))` → `0`) or the scope schema's qualifier. View bodies are compared the same
//! way, so formatting, case and comments do not count.
//!
//! Scripts: DDL is transactional (`SET XACT_ABORT ON; BEGIN TRANSACTION; … COMMIT TRANSACTION`). A view must be alone in
//! its batch, so the script has `GO` lines around each `CREATE VIEW`; statements the gateway's parser cannot read
//! (`ALTER COLUMN`, `ADD CONSTRAINT … DEFAULT … FOR`, computed columns, clustered indexes) also get a batch of their own,
//! where the classifier recognizes them as DDL. After each `GO`, a guard batch (`IF @@ERROR <> 0 … SET NOEXEC ON`) stops a
//! tool that carries on after a failed batch (SSMS, sqlcmd without `-b`) from running the rest outside the rolled-back
//! transaction; the last batch is `SET NOEXEC OFF`. Defaults are named constraints: a script drops the target's by name
//! and adds the source's as `DF_<table>_<column>` unless the source named it. `ALTER COLUMN` restates the type and
//! nullability; keys, checks and indexes over the column are dropped and created again by the planner
//! (`columns_held_by_dependents`, which also moves persisted computed columns out of the way of a table with an IDENTITY
//! column, SQL Server error 11424), and a default constraint around a type or collation change by the renderer. IDENTITY
//! cannot be added or removed in place: that is a manual step.

use super::catalog::bracket as ident;
use crate::{
    contract::{
        replace_schema_qualifier, strip_schema_qualifier, ColumnChanges, ColumnDef, CommentTarget, ConstraintDef, ConstraintKind,
        DialectFeatures, ForeignKeyDef, IndexDef, IndexPart, KeyDef, MigrationStep, RenderContext, RenderedStatement, Risk, SchemaDialect,
        SqlDialectId, TableDef, ViewDef,
    },
    engines::common::sqlclassify::parse,
};

pub struct MssqlSchemaDialect;

pub static MSSQL_SCHEMA_DIALECT: MssqlSchemaDialect = MssqlSchemaDialect;

const SEPARATOR: &str = "GO";

// --- Text helpers -------------------------------------------------------------------------------------------------------

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '$' | '#' | '@')
}

/// The content of the quoted run starting at `chars[at]` (closed by `close`, doubled to escape) and the index after it.
fn read_quoted(chars: &[char], at: usize, close: char) -> (String, usize) {
    let mut content = String::new();
    let mut i = at + 1;
    while i < chars.len() {
        if chars[i] == close {
            if chars.get(i + 1) == Some(&close) {
                content.push(close);
                i += 2;
                continue;
            }
            return (content, i + 1);
        }
        content.push(chars[i]);
        i += 1;
    }
    (content, chars.len())
}

/// A name that needs no brackets.
fn is_plain_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') && chars.all(is_word)
}

/// A canonical spelling of a default, an expression or a view body for comparison: comments dropped, lower-case outside
/// string literals, `[name]` and `"name"` without their quotes (when the name needs none), `N'…'` as `'…'`, whitespace
/// kept only between two words, and redundant outer parentheses removed.
fn canonical(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    let mut i = 0;
    let push_word = |out: &mut String, space: &mut bool, word: &str| {
        if *space && out.chars().last().is_some_and(is_word) && word.chars().next().is_some_and(is_word) {
            out.push(' ');
        }
        *space = false;
        out.push_str(word);
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c.is_whitespace() {
            space = true;
            i += 1;
        } else if c == '-' && next == Some('-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            space = true;
        } else if c == '/' && next == Some('*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            space = true;
        } else if c == '\'' || (matches!(c, 'N' | 'n') && next == Some('\'') && (i == 0 || !is_word(chars[i - 1]))) {
            let start = if c == '\'' { i } else { i + 1 };
            let (content, end) = read_quoted(&chars, start, '\'');
            push_word(&mut out, &mut space, &format!("'{}'", content.replace('\'', "''")));
            i = end;
        } else if c == '[' || c == '"' {
            let (content, end) = read_quoted(&chars, i, if c == '[' { ']' } else { '"' });
            let name = content.to_lowercase();
            if is_plain_name(&name) {
                push_word(&mut out, &mut space, &name);
            } else {
                push_word(&mut out, &mut space, &format!("[{}]", name.replace(']', "]]")));
            }
            i = end;
        } else {
            push_word(&mut out, &mut space, &c.to_lowercase().to_string());
            i += 1;
        }
    }
    strip_outer_parens(&out).to_owned()
}

/// `((0))` → `0`, while the parentheses wrap the whole text.
fn strip_outer_parens(text: &str) -> &str {
    let mut text = text.trim();
    while text.starts_with('(') && text.ends_with(')') && wraps_whole(text) {
        text = text[1..text.len() - 1].trim();
    }
    text
}

/// Whether the first `(` of `text` closes at its last character (outside literals and brackets).
fn wraps_whole(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut depth = 0i32;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\'' => {
                i = read_quoted(&chars, i, '\'').1;
                continue;
            }
            '[' => {
                i = read_quoted(&chars, i, ']').1;
                continue;
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && i + 1 < chars.len() {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    depth == 0 && text.starts_with('(')
}

/// `text` in parentheses unless they already wrap it.
fn parenthesized(text: &str) -> String {
    let text = text.trim();
    if wraps_whole(text) {
        text.to_owned()
    } else {
        format!("({text})")
    }
}

/// A Unicode string literal.
fn literal(text: &str) -> String {
    format!("N'{}'", text.replace('\'', "''"))
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
        (Some(source), Some(target)) if !source.eq_ignore_ascii_case(target) => {
            replace_schema_qualifier(text, source, Some(&ident(target)))
        }
        _ => text.to_owned(),
    }
}

// --- Types --------------------------------------------------------------------------------------------------------------

/// Lower-case, without spaces, aliases folded (`integer` → `int`, `numeric` → `decimal`, `timestamp` → `rowversion`),
/// default modifiers spelled out or dropped as the catalog does (`decimal` → `decimal(18,0)`, `datetime2(7)` →
/// `datetime2`, `float(53)` → `float`, `nvarchar(-1)` → `nvarchar(max)`).
fn canonical_type(raw: &str) -> String {
    let text: String = raw.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let text = text
        .replace(" (", "(")
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(", ", ",")
        .replace(" ,", ",");
    let (base, args) = match text.split_once('(') {
        Some((base, rest)) => (base.trim().to_owned(), Some(rest.trim_end_matches(')').to_owned())),
        None => (text.trim().to_owned(), None),
    };
    let base = match base.as_str() {
        "integer" => "int".to_owned(),
        "numeric" | "dec" => "decimal".to_owned(),
        "timestamp" => "rowversion".to_owned(),
        "double precision" => "float".to_owned(),
        "character varying" | "char varying" => "varchar".to_owned(),
        "character" => "char".to_owned(),
        "national character varying" | "national char varying" => "nvarchar".to_owned(),
        "national character" | "national char" => "nchar".to_owned(),
        _ => base,
    };
    match (base.as_str(), args.as_deref()) {
        ("varchar" | "nvarchar" | "varbinary" | "char" | "nchar" | "binary", Some("-1" | "max")) => format!("{base}(max)"),
        ("varchar" | "nvarchar" | "varbinary" | "char" | "nchar" | "binary", None) => format!("{base}(1)"),
        ("decimal", None) => "decimal(18,0)".to_owned(),
        ("decimal", Some(args)) if !args.contains(',') => format!("decimal({args},0)"),
        ("datetime2" | "time" | "datetimeoffset", Some("7")) => base,
        ("float", Some(bits)) => match bits.parse::<u32>() {
            Ok(bits) if bits <= 24 => "real".to_owned(),
            _ => "float".to_owned(),
        },
        (_, Some(args)) => format!("{base}({args})"),
        (_, None) => base,
    }
}

/// A normalized type split into its base name and numeric modifiers (`max` as `i64::MAX`).
struct ParsedType {
    base: String,
    args: Vec<i64>,
}

fn parse_type(normalized: &str) -> ParsedType {
    let (base, args) = match normalized.split_once('(') {
        Some((base, rest)) => (
            base,
            rest.trim_end_matches(')')
                .split(',')
                .filter_map(|a| match a.trim() {
                    "max" => Some(i64::MAX),
                    other => other.parse().ok(),
                })
                .collect(),
        ),
        None => (normalized, Vec::new()),
    };
    ParsedType {
        base: base.to_owned(),
        args,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    /// Bytes (`bit` = 0).
    Integer(u8),
    Decimal,
    /// Bytes.
    Float(u8),
    /// Unicode, and the capacity in characters (`None` = from the modifier).
    Text(bool, Option<i64>),
    /// Capacity in bytes (`None` = from the modifier).
    Binary(Option<i64>),
    Temporal,
    Other,
}

fn family(base: &str) -> Family {
    match base {
        "bit" => Family::Integer(0),
        "tinyint" => Family::Integer(1),
        "smallint" => Family::Integer(2),
        "int" => Family::Integer(4),
        "bigint" => Family::Integer(8),
        "decimal" => Family::Decimal,
        "real" => Family::Float(4),
        "float" => Family::Float(8),
        "char" | "varchar" => Family::Text(false, None),
        "nchar" | "nvarchar" => Family::Text(true, None),
        "text" => Family::Text(false, Some(i64::from(i32::MAX))),
        "ntext" => Family::Text(true, Some(i64::from(i32::MAX) / 2)),
        "binary" | "varbinary" => Family::Binary(None),
        "image" => Family::Binary(Some(i64::from(i32::MAX))),
        "date" | "smalldatetime" | "datetime" | "datetime2" | "datetimeoffset" | "time" => Family::Temporal,
        _ => Family::Other,
    }
}

/// Fractional-second digits of a temporal type (7 when the catalog leaves the default out).
fn precision(parsed: &ParsedType) -> i64 {
    match parsed.base.as_str() {
        "datetime2" | "time" | "datetimeoffset" => parsed.args.first().copied().unwrap_or(7),
        "datetime" => 3,
        _ => 0,
    }
}

fn temporal_risk(from: &ParsedType, to: &ParsedType) -> Risk {
    let lossy = |lossy: bool| if lossy { Risk::DataLoss } else { Risk::None };
    match (from.base.as_str(), to.base.as_str()) {
        (a, b) if a == b => lossy(precision(to) < precision(from)),
        ("date", "datetime" | "datetime2" | "datetimeoffset") => Risk::None,
        ("smalldatetime", "datetime" | "datetime2" | "datetimeoffset") => Risk::None,
        ("datetime", "datetime2" | "datetimeoffset") => lossy(precision(to) < 3),
        ("datetime" | "datetime2" | "smalldatetime" | "datetimeoffset", "date" | "time" | "smalldatetime") => Risk::DataLoss,
        ("datetime2" | "datetimeoffset", "datetime") => Risk::DataLoss,
        _ => Risk::Review,
    }
}

fn type_risk(from: &ParsedType, to: &ParsedType) -> Risk {
    let capacity = |fixed: Option<i64>, args: &[i64]| fixed.or_else(|| args.first().copied()).unwrap_or(1);
    match (family(&from.base), family(&to.base)) {
        (Family::Integer(f), Family::Integer(t)) => {
            if t < f {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Decimal, Family::Decimal) => {
            let digits = |args: &[i64]| (args.first().copied().unwrap_or(18), args.get(1).copied().unwrap_or(0));
            let ((fp, fs), (tp, ts)) = (digits(&from.args), digits(&to.args));
            if ts < fs || tp - ts < fp - fs {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Integer(bytes), Family::Decimal) => {
            let needed = match bytes {
                0 => 1,
                1 => 3,
                2 => 5,
                4 => 10,
                _ => 19,
            };
            let (p, s) = (to.args.first().copied().unwrap_or(18), to.args.get(1).copied().unwrap_or(0));
            if p - s >= needed {
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
        (Family::Decimal | Family::Float(_), Family::Integer(_)) => Risk::DataLoss,
        (Family::Text(from_unicode, f), Family::Text(to_unicode, t)) => {
            if capacity(t, &to.args) < capacity(f, &from.args) || (from_unicode && !to_unicode) {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Binary(f), Family::Binary(t)) => {
            if capacity(t, &to.args) < capacity(f, &from.args) {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Temporal, Family::Temporal) => temporal_risk(from, to),
        _ => Risk::Review,
    }
}

// --- Rendering ----------------------------------------------------------------------------------------------------------

/// What goes between a view's name and `AS`: its column list and `WITH` attributes.
fn view_header(view: &ViewDef) -> String {
    let mut header = String::new();
    if let Some(columns) = view.option("columns").filter(|c| !c.trim().is_empty()) {
        header.push_str(&format!(" ({columns})"));
    }
    let with: Vec<&str> = [
        ("encryption", "ENCRYPTION"),
        ("schemabinding", "SCHEMABINDING"),
        ("viewMetadata", "VIEW_METADATA"),
    ]
    .into_iter()
    .filter(|(key, _)| view.option(key).is_some_and(|v| v.eq_ignore_ascii_case("on")))
    .map(|(_, word)| word)
    .collect();
    if !with.is_empty() {
        header.push_str(&format!(" WITH {}", with.join(", ")));
    }
    header
}

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
    let mut sql = format!("-- MANUAL STEP: {}", crate::contract::one_line(reason));
    if let Some(hint) = hint {
        for line in hint.lines().map(crate::contract::comment_safe) {
            sql.push_str(&format!("\n--   {line}"));
        }
    }
    warned(sql, Risk::Review, format!("Manual step: {reason}"))
}

/// Whether the server chose a default constraint's name (`DF__orders__statu__1A2B3C4D`).
fn is_system_default_name(name: &str) -> bool {
    name.starts_with("DF__")
        && name
            .rsplit("__")
            .next()
            .is_some_and(|tail| tail.len() >= 8 && tail.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The name a script gives a column's default constraint: the one the source chose, else `DF_<table>_<column>`.
fn default_name(table: &str, column: &ColumnDef) -> String {
    match &column.default_name {
        Some(name) if !is_system_default_name(name) => name.clone(),
        _ => format!("DF_{table}_{}", column.name),
    }
}

/// `[name] type [COLLATE c] [IDENTITY(s,i)] NULL|NOT NULL [CONSTRAINT [DF] DEFAULT (…)]`, or a computed column
/// `[name] AS (expr) [PERSISTED [NOT NULL]]`. `table` names the default constraint; without it the default is left out.
fn column_sql(column: &ColumnDef, table: Option<&str>) -> String {
    if let Some(generated) = &column.generated {
        let mut sql = format!("{} AS {}", ident(&column.name), parenthesized(&generated.expression));
        if generated.stored {
            sql.push_str(" PERSISTED");
            if !column.nullable {
                sql.push_str(" NOT NULL");
            }
        }
        return sql;
    }
    let mut sql = format!("{} {}", ident(&column.name), column.type_name);
    if let Some(collation) = &column.collation {
        sql.push_str(&format!(" COLLATE {collation}"));
    }
    if let Some(identity) = &column.identity {
        sql.push_str(&format!(
            " IDENTITY({},{})",
            identity.start.unwrap_or(1),
            identity.increment.unwrap_or(1)
        ));
    }
    sql.push_str(if column.nullable { " NULL" } else { " NOT NULL" });
    if let (Some(table), Some(default)) = (table, column.default.as_deref().filter(|d| !d.trim().is_empty())) {
        sql.push_str(&format!(
            " CONSTRAINT {} DEFAULT {}",
            ident(&default_name(table, column)),
            parenthesized(default)
        ));
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

fn primary_key_clause(key: &KeyDef) -> String {
    format!(
        "{}PRIMARY KEY ({})",
        constraint_name(&key.name, key.system_named),
        idents(&key.columns)
    )
}

fn unique_clause(key: &KeyDef) -> String {
    format!("{}UNIQUE ({})", constraint_name(&key.name, key.system_named), idents(&key.columns))
}

fn check_clause(name: &Option<String>, system_named: bool, expression: &str) -> String {
    format!("{}CHECK {}", constraint_name(name, system_named), parenthesized(expression))
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
    for (clause, action) in [("ON DELETE", &key.on_delete), ("ON UPDATE", &key.on_update)] {
        let action = action.as_deref().map(|a| a.replace('_', " ").to_uppercase());
        if let Some(action) = action.filter(|a| a != "NO ACTION" && a != "RESTRICT") {
            sql.push_str(&format!(" {clause} {action}"));
        }
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

/// `CREATE [UNIQUE] [CLUSTERED] INDEX … ON … (…) [INCLUDE (…)] [WHERE …]`; nonclustered is the default.
fn index_sql(schema: Option<&str>, table: &str, index: &IndexDef) -> String {
    let clustered = index.method.as_deref().is_some_and(|m| m.eq_ignore_ascii_case("CLUSTERED"));
    let mut sql = format!(
        "CREATE {}{}INDEX {} ON {} ({})",
        if index.unique { "UNIQUE " } else { "" },
        if clustered { "CLUSTERED " } else { "" },
        ident(&index.name),
        qualified(schema, table),
        index.parts.iter().map(index_part_sql).collect::<Vec<_>>().join(", ")
    );
    if !index.include.is_empty() {
        sql.push_str(&format!(" INCLUDE ({})", idents(&index.include)));
    }
    if let Some(predicate) = index.predicate.as_deref().filter(|p| !p.trim().is_empty()) {
        sql.push_str(&format!(" WHERE {}", parenthesized(predicate)));
    }
    sql
}

/// The `CREATE TABLE` statement alone (no trailing `;`): columns with their default constraints, keys, checks and
/// foreign keys.
fn create_table_sql(schema: Option<&str>, table: &TableDef) -> String {
    let mut lines: Vec<String> = table.columns.iter().map(|c| column_sql(c, Some(&table.name))).collect();
    lines.extend(table.primary_key.iter().map(primary_key_clause));
    lines.extend(table.uniques.iter().map(unique_clause));
    lines.extend(
        table
            .checks
            .iter()
            .map(|check| check_clause(&check.name, check.system_named, &check.expression)),
    );
    lines.extend(table.foreign_keys.iter().map(|key| foreign_key_clause(schema, key)));
    format!(
        "CREATE TABLE {} (\n    {}\n)",
        qualified(schema, &table.name),
        lines.join(",\n    ")
    )
}

/// The `@levelN` arguments of an extended property on a table or view (and a column of it).
fn property_levels(schema: &str, kind: &str, name: &str, column: Option<&str>) -> String {
    let mut sql = format!(
        "@level0type = N'SCHEMA', @level0name = {}, @level1type = N'{kind}', @level1name = {}",
        literal(schema),
        literal(name)
    );
    if let Some(column) = column {
        sql.push_str(&format!(", @level2type = N'COLUMN', @level2name = {}", literal(column)));
    }
    sql
}

/// Sets (or, with `None`, removes) the `MS_Description` property, whether or not one is there.
fn comment_sql(schema: &str, kind: &str, name: &str, column: Option<&str>, comment: Option<&str>) -> String {
    let levels = property_levels(schema, kind, name, column);
    let column_args = match column {
        Some(column) => format!("N'COLUMN', {}", literal(column)),
        None => "NULL, NULL".to_owned(),
    };
    let exists = format!(
        "IF EXISTS (SELECT 1 FROM sys.fn_listextendedproperty(N'MS_Description', N'SCHEMA', {}, N'{kind}', {}, {column_args}))",
        literal(schema),
        literal(name)
    );
    match comment {
        Some(comment) => format!(
            "{exists}\n    EXEC sys.sp_updateextendedproperty @name = N'MS_Description', @value = {value}, {levels}\n\
             ELSE\n    EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = {value}, {levels}",
            value = literal(comment)
        ),
        None => format!("{exists}\n    EXEC sys.sp_dropextendedproperty @name = N'MS_Description', {levels}"),
    }
}

/// `EXEC sp_addextendedproperty` for the canonical DDL (shown, not run on a target that may have the property).
fn add_comment_sql(schema: &str, kind: &str, name: &str, column: Option<&str>, comment: &str) -> String {
    format!(
        "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = {}, {};",
        literal(comment),
        property_levels(schema, kind, name, column)
    )
}

/// Whether a statement must be alone in its batch: `CREATE VIEW` (a server rule), and what the gateway's parser cannot
/// read (so the classifier sees it on its own and recognizes it).
fn needs_own_batch(sql: &str) -> bool {
    let code: String = sql
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    let words: Vec<String> = code.split_whitespace().take(4).map(str::to_ascii_uppercase).collect();
    let view = matches!(words.as_slice(), [create, view, ..] if create == "CREATE" && view == "VIEW")
        || matches!(words.as_slice(), [create, or, alter, view] if create == "CREATE" && or == "OR" && alter == "ALTER" && view == "VIEW");
    !code.trim().is_empty() && (view || parse(SqlDialectId::MsSql, &code).is_err())
}

impl MssqlSchemaDialect {
    fn schema<'c>(&self, ctx: &'c RenderContext) -> &'c str {
        ctx.target_schema.as_deref().unwrap_or(super::catalog::DEFAULT_SCHEMA)
    }

    fn table_name(&self, ctx: &RenderContext, name: &str) -> String {
        qualified(Some(self.schema(ctx)), name)
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
        index.predicate = index.predicate.as_deref().map(|p| map_text(p, ctx));
        index
    }

    fn map_foreign_key(&self, key: &ForeignKeyDef, ctx: &RenderContext) -> ForeignKeyDef {
        let mut key = key.clone();
        let source = ctx.source_schema.as_deref();
        if key
            .ref_schema
            .as_deref()
            .is_some_and(|s| source.is_some_and(|source| s.eq_ignore_ascii_case(source)))
        {
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
        let mut column = changes.apply(from, &to);
        let name = self.table_name(ctx, table);
        let mut out = Vec::new();
        if changes.identity {
            out.push(manual(
                &format!(
                    "SQL Server cannot add or remove IDENTITY on {table}.{} in place; create a new table with the wanted \
                     column, copy the rows (SET IDENTITY_INSERT), drop the old table and rename the new one",
                    to.name
                ),
                None,
            ));
            column.identity = from.identity.clone();
        }
        let drop_default = |out: &mut Vec<RenderedStatement>| match &from.default_name {
            Some(default) => out.push(statement(format!("ALTER TABLE {name} DROP CONSTRAINT {}", ident(default)))),
            None => out.push(manual(&format!("Drop the default of {table}.{} by hand", from.name), None)),
        };
        if changes.generated || (from.generated.is_some() && (changes.type_name || changes.nullability)) {
            // A computed column cannot be altered: drop it and add it again.
            let risk = if from.generated.is_some() { Risk::Review } else { Risk::DataLoss };
            if from.default.is_some() {
                drop_default(&mut out);
            }
            out.push(warned(
                format!("ALTER TABLE {name} DROP COLUMN {}", ident(&from.name)),
                risk,
                format!(
                    "{} is dropped and added again{}",
                    to.name,
                    if risk == Risk::DataLoss { "; its values are lost" } else { "" }
                ),
            ));
            out.push(statement(format!("ALTER TABLE {name} ADD {}", column_sql(&column, Some(table)))));
            return out;
        }
        let retype = changes.type_name || changes.collation || changes.nullability;
        // The type or collation of a column with a default constraint cannot change: the default goes and comes back
        // around it.
        let cycle = (changes.type_name || changes.collation) && from.default.is_some();
        if from.default.is_some() && (changes.default || cycle) {
            drop_default(&mut out);
        }
        if retype {
            let mut sql = format!(
                "ALTER TABLE {name} ALTER COLUMN {}",
                column_sql(
                    &ColumnDef {
                        identity: None,
                        ..column.clone()
                    },
                    None
                )
            );
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
                let stub = crate::contract::null_fill_stub(&name, &ident(&column.name));
                if let Some(stub) = &stub {
                    sql = format!("{stub}\n{sql}");
                }
                risk = risk.max(Risk::Review);
                warnings.push(format!(
                    "Fails if {table}.{} has NULLs; fill them first{}",
                    column.name,
                    if stub.is_some() { " (see the UPDATE stub)" } else { "" }
                ));
            }
            out.push(RenderedStatement { sql, risk, warnings });
        }
        if let Some(default) = column.default.as_deref().filter(|_| changes.default || cycle) {
            out.push(statement(format!(
                "ALTER TABLE {name} ADD CONSTRAINT {} DEFAULT {} FOR {}",
                ident(&default_name(table, &column)),
                parenthesized(default),
                ident(&column.name)
            )));
        }
        out
    }

    fn comment(&self, ctx: &RenderContext, target: &CommentTarget, comment: Option<&str>) -> RenderedStatement {
        let kind = match target.kind.as_str() {
            "view" | "materializedView" => "VIEW",
            _ => "TABLE",
        };
        statement(comment_sql(self.schema(ctx), kind, &target.name, target.column.as_deref(), comment))
    }
}

impl SchemaDialect for MssqlSchemaDialect {
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
        canonical(text.trim().trim_end_matches(';'))
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
            transactional_ddl: true,
            alter_column_type: true,
            drop_column: true,
            restrict_equals_no_action: true,
            batch_separator: Some(SEPARATOR),
            inline_comments: false,
            columns_held_by_dependents: true,
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
        let scope = schema.unwrap_or(super::catalog::DEFAULT_SCHEMA);
        if let Some(comment) = table.comment.as_deref().filter(|c| !c.is_empty()) {
            parts.push(add_comment_sql(scope, "TABLE", &table.name, None, comment));
        }
        for column in &table.columns {
            if let Some(comment) = column.comment.as_deref().filter(|c| !c.is_empty()) {
                parts.push(add_comment_sql(scope, "TABLE", &table.name, Some(&column.name), comment));
            }
        }
        parts.join("\n\n")
    }

    fn view_ddl(&self, schema: Option<&str>, view: &ViewDef) -> String {
        let body = view.definition.trim().trim_end_matches(';').trim_end();
        let mut text = format!("CREATE VIEW {}{} AS\n{body};", qualified(schema, &view.name), view_header(view));
        if let Some(comment) = view.comment.as_deref().filter(|c| !c.is_empty()) {
            let scope = schema.unwrap_or(super::catalog::DEFAULT_SCHEMA);
            text.push_str(&format!("\n\n{}", add_comment_sql(scope, "VIEW", &view.name, None, comment)));
        }
        text
    }

    fn render(&self, step: &MigrationStep, ctx: &RenderContext) -> Vec<RenderedStatement> {
        let table_name = |name: &str| self.table_name(ctx, name);
        let schema = Some(self.schema(ctx));
        match step {
            MigrationStep::DropForeignKey { table, name } => {
                vec![statement(format!(
                    "ALTER TABLE {} DROP CONSTRAINT {}",
                    table_name(table),
                    ident(name)
                ))]
            }
            MigrationStep::DropView { name, .. } => vec![statement(format!("DROP VIEW {}", table_name(name)))],
            MigrationStep::DropIndex { table, name } => {
                vec![statement(format!("DROP INDEX {} ON {}", ident(name), table_name(table)))]
            }
            MigrationStep::DropConstraint { table, kind, name } => match name {
                Some(name) => vec![statement(format!(
                    "ALTER TABLE {} DROP CONSTRAINT {}",
                    table_name(table),
                    ident(name)
                ))],
                None => {
                    let what = match kind {
                        ConstraintKind::PrimaryKey => "primary key",
                        ConstraintKind::Unique => "unique constraint",
                        ConstraintKind::Check => "check constraint",
                    };
                    vec![manual(&format!("Drop the unnamed {what} of {table}"), None)]
                }
            },
            MigrationStep::DropTable { name } => vec![warned(
                format!("DROP TABLE {}", table_name(name)),
                Risk::DataLoss,
                format!("Drops table {name} and all its rows"),
            )],
            MigrationStep::CreateTable { table } => {
                let table = self.map_table(table, ctx);
                vec![statement(create_table_sql(schema, &table))]
            }
            MigrationStep::AddColumn { table, column } => {
                let column = self.map_column(column, ctx);
                let sql = format!("ALTER TABLE {} ADD {}", table_name(table), column_sql(&column, Some(table)));
                let required = !column.nullable && column.default.is_none() && column.identity.is_none() && column.generated.is_none();
                if required {
                    vec![warned(
                        sql,
                        Risk::Review,
                        format!("{table}.{} is NOT NULL without a default: fails if the table has rows", column.name),
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
            MigrationStep::RebuildTable { target_table, .. } => {
                vec![manual(&format!("Rebuild table {} by hand", target_table.name), None)]
            }
            MigrationStep::AddPrimaryKey { table, key } => vec![warned(
                format!("ALTER TABLE {} ADD {}", table_name(table), primary_key_clause(key)),
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
                let sql = index_sql(schema, table, &index);
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
                let create = if *replace { "CREATE OR ALTER VIEW" } else { "CREATE VIEW" };
                let mut rendered = statement(format!("{create} {name}{} AS\n{mapped}", view_header(view)));
                if mapped != body {
                    rendered.risk = Risk::Review;
                    rendered.warnings.push(format!(
                        "Schema references in the body of {} were changed from {} to {}; review them",
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
                    format!("ALTER TABLE {} ADD {}", table_name(table), foreign_key_clause(schema, &key)),
                    Risk::Review,
                    format!(
                        "The foreign key {} on {table} fails if existing rows have no match",
                        key.name.as_deref().unwrap_or("(unnamed)")
                    ),
                )]
            }
            MigrationStep::SetComment { target, comment } => vec![self.comment(ctx, target, comment.as_deref())],
            MigrationStep::SetTableOption { table, name, value } => {
                vec![manual(&format!("Set option {name} = {value} on {table} by hand"), None)]
            }
            MigrationStep::Manual { reason, ddl_hint } => vec![manual(reason, ddl_hint.as_deref())],
        }
    }

    fn wrap(&self, body: Vec<RenderedStatement>, transactional: bool) -> Vec<RenderedStatement> {
        let mut batches: Vec<Vec<RenderedStatement>> = vec![Vec::new()];
        if transactional {
            batches[0].push(statement("SET XACT_ABORT ON".into()));
            batches[0].push(statement("BEGIN TRANSACTION".into()));
        }
        for rendered in body {
            if needs_own_batch(&rendered.sql) {
                if batches.last().is_some_and(|batch| !batch.is_empty()) {
                    batches.push(Vec::new());
                }
                batches.push(vec![rendered]);
                batches.push(Vec::new());
            } else if let Some(batch) = batches.last_mut() {
                batch.push(rendered);
            }
        }
        if transactional {
            if let Some(batch) = batches.last_mut() {
                batch.push(statement("COMMIT TRANSACTION".into()));
            }
        }
        batches.retain(|batch| !batch.is_empty());
        // After a failed batch, SSMS and sqlcmd go on with the next one: the guard stops them running the rest.
        let guard = if transactional {
            "IF @@ERROR <> 0 OR @@TRANCOUNT = 0 SET NOEXEC ON"
        } else {
            "IF @@ERROR <> 0 SET NOEXEC ON"
        };
        let several = batches.len() > 1;
        let mut out = Vec::new();
        for (at, batch) in batches.into_iter().enumerate() {
            if at > 0 {
                out.push(statement(SEPARATOR.into()));
                out.push(statement(guard.into()));
                out.push(statement(SEPARATOR.into()));
            }
            out.extend(batch);
        }
        if several {
            out.push(statement(SEPARATOR.into()));
            out.push(statement("SET NOEXEC OFF".into()));
        }
        out
    }
}
