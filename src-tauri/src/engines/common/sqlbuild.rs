//! Builds browse and row-change SQL for any dialect. Values are never concatenated into the SQL: every user value becomes a bound
//! parameter, rendered by the dialect's placeholder (`($1::text)::numeric`, `?3`, `{p2:Nullable(Int32)}`, `:4`). Identifiers come
//! from the catalog and are quoted by the dialect.

use super::value::{parse_input, Value};
use super::{BrowseRequest, ColumnInfo, EngineError, FilterOp, ObjectRef, Page, Result};
use crate::contract::tabular::{Field, RowChange, RowCheck};

/// What a parameter is compared with: a column (bind with the column's type) or plain text (LIKE patterns).
#[derive(Debug, Clone, PartialEq)]
pub enum ParamTarget {
    Column(ColumnInfo),
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub value: Value,
    pub target: ParamTarget,
}

pub trait SqlDialect {
    fn quote_ident(&self, name: &str) -> String;

    /// The qualified name of a table (`"schema"."name"`).
    fn table_sql(&self, obj: &ObjectRef) -> String {
        match &obj.schema {
            Some(s) => format!("{}.{}", self.quote_ident(s), self.quote_ident(&obj.name)),
            None => self.quote_ident(&obj.name),
        }
    }

    /// Placeholder for parameter `n` (1-based).
    fn placeholder(&self, n: usize, target: &ParamTarget) -> String;

    /// The expression selected for a column (engines can wrap types their client cannot read).
    fn select_expr(&self, col: &ColumnInfo) -> String {
        self.quote_ident(&col.name)
    }

    /// The column converted to text, for substring and LIKE filters.
    fn text_expr(&self, col_sql: &str, col: &ColumnInfo) -> String;

    /// `lhs LIKE rhs`, with backslash as the escape character; case-insensitive when `ci`.
    fn like(&self, lhs: &str, rhs: &str, ci: bool, negate: bool) -> String;

    fn limit_offset(&self, limit: u64, offset: u64) -> String {
        if offset > 0 {
            format!("LIMIT {limit} OFFSET {offset}")
        } else {
            format!("LIMIT {limit}")
        }
    }

    /// `TOP (n)` written after SELECT where the dialect limits rows that way (T-SQL); `None` uses [`limit_offset`].
    ///
    /// [`limit_offset`]: SqlDialect::limit_offset
    fn top(&self, _limit: u64, _offset: u64) -> Option<String> {
        None
    }

    /// The ORDER BY of an unordered page when the dialect's paging needs one (T-SQL `OFFSET … FETCH`).
    fn unordered(&self) -> Option<&'static str> {
        None
    }

    /// Characters that are special in a LIKE pattern and escaped with a backslash (T-SQL adds `[`).
    fn like_specials(&self) -> &'static str {
        "%_\\"
    }

    /// The column side of a row-check comparison (T-SQL compares `xml` and `ntext` only as text).
    fn compare_column(&self, col_sql: String, _col: &ColumnInfo) -> String {
        col_sql
    }

    /// The NULL-safe equality operator (`<=>`, `IS NOT DISTINCT FROM`) used by row checks; `None` writes
    /// `(col = ? OR (col IS NULL AND ? IS NULL))`.
    fn null_safe_eq(&self) -> Option<&'static str> {
        None
    }

    /// A parameter compared with `col` in a row check (MySQL compares a JSON column only with a JSON value).
    fn compare_param(&self, placeholder: String, _col: &ColumnInfo) -> String {
        placeholder
    }

    /// Appended to guarded updates and deletes so a duplicated key changes one row (MySQL `LIMIT 1`).
    fn single_row(&self) -> &'static str {
        ""
    }
}

/// Collects bound parameters while SQL is being written.
pub struct Binder<'a, D: SqlDialect + ?Sized> {
    pub dialect: &'a D,
    pub params: Vec<Param>,
}

impl<'a, D: SqlDialect + ?Sized> Binder<'a, D> {
    pub fn new(dialect: &'a D) -> Self {
        Self {
            dialect,
            params: Vec::new(),
        }
    }
    pub fn bind(&mut self, value: Value, target: ParamTarget) -> String {
        self.params.push(Param { value, target });
        let target = &self.params.last().expect("just pushed").target;
        self.dialect.placeholder(self.params.len(), target)
    }
    pub fn bind_column(&mut self, value: Value, col: &ColumnInfo) -> String {
        self.bind(value, ParamTarget::Column(col.clone()))
    }
}

fn find_column<'c>(columns: &'c [ColumnInfo], name: &str) -> Result<&'c ColumnInfo> {
    columns
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| EngineError::input(format!("The table has no column named '{name}'.")))
}

