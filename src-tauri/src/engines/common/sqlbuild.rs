//! Builds browse and row-change SQL for any dialect. Values are never concatenated into the SQL: every user value becomes a bound
//! parameter, rendered by the dialect's placeholder (`($1::text)::numeric`, `?3`, `{p2:Nullable(Int32)}`, `:4`). Identifiers come
//! from the catalog and are quoted by the dialect.

use super::value::{parse_input, Value};
use super::{BrowseRequest, ColumnInfo, EngineError, FilterOp, ObjectRef, Page, Result};

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
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
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
}

/// Builds `SELECT … FROM … WHERE <filter> [AND <keyset>] ORDER BY … LIMIT limit+1`. `key` are the row-identifying columns.
pub fn build_browse<D: SqlDialect + ?Sized>(d: &D, req: &BrowseRequest, columns: &[ColumnInfo], key: &[String]) -> Result<BrowseSql> {
    let mut b = Binder::new(d);
    let mut conds = Vec::new();
    for f in &req.filter {
        let col = find_column(columns, &f.column)?;
        conds.push(filter_sql(&mut b, f.op, col, &f.values)?);
    }
    let filter_sql = if conds.is_empty() {
        String::new()
    } else {
        let joiner = if req.match_any { " OR " } else { " AND " };
        format!("({})", conds.join(joiner))
    };

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
        where_parts.push(keyset_sql(&mut b, &order, after, columns)?);
    }
    let where_clause = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };

    let select_list = columns.iter().map(|c| d.select_expr(c)).collect::<Vec<_>>().join(", ");
    let order_sql = if order.is_empty() {
        String::new()
    } else {
        let items: Vec<String> = order
            .iter()
            .map(|(c, desc)| format!("{}{}", d.quote_ident(c), if *desc { " DESC" } else { " ASC" }))
            .collect();
        format!(" ORDER BY {}", items.join(", "))
    };
    let offset = if req.after.is_some() { 0 } else { req.offset };
    let sql = format!(
        "SELECT {select_list} FROM {}{where_clause}{order_sql} {}",
        d.table_sql(&req.object),
        d.limit_offset(req.limit.saturating_add(1), offset)
    );
    Ok(BrowseSql {
        sql,
        params: b.params,
        order,
        keyset_ok,
        where_sql: filter_sql,
    })
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
            let e = escape_like(&values[0]);
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

/// Trims the extra row fetched to detect more pages and computes the keyset cursor.
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
}
