//! The PostgreSQL schema compare dialect: normalizes catalog text for comparison, rates column type changes, writes
//! canonical DDL and renders migration steps.
//!
//! Snapshots are read with `search_path = pg_catalog`, so user objects in types, defaults and expressions carry their
//! schema. Normalizers drop the side's scope schema (and the cast of a literal default, `'a'::character varying` →
//! `'a'`); renderers point the source scope at the target scope, also inside `'schema.seq'::regclass` literals.
//! PostgreSQL DDL is transactional, so a script is wrapped in `BEGIN` … `COMMIT`.

use super::catalog::ident;
use crate::contract::{
    replace_schema_qualifier, ColumnDef, CommentTarget, ConstraintDef, ConstraintKind, DialectFeatures, ForeignKeyDef, IdentityDef,
    IdentityKind, IndexDef, KeyDef, MigrationStep, RenderContext, RenderedStatement, Risk, SchemaDialect, TableDef, ViewDef,
};

pub struct PgSchemaDialect;

pub static PG_SCHEMA_DIALECT: PgSchemaDialect = PgSchemaDialect;

// --- Text helpers -------------------------------------------------------------------------------------------------------

/// The end (exclusive) of the quoted run starting at `text[0]` (`'…'` or `"…"`, a doubled quote escapes).
fn quoted_end(text: &str, quote: char) -> usize {
    let mut end = 1;
    loop {
        match text[end..].find(quote) {
            Some(at) => {
                end += at + 1;
                if text[end..].starts_with(quote) {
                    end += 1;
                    continue;
                }
                return end;
            }
            None => return text.len(),
        }
    }
}