/// Escapes `%`, `_` and `\` so a substring is matched literally by `LIKE … ESCAPE '\'`.
pub fn escape_like(text: &str) -> String {
    escape_like_with(text, "%_\\")
}

/// Escapes `specials` with a backslash.
pub fn escape_like_with(text: &str, specials: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        if specials.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A browse query ready to run.
#[derive(Debug, Clone)]
pub struct BrowseSql {
    pub sql: String,
    pub params: Vec<Param>,
    /// The ORDER BY columns and whether each is descending (sort keys, then the key columns as a tiebreak).
    pub order: Vec<(String, bool)>,
    /// True when the order is total and non-null, so the last row's order values can serve as a keyset cursor.
    pub keyset_ok: bool,
    /// The WHERE clause alone (with the same parameters), for estimated counts.
    pub where_sql: String,
    /// The rows come in reversed order (a page read from the end); [`finish_page`] turns them back.
    pub reverse: bool,
}

/// Builds `SELECT … FROM … WHERE <filter> [AND <keyset>] ORDER BY … LIMIT limit+1`. `key` are the row-identifying columns.
pub fn build_browse<D: SqlDialect + ?Sized>(d: &D, req: &BrowseRequest, columns: &[ColumnInfo], key: &[String]) -> Result<BrowseSql> {
    build_browse_with_raw(d, req, columns, key, None)
}

/// [`build_browse`] plus a raw filter in the engine's SQL, appended as `(…)`. The caller has validated it as one
/// condition (the gateway parses raw filters).
pub fn build_browse_with_raw<D: SqlDialect + ?Sized>(
    d: &D,
    req: &BrowseRequest,
    columns: &[ColumnInfo],
    key: &[String],
    raw: Option<&str>,
) -> Result<BrowseSql> {
    let mut b = Binder::new(d);
    let mut conds = Vec::new();
    for f in &req.filter {
        let col = find_column(columns, &f.column)?;
        conds.push(filter_sql(&mut b, f.op, col, &f.values)?);
    }
    let mut filter_parts = Vec::new();
    if !conds.is_empty() {
        let joiner = if req.match_any { " OR " } else { " AND " };
        filter_parts.push(format!("({})", conds.join(joiner)));
    }
    if let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) {
        filter_parts.push(format!("({raw})"));
    }
    let filter_sql = filter_parts.join(" AND ");

    let mut order: Vec<(String, bool)> = Vec::new();
    for s in &req.sort {
        find_column(columns, &s.column)?;
        if !order.iter().any(|(c, _)| *c == s.column) {
            order.push((s.column.clone(), s.descending));
        }
    }
    for k in key {
        if !order.iter().any(|(c, _)| c == k) {
            order.push((k.clone(), false));
        }
    }
    let keyset_ok = !key.is_empty() && order.iter().all(|(c, _)| columns.iter().any(|col| col.name == *c && !col.nullable));
    if req.reverse && order.is_empty() {
        return Err(EngineError::input(
            "Reading from the end needs an order: sort by a column, or give the table a primary key.",
        ));
    }
    // Reading from the end flips every direction; a plain flip also mirrors where NULLs go in every SQL dialect.
    let sql_order: Vec<(String, bool)> = order.iter().map(|(c, desc)| (c.clone(), *desc != req.reverse)).collect();

    let mut where_parts = Vec::new();
    if !filter_sql.is_empty() {
        where_parts.push(filter_sql.clone());
    }
    if let Some(after) = &req.after {
        if !keyset_ok {
            return Err(EngineError::input(
                "Keyset paging needs a primary key and non-null sort columns; page by offset for this order.",
            ));
        }
        if after.len() != order.len() {
            return Err(EngineError::input(
                "The keyset cursor does not match the sort order; reload the first page.",
            ));
        }
        where_parts.push(keyset_sql(&mut b, &sql_order, after, columns)?);
    }
    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };

    let select_list = columns.iter().map(|c| d.select_expr(c)).collect::<Vec<_>>().join(", ");
    let offset = if req.after.is_some() { 0 } else { req.offset };
    let (top, tail) = limit_parts(d, req.limit.saturating_add(1), offset);
    let order_sql = if order.is_empty() {
        match d.unordered().filter(|_| !tail.is_empty()) {
            Some(fallback) => format!(" ORDER BY {fallback}"),
            None => String::new(),
        }
    } else {
        let items: Vec<String> = sql_order
            .iter()
            .map(|(c, desc)| format!("{}{}", d.quote_ident(c), if *desc { " DESC" } else { " ASC" }))
            .collect();
        format!(" ORDER BY {}", items.join(", "))
    };
    let sql = format!(
        "SELECT {top}{select_list} FROM {}{where_clause}{order_sql}{tail}",
        d.table_sql(&req.object)
    );
    Ok(BrowseSql {
        sql,
        params: b.params,
        order,
        keyset_ok,
        where_sql: filter_sql,
        reverse: req.reverse,
    })
}

/// `("TOP (n) ", "")` or `("", " LIMIT n OFFSET m")`.
fn limit_parts<D: SqlDialect + ?Sized>(d: &D, limit: u64, offset: u64) -> (String, String) {
    match d.top(limit, offset) {
        Some(top) => (format!("{top} "), String::new()),
        None => (String::new(), format!(" {}", d.limit_offset(limit, offset))),
    }
}

fn filter_sql<D: SqlDialect + ?Sized>(b: &mut Binder<D>, op: FilterOp, col: &ColumnInfo, values: &[String]) -> Result<String> {
    let d = b.dialect;
    let c = d.quote_ident(&col.name);
    if let Some(n) = op.arity() {
        if values.len() != n {
            return Err(EngineError::input(format!("The filter on '{}' needs {n} value(s).", col.name)));
        }
    } else if values.is_empty() {
        return Err(EngineError::input(format!(
            "The filter on '{}' needs at least one value.",
            col.name
        )));
    }
    let typed = |v: &str| parse_input(&col.kind, v);
    Ok(match op {
        FilterOp::IsNull => format!("{c} IS NULL"),
        FilterOp::IsNotNull => format!("{c} IS NOT NULL"),
        FilterOp::Eq | FilterOp::Ne | FilterOp::Lt | FilterOp::Le | FilterOp::Gt | FilterOp::Ge => {
            let sym = match op {
                FilterOp::Eq => "=",
                FilterOp::Ne => "<>",
                FilterOp::Lt => "<",
                FilterOp::Le => "<=",
                FilterOp::Gt => ">",
                _ => ">=",
            };
            let p = b.bind_column(typed(&values[0])?, col);
            format!("{c} {sym} {p}")
        }
        FilterOp::Between => {
            let lo = b.bind_column(typed(&values[0])?, col);
            let hi = b.bind_column(typed(&values[1])?, col);
            format!("{c} BETWEEN {lo} AND {hi}")
        }
        FilterOp::In | FilterOp::NotIn => {
            let mut ps = Vec::new();
            for v in values {
                ps.push(b.bind_column(typed(v)?, col));
            }
            format!("{c} {}IN ({})", if op == FilterOp::NotIn { "NOT " } else { "" }, ps.join(", "))
        }
        FilterOp::Contains | FilterOp::NotContains | FilterOp::StartsWith | FilterOp::EndsWith => {
            let e = escape_like_with(&values[0], d.like_specials());
            let pattern = match op {
                FilterOp::StartsWith => format!("{e}%"),
                FilterOp::EndsWith => format!("%{e}"),
                _ => format!("%{e}%"),
            };
            let p = b.bind(Value::Text(pattern), ParamTarget::Text);
            d.like(&d.text_expr(&c, col), &p, true, op == FilterOp::NotContains)
        }
        FilterOp::Like | FilterOp::NotLike => {
            let p = b.bind(Value::Text(values[0].clone()), ParamTarget::Text);
            d.like(&d.text_expr(&c, col), &p, false, op == FilterOp::NotLike)
        }
    })
}

/// `(a > ?) OR (a = ? AND b > ?) OR …`, honouring each column's direction. Works in every dialect (Oracle has no row-value
/// comparison) and with mixed directions.
fn keyset_sql<D: SqlDialect + ?Sized>(
    b: &mut Binder<D>,
    order: &[(String, bool)],
    after: &[Value],
    columns: &[ColumnInfo],
) -> Result<String> {
    let mut ors = Vec::new();
    for i in 0..order.len() {
        let mut ands = Vec::new();
        for j in 0..=i {
            let (name, desc) = &order[j];
            let col = find_column(columns, name)?;
            let c = b.dialect.quote_ident(name);
            let p = b.bind_column(after[j].clone(), col);
            let op = if j < i {
                "="
            } else if *desc {
                "<"
            } else {
                ">"
            };
            ands.push(format!("{c} {op} {p}"));
        }
        ors.push(format!("({})", ands.join(" AND ")));
    }
    Ok(format!("({})", ors.join(" OR ")))
}

/// Trims the extra row fetched to detect more pages and computes the keyset cursor. A page read from the end gets its
/// rows back in the normal order; its keyset is that of its first row, for the page before it.
pub fn finish_page(
    columns: Vec<ColumnInfo>,
    mut rows: Vec<Vec<Value>>,
    limit: u64,
    built: &BrowseSql,
    key_columns: Vec<String>,
    estimated_total: Option<i64>,
) -> Page {
    let has_more = rows.len() as u64 > limit;
    rows.truncate(limit as usize);
    finish_rows(columns, rows, has_more, built, key_columns, estimated_total)
}