/// Runs of whitespace outside literals and quoted identifiers as one space, trimmed.
fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text.trim();
    let mut space = false;
    while let Some(c) = rest.chars().next() {
        if c.is_whitespace() {
            space = true;
            rest = &rest[c.len_utf8()..];
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        let end = if c == '\'' || c == '"' { quoted_end(rest, c) } else { c.len_utf8() };
        out.push_str(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

/// `'schema.name'::regclass` literals with the schema prefix replaced (`replacement` is the target schema as it reads
/// inside a regclass literal) or dropped.
fn map_regclass(text: &str, schema: &str, replacement: Option<&str>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '"' {
            let end = quoted_end(rest, '"');
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        if c != '\'' {
            out.push(c);
            rest = &rest[c.len_utf8()..];
            continue;
        }
        let end = quoted_end(rest, '\'');
        let literal = &rest[..end];
        if rest[end..].starts_with("::regclass") && literal.len() >= 2 {
            let inner = &literal[1..literal.len() - 1];
            let prefixes = [format!("{schema}."), format!("\"{schema}\".")];
            let stripped = prefixes.iter().find_map(|prefix| {
                inner
                    .get(..prefix.len())
                    .filter(|head| {
                        if prefix.starts_with('"') {
                            *head == prefix
                        } else {
                            head.eq_ignore_ascii_case(prefix)
                        }
                    })
                    .map(|_| &inner[prefix.len()..])
            });
            match stripped {
                Some(name) => {
                    out.push('\'');
                    if let Some(replacement) = replacement {
                        out.push_str(&replacement.replace('\'', "''"));
                        out.push('.');
                    }
                    out.push_str(name);
                    out.push('\'');
                }
                None => out.push_str(literal),
            }
        } else {
            out.push_str(literal);
        }
        rest = &rest[end..];
    }
    out
}

/// Without the scope's schema, for comparison.
fn unqualified(text: &str, schema: Option<&str>) -> String {
    let text = collapse(text);
    match schema {
        Some(schema) => map_regclass(&replace_schema_qualifier(&text, schema, None), schema, None),
        None => text,
    }
}

/// `'abc'::character varying` → `'abc'` (a literal and its casts and nothing else).
fn strip_literal_cast(text: &str) -> String {
    if text.starts_with('\'') {
        let end = quoted_end(text, '\'');
        // Casts only: every part after `::` is a type name (`character varying`, `text[]`, `numeric(12,2)`).
        let casts = &text[end..];
        if casts.starts_with("::")
            && casts
                .split("::")
                .skip(1)
                .all(|cast| !cast.is_empty() && cast.chars().all(|c| c.is_alphanumeric() || " _[]().,\"".contains(c)))
            && !casts.contains("::regclass")
        {
            return text[..end].to_owned();
        }
    }
    text.to_owned()
}

/// Source-scope references pointed at the target scope (types, defaults, expressions, view bodies).
fn map_text(text: &str, ctx: &RenderContext) -> String {
    match (&ctx.source_schema, &ctx.target_schema) {
        (Some(source), Some(target)) if source != target => {
            let mapped = replace_schema_qualifier(text, source, Some(&ident(target)));
            map_regclass(&mapped, source, Some(&ident(target)))
        }
        _ => text.to_owned(),
    }
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

// --- Types --------------------------------------------------------------------------------------------------------------

/// A type split into its name without modifiers, its numeric modifiers and whether it is an array.
struct ParsedType {
    base: String,
    args: Vec<i64>,
    array: bool,
}

fn parse_type(normalized: &str) -> ParsedType {
    let array = normalized.ends_with("[]");
    let text = normalized.trim_end_matches("[]");
    let mut base = String::new();
    let mut args = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        base.push_str(&rest[..open]);
        let close = rest[open..].find(')').map_or(rest.len(), |at| open + at);
        args.extend(rest[open + 1..close].split(',').filter_map(|a| a.trim().parse::<i64>().ok()));
        rest = rest.get(close + 1..).unwrap_or("");
    }
    base.push_str(rest);
    ParsedType {
        base: base.split_whitespace().collect::<Vec<_>>().join(" "),
        args,
        array,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Integer(u8),
    Numeric,
    Float(u8),
    Text,
    Other,
}

fn family(base: &str) -> Family {
    match base {
        "smallint" => Family::Integer(2),
        "integer" => Family::Integer(4),
        "bigint" => Family::Integer(8),
        "numeric" => Family::Numeric,
        "real" => Family::Float(4),
        "double precision" => Family::Float(8),
        "text" | "character varying" | "character" | "bpchar" | "name" | "citext" => Family::Text,
        _ => Family::Other,
    }
}

/// Decimal digits an integer type holds.
fn integer_digits(bytes: u8) -> i64 {
    match bytes {
        2 => 5,
        4 => 10,
        _ => 19,
    }
}

fn numbers(family: Family) -> bool {
    matches!(family, Family::Integer(_) | Family::Numeric | Family::Float(_))
}

/// Whether `ALTER COLUMN … TYPE` needs `USING` (no implicit or assignment cast between the types).
fn needs_using(from: &ParsedType, to: &ParsedType) -> bool {
    let (a, b) = (family(&from.base), family(&to.base));
    if from.array != to.array {
        return true;
    }
    if from.base == to.base || b == Family::Text || (numbers(a) && numbers(b)) {
        return false;
    }
    true
}

fn type_risk(from: &ParsedType, to: &ParsedType) -> Risk {
    if from.array != to.array {
        return Risk::Review;
    }
    let bounded_down = |from: &[i64], to: &[i64]| -> Risk {
        if to.is_empty() {
            return Risk::None;
        }
        if from.is_empty() || from.iter().zip(to).any(|(f, t)| t < f) {
            return Risk::DataLoss;
        }
        Risk::None
    };
    match (family(&from.base), family(&to.base)) {
        _ if from.base == to.base => match family(&from.base) {
            Family::Numeric => match (from.args.as_slice(), to.args.as_slice()) {
                (_, []) => Risk::None,
                ([], _) => Risk::DataLoss,
                (f, t) => {
                    let (fp, fs) = (f[0], f.get(1).copied().unwrap_or(0));
                    let (tp, ts) = (t[0], t.get(1).copied().unwrap_or(0));
                    if ts < fs || tp - ts < fp - fs {
                        Risk::DataLoss
                    } else {
                        Risk::None
                    }
                }
            },
            _ => bounded_down(&from.args, &to.args),
        },
        (Family::Integer(f), Family::Integer(t)) => {
            if t < f {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Integer(bytes), Family::Numeric) => match to.args.as_slice() {
            [] => Risk::None,
            [p] if *p >= integer_digits(bytes) => Risk::None,
            [p, s, ..] if p - s >= integer_digits(bytes) => Risk::None,
            _ => Risk::DataLoss,
        },
        (Family::Integer(_) | Family::Numeric, Family::Float(_)) | (Family::Float(_), Family::Numeric) => Risk::Review,
        (Family::Float(f), Family::Float(t)) => {
            if t < f {
                Risk::DataLoss
            } else {
                Risk::None
            }
        }
        (Family::Numeric | Family::Float(_), Family::Integer(_)) => Risk::DataLoss,
        (Family::Text, Family::Text) => match (to.base.as_str(), to.args.as_slice()) {
            ("text", _) | ("character varying", []) => Risk::None,
            (_, [n]) => match (from.base.as_str(), from.args.as_slice()) {
                ("character varying" | "character", [m]) if n >= m => Risk::None,
                _ => Risk::DataLoss,
            },
            _ => Risk::Review,
        },
        (_, Family::Text) if to.args.is_empty() => Risk::None,
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

/// The serial pseudo-type of an integer type (`serial`, `bigserial`, `smallserial`).
fn serial_type(type_name: &str) -> Option<&'static str> {
    match type_name.trim().to_lowercase().as_str() {
        "integer" | "int" | "int4" => Some("serial"),
        "bigint" | "int8" => Some("bigserial"),
        "smallint" | "int2" => Some("smallserial"),
        _ => None,
    }
}

fn identity_clause(identity: &IdentityDef) -> String {
    let kind = if identity.kind == IdentityKind::Always {
        "ALWAYS"
    } else {
        "BY DEFAULT"
    };
    let mut options = Vec::new();
    if let Some(start) = identity.start.filter(|s| *s != 1) {
        options.push(format!("START WITH {start}"));
    }
    if let Some(increment) = identity.increment.filter(|i| *i != 1) {
        options.push(format!("INCREMENT BY {increment}"));
    }
    if options.is_empty() {
        format!("GENERATED {kind} AS IDENTITY")
    } else {
        format!("GENERATED {kind} AS IDENTITY ({})", options.join(" "))
    }
}

fn is_serial(column: &ColumnDef) -> bool {
    column.identity.as_ref().is_some_and(|i| i.kind == IdentityKind::Serial)
}

/// `name type [COLLATE c] [GENERATED … | DEFAULT …] [NOT NULL]`.
fn column_sql(column: &ColumnDef) -> String {
    let serial = is_serial(column).then(|| serial_type(&column.type_name)).flatten();
    let mut sql = format!("{} {}", ident(&column.name), serial.unwrap_or(&column.type_name));
    if let Some(collation) = &column.collation {
        sql.push_str(&format!(" COLLATE {}", ident(collation)));
    }
    match (&column.generated, &column.identity) {
        (Some(generated), _) => sql.push_str(&format!(" GENERATED ALWAYS AS ({}) STORED", generated.expression)),
        (None, Some(identity)) if identity.kind != IdentityKind::Serial => {
            sql.push(' ');
            sql.push_str(&identity_clause(identity));
        }
        _ if serial.is_some() => {}
        _ => {
            if let Some(default) = &column.default {
                sql.push_str(&format!(" DEFAULT {default}"));
            }
        }
    }
    if !column.nullable {
        sql.push_str(" NOT NULL");
    }
    sql
}

fn constraint_name(name: &Option<String>) -> String {
    name.as_deref().map(|n| format!("CONSTRAINT {} ", ident(n))).unwrap_or_default()
}

fn key_clause(kind: &str, key: &KeyDef) -> String {
    format!("{}{kind} ({})", constraint_name(&key.name), idents(&key.columns))
}

fn foreign_key_clause(schema: Option<&str>, key: &ForeignKeyDef) -> String {
    let ref_schema = key.ref_schema.as_deref().or(schema);
    let mut sql = format!(
        "{}FOREIGN KEY ({}) REFERENCES {} ({})",
        constraint_name(&key.name),
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

fn index_sql(schema: Option<&str>, table: &str, index: &IndexDef) -> String {
    let parts: Vec<String> = index
        .parts
        .iter()
        .map(|part| {
            let key = match (&part.column, &part.expression) {
                (Some(column), _) => ident(column),
                (None, Some(expression)) => format!("({expression})"),
                (None, None) => String::new(),
            };
            if part.descending {
                format!("{key} DESC")
            } else {
                key
            }
        })
        .collect();
    let method = index
        .method
        .as_deref()
        .filter(|m| !m.eq_ignore_ascii_case("btree"))
        .map(|m| format!(" USING {}", m.to_lowercase()))
        .unwrap_or_default();
    let mut sql = format!(
        "CREATE {}INDEX {} ON {}{method} ({})",
        if index.unique { "UNIQUE " } else { "" },
        ident(&index.name),
        qualified(schema, table),
        parts.join(", ")
    );
    if !index.include.is_empty() {
        sql.push_str(&format!(" INCLUDE ({})", idents(&index.include)));
    }
    if let Some(predicate) = &index.predicate {
        sql.push_str(&format!(" WHERE {predicate}"));
    }
    sql
}

/// The `CREATE TABLE` statement alone (no trailing `;`): columns, keys, checks and whatever foreign keys the table has.
fn create_table_sql(schema: Option<&str>, table: &TableDef) -> String {
    let mut lines: Vec<String> = table.columns.iter().map(column_sql).collect();
    if let Some(key) = &table.primary_key {
        lines.push(key_clause("PRIMARY KEY", key));
    }
    lines.extend(table.uniques.iter().map(|key| key_clause("UNIQUE", key)));
    lines.extend(
        table
            .checks
            .iter()
            .map(|check| format!("{}CHECK ({})", constraint_name(&check.name), check.expression)),
    );
    lines.extend(table.foreign_keys.iter().map(|key| foreign_key_clause(schema, key)));
    format!(
        "CREATE TABLE {} (\n    {}\n)",
        qualified(schema, &table.name),
        lines.join(",\n    ")
    )
}

fn comment_sql(what: &str, name: &str, comment: Option<&str>) -> String {
    format!("COMMENT ON {what} {name} IS {}", comment.map_or("NULL".to_owned(), literal))
}

impl PgSchemaDialect {
    /// The table with every reference to the source scope pointed at the target scope.
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
            *key = self.map_foreign_key(key, ctx);
        }
        table
    }

    fn map_column(&self, column: &ColumnDef, ctx: &RenderContext) -> ColumnDef {
        let mut column = column.clone();
        column.type_name = map_text(&column.type_name, ctx);
        column.default = column.default.map(|d| map_text(&d, ctx));
        if let Some(generated) = &mut column.generated {
            generated.expression = map_text(&generated.expression, ctx);
        }
        column
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
        if key.ref_schema.is_some() && key.ref_schema == ctx.source_schema {
            key.ref_schema = None;
        }
        key
    }

    fn table_name(&self, ctx: &RenderContext, name: &str) -> String {
        qualified(ctx.target_schema.as_deref(), name)
    }

    fn alter_column(
        &self,
        ctx: &RenderContext,
        table: &str,
        from: &ColumnDef,
        to: &ColumnDef,
        changes: crate::contract::ColumnChanges,
    ) -> Vec<RenderedStatement> {
        let to = self.map_column(to, ctx);
        let alter = format!("ALTER TABLE {} ALTER COLUMN {}", self.table_name(ctx, table), ident(&to.name));
        let mut out = Vec::new();
        if changes.generated {
            match (&from.generated, &to.generated) {
                (Some(_), None) => out.push(statement(format!("{alter} DROP EXPRESSION"))),
                (_, Some(_)) => {
                    let table_name = self.table_name(ctx, table);
                    let risk = if from.generated.is_some() { Risk::Review } else { Risk::DataLoss };
                    out.push(warned(
                        format!("ALTER TABLE {table_name} DROP COLUMN {}", ident(&from.name)),
                        risk,
                        format!(
                            "The generated column {} is dropped and added again{}",
                            to.name,
                            if risk == Risk::DataLoss {
                                "; its stored values are lost"
                            } else {
                                ""
                            }
                        ),
                    ));
                    out.push(statement(format!("ALTER TABLE {table_name} ADD COLUMN {}", column_sql(&to))));
                    return out;
                }
                (None, None) => {}
            }
        }
        let identity = changes
            .identity
            .then(|| (from.identity.as_ref().map(|i| i.kind), to.identity.as_ref().map(|i| i.kind)));
        let serial_change = identity.is_some_and(|(a, b)| a == Some(IdentityKind::Serial) || b == Some(IdentityKind::Serial));
        if serial_change {
            out.push(manual(
                &format!("Change {}.{} between serial and identity (or plain) by hand", table, to.name),
                Some("Serial columns own a sequence: create or drop it, then set the default or identity."),
            ));
        }
        if let Some((Some(_), None)) = identity.filter(|_| !serial_change) {
            out.push(statement(format!("{alter} DROP IDENTITY")));
        }
        if changes.type_name || changes.collation {
            let (from_type, to_type) = (
                parse_type(&self.normalize_type(&from.type_name, ctx.target_schema.as_deref())),
                parse_type(&self.normalize_type(&to.type_name, ctx.target_schema.as_deref())),
            );
            let mut sql = format!("{alter} TYPE {}", to.type_name);
            if let Some(collation) = &to.collation {
                sql.push_str(&format!(" COLLATE {}", ident(collation)));
            }
            let mut warnings = Vec::new();
            let mut risk = self.type_change_risk(from, &to);
            if changes.type_name && needs_using(&from_type, &to_type) {
                sql.push_str(&format!(" USING {}::{}", ident(&to.name), to.type_name));
                warnings.push(format!(
                    "Converts {} from {} to {}; review the USING cast",
                    to.name, from.type_name, to.type_name
                ));
                risk = risk.max(Risk::Review);
            }
            if risk == Risk::DataLoss {
                warnings.insert(
                    0,
                    format!("Changing {} from {} to {} may lose data", to.name, from.type_name, to.type_name),
                );
            }
            out.push(RenderedStatement { sql, risk, warnings });
        }
        if changes.default && !serial_change {
            let identity_target = to.identity.as_ref().is_some_and(|i| i.kind != IdentityKind::Serial);
            match (&to.default, identity_target) {
                (Some(default), false) => out.push(statement(format!("{alter} SET DEFAULT {default}"))),
                _ => out.push(statement(format!("{alter} DROP DEFAULT"))),
            }
        }
        if changes.nullability {
            if to.nullable {
                out.push(statement(format!("{alter} DROP NOT NULL")));
            } else {
                out.push(warned(
                    format!(
                        "-- UPDATE {} SET {} = ... WHERE {} IS NULL;\n{alter} SET NOT NULL",
                        self.table_name(ctx, table),
                        ident(&to.name),
                        ident(&to.name)
                    ),
                    Risk::Review,
                    format!("Fails if {}.{} has NULLs; fill them first (see the UPDATE stub)", table, to.name),
                ));
            }
        }
        if let Some((from_kind, Some(kind))) = identity.filter(|_| !serial_change) {
            let identity = to.identity.clone().unwrap_or(IdentityDef {
                kind,
                start: None,
                increment: None,
            });
            let word = if kind == IdentityKind::Always { "ALWAYS" } else { "BY DEFAULT" };
            match from_kind {
                None => out.push(statement(format!("{alter} ADD {}", identity_clause(&identity)))),
                Some(_) => out.push(statement(format!("{alter} SET GENERATED {word}"))),
            }
        }
        out
    }
}

impl SchemaDialect for PgSchemaDialect {
    fn normalize_type(&self, raw: &str, schema: Option<&str>) -> String {
        unqualified(raw, schema).to_lowercase()
    }

    fn normalize_default(&self, raw: &str, schema: Option<&str>) -> String {
        strip_literal_cast(&unqualified(raw, schema))
    }

    fn normalize_expression(&self, raw: &str, schema: Option<&str>) -> String {
        unqualified(raw, schema).trim_end_matches(';').trim_end().to_owned()
    }

    fn type_change_risk(&self, from: &ColumnDef, to: &ColumnDef) -> Risk {
        let normalized = |column: &ColumnDef| parse_type(&collapse(&column.type_name).to_lowercase());
        let (from, to) = (normalized(from), normalized(to));
        if from.base == to.base && from.args == to.args && from.array == to.array {
            return Risk::None;
        }
        type_risk(&from, &to)
    }

    fn features(&self) -> DialectFeatures {
        DialectFeatures {
            transactional_ddl: true,
            alter_column_type: true,
            drop_column: true,
            restrict_equals_no_action: false,
            batch_separator: None,
            inline_comments: false,
        }
    }

    fn quote_ident(&self, name: &str) -> String {
        ident(name)
    }

    fn table_ddl(&self, schema: Option<&str>, table: &TableDef) -> String {
        let mut parts = vec![format!("{};", create_table_sql(schema, table))];
        let name = qualified(schema, &table.name);
        parts.extend(
            table
                .indexes
                .iter()
                .map(|index| format!("{};", index_sql(schema, &table.name, index))),
        );
        if let Some(comment) = table.comment.as_deref().filter(|c| !c.is_empty()) {
            parts.push(format!("{};", comment_sql("TABLE", &name, Some(comment))));
        }
        for column in &table.columns {
            if let Some(comment) = column.comment.as_deref().filter(|c| !c.is_empty()) {
                parts.push(format!(
                    "{};",
                    comment_sql("COLUMN", &format!("{name}.{}", ident(&column.name)), Some(comment))
                ));
            }
        }
        parts.join("\n\n")
    }

    fn view_ddl(&self, schema: Option<&str>, view: &ViewDef) -> String {
        let name = qualified(schema, &view.name);
        let body = view.definition.trim().trim_end_matches(';').trim_end();
        let mut text = if view.materialized {
            format!("CREATE MATERIALIZED VIEW {name} AS\n{body};")
        } else {
            format!("CREATE VIEW {name} AS\n{body};")
        };
        if let Some(comment) = view.comment.as_deref().filter(|c| !c.is_empty()) {
            let what = if view.materialized { "MATERIALIZED VIEW" } else { "VIEW" };
            text.push_str(&format!("\n\n{};", comment_sql(what, &name, Some(comment))));
        }
        text
    }

    fn render(&self, step: &MigrationStep, ctx: &RenderContext) -> Vec<RenderedStatement> {
        let table_name = |name: &str| self.table_name(ctx, name);
        match step {
            MigrationStep::DropForeignKey { table, name } => {
                vec![statement(format!(
                    "ALTER TABLE {} DROP CONSTRAINT {}",
                    table_name(table),
                    ident(name)
                ))]
            }
            MigrationStep::DropView { name, materialized } => {
                let what = if *materialized { "MATERIALIZED VIEW" } else { "VIEW" };
                vec![statement(format!("DROP {what} {}", table_name(name)))]
            }
            MigrationStep::DropIndex { name, .. } => vec![statement(format!("DROP INDEX {}", table_name(name)))],
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
            MigrationStep::RebuildTable { target_table, .. } => vec![manual(&format!("Rebuild table {} by hand", target_table.name), None)],
            MigrationStep::AddPrimaryKey { table, key } => vec![warned(
                format!("ALTER TABLE {} ADD {}", table_name(table), key_clause("PRIMARY KEY", key)),
                Risk::Review,
                format!("Adding the primary key of {table} fails on duplicate or NULL keys"),
            )],
            MigrationStep::AddConstraint { table, constraint } => {
                let (clause, what) = match constraint {
                    ConstraintDef::Unique { key } => (key_clause("UNIQUE", key), "unique constraint"),
                    ConstraintDef::Check { check } => (
                        format!("{}CHECK ({})", constraint_name(&check.name), map_text(&check.expression, ctx)),
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
                vec![statement(index_sql(ctx.target_schema.as_deref(), table, &index))]
            }
            MigrationStep::CreateView { view, replace } => {
                let body = view.definition.trim().trim_end_matches(';').trim_end();
                let mapped = map_text(body, ctx);
                let name = table_name(&view.name);
                let sql = match (view.materialized, replace) {
                    (true, _) => format!("CREATE MATERIALIZED VIEW {name} AS\n{mapped}"),
                    (false, true) => format!("CREATE OR REPLACE VIEW {name} AS\n{mapped}"),
                    (false, false) => format!("CREATE VIEW {name} AS\n{mapped}"),
                };
                let mut rendered = statement(sql);
                if mapped != body {
                    rendered.risk = Risk::Review;
                    rendered.warnings.push(format!(
                        "Schema references in the body of {} were changed from {} to {}; review them",
                        view.name,
                        ctx.source_schema.as_deref().unwrap_or_default(),
                        ctx.target_schema.as_deref().unwrap_or_default()
                    ));
                }
                if view.materialized {
                    rendered.risk = Risk::Review;
                    rendered
                        .warnings
                        .push(format!("The materialized view {} is filled again when it is created", view.name));
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
            MigrationStep::SetComment { target, comment } => vec![statement(self.comment(ctx, target, comment.as_deref()))],
            MigrationStep::SetTableOption { table, name, value } => {
                vec![manual(&format!("Set option {name} = {value} on {table} by hand"), None)]
            }
            MigrationStep::Manual { reason, ddl_hint } => vec![manual(reason, ddl_hint.as_deref())],
        }
    }

    fn wrap(&self, body: Vec<RenderedStatement>, transactional: bool) -> Vec<RenderedStatement> {
        if !transactional {
            return body;
        }
        let mut out = Vec::with_capacity(body.len() + 2);
        out.push(statement("BEGIN".into()));
        out.extend(body);
        out.push(statement("COMMIT".into()));
        out
    }
}

impl PgSchemaDialect {
    fn comment(&self, ctx: &RenderContext, target: &CommentTarget, comment: Option<&str>) -> String {
        let name = self.table_name(ctx, &target.name);
        match (&target.column, target.kind.as_str()) {
            (Some(column), _) => comment_sql("COLUMN", &format!("{name}.{}", ident(column)), comment),
            (None, "materializedView") => comment_sql("MATERIALIZED VIEW", &name, comment),
            (None, "view") => comment_sql("VIEW", &name, comment),
            (None, _) => comment_sql("TABLE", &name, comment),
        }
    }
}