/// Puts the per-row extras of a page (row versions) in the order [`finish_page`] gives its rows.
pub fn page_order<T>(mut items: Vec<T>, limit: u64, built: &BrowseSql) -> Vec<T> {
    items.truncate(limit as usize);
    if built.reverse {
        items.reverse();
    }
    items
}

fn finish_rows(
    columns: Vec<ColumnInfo>,
    mut rows: Vec<Vec<Value>>,
    has_more: bool,
    built: &BrowseSql,
    key_columns: Vec<String>,
    estimated_total: Option<i64>,
) -> Page {
    let next_keyset = if built.keyset_ok && has_more {
        rows.last().map(|row| {
            built
                .order
                .iter()
                .map(|(name, _)| {
                    columns
                        .iter()
                        .position(|c| c.name == *name)
                        .and_then(|i| row.get(i).cloned())
                        .unwrap_or(Value::Null)
                })
                .collect()
        })
    } else {
        None
    };
    if built.reverse {
        rows.reverse();
    }
    Page {
        columns,
        rows,
        has_more,
        key_columns,
        next_keyset,
        estimated_total,
        sql: built.sql.clone(),
    }
}

/// A row-change statement ready to run.
#[derive(Debug, Clone)]
pub struct ChangeSql {
    pub sql: String,
    pub params: Vec<Param>,
}

fn key_where<D: SqlDialect + ?Sized>(b: &mut Binder<D>, key: &[(String, Value)], columns: &[ColumnInfo]) -> Result<String> {
    if key.is_empty() {
        return Err(EngineError::input("The row has no key, so it cannot be changed safely."));
    }
    let mut parts = Vec::new();
    for (name, v) in key {
        let col = find_column(columns, name)?;
        let c = b.dialect.quote_ident(name);
        if v.is_null() {
            parts.push(format!("{c} IS NULL"));
        } else {
            let p = b.bind_column(v.clone(), col);
            parts.push(format!("{c} = {p}"));
        }
    }
    Ok(parts.join(" AND "))
}

pub fn build_insert<D: SqlDialect + ?Sized>(
    d: &D,
    obj: &ObjectRef,
    values: &[(String, Value)],
    columns: &[ColumnInfo],
) -> Result<ChangeSql> {
    let mut b = Binder::new(d);
    if values.is_empty() {
        return Ok(ChangeSql {
            sql: format!("INSERT INTO {} DEFAULT VALUES", d.table_sql(obj)),
            params: Vec::new(),
        });
    }
    let mut names = Vec::new();
    let mut ps = Vec::new();
    for (name, v) in values {
        let col = find_column(columns, name)?;
        names.push(d.quote_ident(name));
        ps.push(if v.is_null() {
            "NULL".to_owned()
        } else {
            b.bind_column(v.clone(), col)
        });
    }
    Ok(ChangeSql {
        sql: format!("INSERT INTO {} ({}) VALUES ({})", d.table_sql(obj), names.join(", "), ps.join(", ")),
        params: b.params,
    })
}

pub fn build_update<D: SqlDialect + ?Sized>(
    d: &D,
    obj: &ObjectRef,
    key: &[(String, Value)],
    values: &[(String, Value)],
    columns: &[ColumnInfo],
) -> Result<ChangeSql> {
    if values.is_empty() {
        return Err(EngineError::input("The update changes no columns."));
    }
    let mut b = Binder::new(d);
    let mut sets = Vec::new();
    for (name, v) in values {
        let col = find_column(columns, name)?;
        let p = if v.is_null() {
            "NULL".to_owned()
        } else {
            b.bind_column(v.clone(), col)
        };
        sets.push(format!("{} = {p}", d.quote_ident(name)));
    }
    let w = key_where(&mut b, key, columns)?;
    Ok(ChangeSql {
        sql: format!("UPDATE {} SET {} WHERE {w}", d.table_sql(obj), sets.join(", ")),
        params: b.params,
    })
}

pub fn build_delete<D: SqlDialect + ?Sized>(d: &D, obj: &ObjectRef, key: &[(String, Value)], columns: &[ColumnInfo]) -> Result<ChangeSql> {
    let mut b = Binder::new(d);
    let w = key_where(&mut b, key, columns)?;
    Ok(ChangeSql {
        sql: format!("DELETE FROM {} WHERE {w}", d.table_sql(obj)),
        params: b.params,
    })
}

// ---------------------------------------------------------------------------------------------------------------------------
// Guarded row changes (optimistic concurrency, see `contract::tabular`)

fn pairs(fields: &[Field]) -> Vec<(String, Value)> {
    fields.iter().map(|field| (field.column.clone(), field.value.clone())).collect()
}

/// `col <=> ?` in the dialect's NULL-safe form.
fn null_safe_eq<D: SqlDialect + ?Sized>(b: &mut Binder<D>, col: &ColumnInfo, value: &Value) -> String {
    let c = b.dialect.compare_column(b.dialect.quote_ident(&col.name), col);
    match b.dialect.null_safe_eq() {
        Some(op) => {
            let p = b.bind_column(value.clone(), col);
            format!("{c} {op} {}", b.dialect.compare_param(p, col))
        }
        None => {
            let p = b.bind_column(value.clone(), col);
            let p = b.dialect.compare_param(p, col);
            let again = b.bind_column(value.clone(), col);
            format!("({c} = {p} OR ({c} IS NULL AND {again} IS NULL))")
        }
    }
}

/// The WHERE clause of a guarded update or delete: the key, then what `check` asks for. `version_column` is the
/// engine's row-version column (`xmin`, a `rowversion` column); without one a version check is a key-only check.
pub fn guarded_where<D: SqlDialect + ?Sized>(
    b: &mut Binder<D>,
    key: &[Field],
    check: RowCheck<'_>,
    columns: &[ColumnInfo],
    version_column: Option<&str>,
) -> Result<String> {
    let mut parts = vec![key_where(b, &pairs(key), columns)?];
    match check {
        RowCheck::KeyOnly => {}
        RowCheck::Version(version) => {
            if let Some(name) = version_column {
                let p = b.bind(version.clone(), ParamTarget::Text);
                parts.push(format!("{} = {p}", b.dialect.quote_ident(name)));
            }
        }
        RowCheck::Original(fields) => {
            for field in fields.iter().filter(|field| !key.iter().any(|k| k.column == field.column)) {
                let col = find_column(columns, &field.column)?;
                parts.push(null_safe_eq(b, col, &field.value));
            }
        }
    }
    Ok(parts.join(" AND "))
}

/// The statement for one contract row change: inserts as [`build_insert`], updates and deletes guarded by
/// `RowChange::check()`. The engine must treat an update or delete that matches no row as a conflict.
pub fn build_guarded<D: SqlDialect + ?Sized>(
    d: &D,
    obj: &ObjectRef,
    change: &RowChange,
    columns: &[ColumnInfo],
    version_column: Option<&str>,
) -> Result<ChangeSql> {
    match change {
        RowChange::Insert { values } => build_insert(d, obj, &pairs(values), columns),
        RowChange::Update { key, values, .. } => {
            if values.is_empty() {
                return Err(EngineError::input("The update changes no columns."));
            }
            let mut b = Binder::new(d);
            let mut sets = Vec::new();
            for (name, v) in pairs(values) {
                let col = find_column(columns, &name)?;
                let p = if v.is_null() { "NULL".to_owned() } else { b.bind_column(v, col) };
                sets.push(format!("{} = {p}", d.quote_ident(&name)));
            }
            let w = guarded_where(&mut b, key, change.check(), columns, version_column)?;
            Ok(ChangeSql {
                sql: format!("UPDATE {} SET {} WHERE {w}{}", d.table_sql(obj), sets.join(", "), d.single_row()),
                params: b.params,
            })
        }
        RowChange::Delete { key, .. } => {
            let mut b = Binder::new(d);
            let w = guarded_where(&mut b, key, change.check(), columns, version_column)?;
            Ok(ChangeSql {
                sql: format!("DELETE FROM {} WHERE {w}{}", d.table_sql(obj), d.single_row()),
                params: b.params,
            })
        }
    }
}

/// `SELECT <columns> FROM <table> WHERE <key>`: re-reads a row after a conflict.
pub fn build_reread<D: SqlDialect + ?Sized>(d: &D, obj: &ObjectRef, key: &[Field], columns: &[ColumnInfo]) -> Result<ChangeSql> {
    let mut b = Binder::new(d);
    let w = key_where(&mut b, &pairs(key), columns)?;
    let select_list = columns.iter().map(|c| d.select_expr(c)).collect::<Vec<_>>().join(", ");
    let (top, tail) = limit_parts(d, 1, 0);
    Ok(ChangeSql {
        sql: format!("SELECT {top}{select_list} FROM {} WHERE {w}{tail}", d.table_sql(obj)),
        params: b.params,
    })
}

/// `SELECT DISTINCT c FROM (SELECT c FROM t LIMIT sample) s LIMIT limit` in `d`'s dialect (`TOP (n)` where it limits
/// that way): at most `limit` distinct values of one column among the first `sample` rows, for the assistant's value
/// hints. Identifiers are quoted by the dialect; the column is selected the way browsing selects it.
pub fn build_distinct_sample<D: SqlDialect + ?Sized>(d: &D, obj: &ObjectRef, col: &ColumnInfo, sample: u64, limit: u64) -> String {
    let (inner_top, inner_tail) = limit_parts(d, sample, 0);
    let (top, tail) = limit_parts(d, limit, 0);
    format!(
        "SELECT DISTINCT {top}{} FROM (SELECT {inner_top}{} FROM {}{inner_tail}) s{tail}",
        d.quote_ident(&col.name),
        d.select_expr(col),
        d.table_sql(obj)
    )
}

/// Doubles `quote` inside `name` and wraps it: the standard identifier quoting.
pub fn quote_with(name: &str, quote: char) -> String {
    let mut s = String::with_capacity(name.len() + 2);
    s.push(quote);
    for c in name.chars() {
        if c == quote {
            s.push(quote);
        }
        s.push(c);
    }
    s.push(quote);
    s
}

#[cfg(test)]
mod tests {
    use super::super::{FilterCond, NodeKind, SortKey, ValueKind};
    use super::*;

    struct Ansi;
    impl SqlDialect for Ansi {
        fn quote_ident(&self, name: &str) -> String {
            quote_with(name, '"')
        }
        fn placeholder(&self, n: usize, _t: &ParamTarget) -> String {
            format!("${n}")
        }
        fn text_expr(&self, col_sql: &str, _c: &ColumnInfo) -> String {
            format!("CAST({col_sql} AS text)")
        }
        fn like(&self, lhs: &str, rhs: &str, ci: bool, negate: bool) -> String {
            format!(
                "{lhs} {}{} {rhs}",
                if negate { "NOT " } else { "" },
                if ci { "ILIKE" } else { "LIKE" }
            )
        }
    }

    fn cols() -> Vec<ColumnInfo> {
        let mut id = ColumnInfo::result("id", "int", ValueKind::Int);
        id.nullable = false;
        let mut created = ColumnInfo::result("created", "timestamp", ValueKind::Timestamp);
        created.nullable = false;
        vec![id, ColumnInfo::result("name", "text", ValueKind::Text), created]
    }

    #[test]
    fn browse_binds_every_value() {
        let mut req = BrowseRequest::new(ObjectRef::new(None, Some("s"), "t\"x", NodeKind::Table), 50);
        req.filter = vec![
            FilterCond::new("id", FilterOp::In, &["1", "2"]),
            FilterCond::new("name", FilterOp::Contains, &["50%_off'"]),
            FilterCond::new("created", FilterOp::IsNotNull, &[]),
        ];
        req.sort = vec![SortKey::desc("created")];
        req.offset = 100;
        let b = build_browse(&Ansi, &req, &cols(), &["id".to_owned()]).unwrap();
        assert_eq!(
            b.sql,
            "SELECT \"id\", \"name\", \"created\" FROM \"s\".\"t\"\"x\" WHERE (\"id\" IN ($1, $2) AND CAST(\"name\" AS text) ILIKE $3 AND \"created\" IS NOT NULL) ORDER BY \"created\" DESC, \"id\" ASC LIMIT 51 OFFSET 100"
        );
        assert_eq!(b.params.len(), 3);
        assert_eq!(b.params[2].value, Value::Text("%50\\%\\_off'%".into()));
        assert!(b.keyset_ok);
        assert!(!b.sql.contains("off'"));
    }

    #[test]
    fn keyset_expands_to_an_or_chain() {
        let mut req = BrowseRequest::new(ObjectRef::table(None, "t"), 10);
        req.sort = vec![SortKey::desc("created")];
        let ts = parse_input(&ValueKind::Timestamp, "2026-01-01 00:00:00").unwrap();
        req.after = Some(vec![ts, Value::Int(7)]);
        let b = build_browse(&Ansi, &req, &cols(), &["id".to_owned()]).unwrap();
        assert!(
            b.sql.contains("WHERE ((\"created\" < $1) OR (\"created\" = $2 AND \"id\" > $3))"),
            "{}",
            b.sql
        );
        assert!(b.sql.ends_with("LIMIT 11"));
        // Nullable sort column: no keyset.
        req.sort = vec![SortKey::asc("name")];
        req.after = Some(vec![Value::Text("a".into()), Value::Int(1)]);
        assert!(build_browse(&Ansi, &req, &cols(), &["id".to_owned()]).is_err());
    }

    #[test]
    fn pages_from_the_end_reverse_the_order_and_turn_the_rows_back() {
        let mut req = BrowseRequest::new(ObjectRef::table(None, "t"), 2);
        req.sort = vec![SortKey::desc("created")];
        req.reverse = true;
        let key = ["id".to_owned()];
        let b = build_browse(&Ansi, &req, &cols(), &key).unwrap();
        assert!(b.sql.ends_with("ORDER BY \"created\" ASC, \"id\" DESC LIMIT 3"), "{}", b.sql);
        // The rows arrive last first; one more than the limit means there are rows before them.
        let row = |id: i64| vec![Value::Int(id), Value::Null, Value::Int(id * 10)];
        let page = finish_page(cols(), vec![row(9), row(8), row(7)], 2, &b, key.to_vec(), None);
        assert_eq!(page.rows, vec![row(8), row(9)]);
        assert!(page.has_more);
        // The keyset of the first row on the page reads the page before it.
        assert_eq!(page.next_keyset, Some(vec![Value::Int(80), Value::Int(8)]));
        assert_eq!(page_order(vec!["v9", "v8", "v7"], 2, &b), vec!["v8", "v9"]);
        req.after = page.next_keyset;
        let before = build_browse(&Ansi, &req, &cols(), &key).unwrap();
        assert!(
            before
                .sql
                .contains("WHERE ((\"created\" > $1) OR (\"created\" = $2 AND \"id\" < $3))"),
            "{}",
            before.sql
        );
        // Without any order there is no end to read from.
        let mut unordered = BrowseRequest::new(ObjectRef::table(None, "t"), 2);
        unordered.reverse = true;
        assert!(build_browse(&Ansi, &unordered, &cols(), &[]).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        let mut req = BrowseRequest::new(ObjectRef::table(None, "t"), 10);
        req.filter = vec![FilterCond::new("id", FilterOp::Eq, &["abc"])];
        assert!(build_browse(&Ansi, &req, &cols(), &[]).is_err());
        req.filter = vec![FilterCond::new("nope", FilterOp::Eq, &["1"])];
        assert!(build_browse(&Ansi, &req, &cols(), &[]).is_err());
        req.filter = vec![FilterCond::new("id", FilterOp::Between, &["1"])];
        assert!(build_browse(&Ansi, &req, &cols(), &[]).is_err());
    }

    #[test]
    fn row_change_statements() {
        let obj = ObjectRef::table(None, "t");
        let ins = build_insert(&Ansi, &obj, &[("id".into(), Value::Int(1)), ("name".into(), Value::Null)], &cols()).unwrap();
        assert_eq!(ins.sql, "INSERT INTO \"t\" (\"id\", \"name\") VALUES ($1, NULL)");
        let up = build_update(
            &Ansi,
            &obj,
            &[("id".into(), Value::Int(1))],
            &[("name".into(), Value::Text("x".into()))],
            &cols(),
        )
        .unwrap();
        assert_eq!(up.sql, "UPDATE \"t\" SET \"name\" = $1 WHERE \"id\" = $2");
        let del = build_delete(&Ansi, &obj, &[("name".into(), Value::Null)], &cols()).unwrap();
        assert_eq!(del.sql, "DELETE FROM \"t\" WHERE \"name\" IS NULL");
        assert!(build_delete(&Ansi, &obj, &[], &cols()).is_err());
    }

    struct NullSafe;
    impl SqlDialect for NullSafe {
        fn quote_ident(&self, name: &str) -> String {
            quote_with(name, '`')
        }
        fn placeholder(&self, _n: usize, _t: &ParamTarget) -> String {
            "?".to_owned()
        }
        fn text_expr(&self, col_sql: &str, _c: &ColumnInfo) -> String {
            col_sql.to_owned()
        }
        fn like(&self, lhs: &str, rhs: &str, _ci: bool, negate: bool) -> String {
            format!("{lhs} {}LIKE {rhs}", if negate { "NOT " } else { "" })
        }
        fn null_safe_eq(&self) -> Option<&'static str> {
            Some("<=>")
        }
        fn single_row(&self) -> &'static str {
            " LIMIT 1"
        }
    }

    fn field(column: &str, value: Value) -> Field {
        Field::new(column, value)
    }

    #[test]
    fn guarded_changes_check_original_values_null_safely() {
        let obj = ObjectRef::table(None, "t");
        let update = RowChange::Update {
            key: vec![field("id", Value::Int(1))],
            values: vec![field("name", Value::Text("new".into()))],
            original: vec![field("name", Value::Text("old".into())), field("created", Value::Null)],
            version: None,
            force: false,
        };
        let generic = build_guarded(&Ansi, &obj, &update, &cols(), None).unwrap();
        assert_eq!(
            generic.sql,
            "UPDATE \"t\" SET \"name\" = $1 WHERE \"id\" = $2 AND (\"name\" = $3 OR (\"name\" IS NULL AND $4 IS NULL)) \
             AND (\"created\" = $5 OR (\"created\" IS NULL AND $6 IS NULL))"
        );
        assert_eq!(generic.params.len(), 6);
        let mysql = build_guarded(&NullSafe, &obj, &update, &cols(), None).unwrap();
        assert_eq!(
            mysql.sql,
            "UPDATE `t` SET `name` = ? WHERE `id` = ? AND `name` <=> ? AND `created` <=> ? LIMIT 1"
        );
        assert_eq!(mysql.params[3].value, Value::Null);
        let delete = RowChange::Delete {
            key: vec![field("id", Value::Int(2))],
            original: vec![field("id", Value::Int(2)), field("name", Value::Text("x".into()))],
            version: None,
            force: false,
        };
        let built = build_guarded(&NullSafe, &obj, &delete, &cols(), None).unwrap();
        assert_eq!(built.sql, "DELETE FROM `t` WHERE `id` = ? AND `name` <=> ? LIMIT 1");
    }

    #[test]
    fn forced_and_versioned_changes_check_the_key_or_the_version() {
        let obj = ObjectRef::table(None, "t");
        let forced = RowChange::Update {
            key: vec![field("id", Value::Int(1))],
            values: vec![field("name", Value::Text("new".into()))],
            original: vec![field("name", Value::Text("old".into()))],
            version: None,
            force: true,
        };
        let built = build_guarded(&NullSafe, &obj, &forced, &cols(), None).unwrap();
        assert_eq!(built.sql, "UPDATE `t` SET `name` = ? WHERE `id` = ? LIMIT 1");
        let versioned = RowChange::Delete {
            key: vec![field("id", Value::Int(1))],
            original: vec![],
            version: Some(Value::Int(42)),
            force: false,
        };
        let built = build_guarded(&Ansi, &obj, &versioned, &cols(), Some("xmin")).unwrap();
        assert_eq!(built.sql, "DELETE FROM \"t\" WHERE \"id\" = $1 AND \"xmin\" = $2");
        let without = build_guarded(&Ansi, &obj, &versioned, &cols(), None).unwrap();
        assert_eq!(without.sql, "DELETE FROM \"t\" WHERE \"id\" = $1");
        let reread = build_reread(&NullSafe, &obj, &[field("id", Value::Int(1))], &cols()).unwrap();
        assert_eq!(reread.sql, "SELECT `id`, `name`, `created` FROM `t` WHERE `id` = ? LIMIT 1");
    }

    #[test]
    fn raw_filters_are_appended_as_one_condition() {
        let mut req = BrowseRequest::new(ObjectRef::table(None, "t"), 10);
        req.filter = vec![FilterCond::new("id", FilterOp::Gt, &["1"])];
        let b = build_browse_with_raw(&NullSafe, &req, &cols(), &["id".to_owned()], Some(" name = 'a' OR id = 2 ")).unwrap();
        assert!(b.sql.contains("WHERE (`id` > ?) AND (name = 'a' OR id = 2) ORDER BY"), "{}", b.sql);
        assert_eq!(b.where_sql, "(`id` > ?) AND (name = 'a' OR id = 2)");
    }

    #[test]
    fn distinct_samples_quote_names_and_limit_both_levels() {
        let obj = ObjectRef::new(None, Some("s"), "t\"x", NodeKind::Table);
        let col = ColumnInfo::result("st\"atus", "text", ValueKind::Text);
        assert_eq!(
            build_distinct_sample(&Ansi, &obj, &col, 1000, 21),
            "SELECT DISTINCT \"st\"\"atus\" FROM (SELECT \"st\"\"atus\" FROM \"s\".\"t\"\"x\" LIMIT 1000) s LIMIT 21"
        );
        struct Top;
        impl SqlDialect for Top {
            fn quote_ident(&self, name: &str) -> String {
                quote_with(name, '"')
            }
            fn placeholder(&self, n: usize, _t: &ParamTarget) -> String {
                format!("@P{n}")
            }
            fn text_expr(&self, col_sql: &str, _c: &ColumnInfo) -> String {
                col_sql.to_owned()
            }
            fn like(&self, lhs: &str, rhs: &str, _ci: bool, _negate: bool) -> String {
                format!("{lhs} LIKE {rhs}")
            }
            fn top(&self, limit: u64, _offset: u64) -> Option<String> {
                Some(format!("TOP ({limit})"))
            }
        }
        assert_eq!(
            build_distinct_sample(&Top, &ObjectRef::table(None, "t"), &col, 1000, 21),
            "SELECT DISTINCT TOP (21) \"st\"\"atus\" FROM (SELECT TOP (1000) \"st\"\"atus\" FROM \"t\") s"
        );
    }
}
