//! Browsing and editing rows: SQL from `engines::common::sqlbuild` with every value bound, run as prepared statements
//! (a raw filter that slipped past the gateway could not stack a second statement). Edits run in one transaction per
//! call on a `CLIENT_FOUND_ROWS` connection; each update and delete is guarded by its row check, and any change that
//! matches no row rolls everything back with a `CONFLICT` listing the rows as they are now.

use std::time::Instant;

use async_trait::async_trait;
use mysql_async::{prelude::*, Conn, Row, TxOpts};

use super::{
    adapter::MySqlConnection,
    catalog::{self, database_of, qualified, quote_ident, TableColumns},
    connect::{within, CHECKOUT_TIMEOUT},
    values::{coerce, to_param, to_value},
};
use crate::{
    contract::{
        tabular::{conflict_error, RowConflict},
        ApplyResult, BrowseRequest, CallContext, ColumnInfo, Field, Filter, Page, RowChange, RowChangeSet, TabularData, Total, Value,
    },
    engines::common::{
        self, adapt,
        sqlbuild::{self, ChangeSql, Param, ParamTarget, SqlDialect},
    },
    error::AppResult,
};

/// Pages are capped so one request cannot pull a whole table.
const MAX_PAGE_SIZE: u64 = 5000;
/// Below this many rows (by statistics) a total is counted exactly; above it the estimate is shown.
const EXACT_COUNT_BELOW: u64 = 50_000;

/// Backtick quoting, `?` placeholders, `<=>` and `LIKE … ESCAPE '\\'`.
pub struct MySqlDialect {
    pub mariadb: bool,
}

impl SqlDialect for MySqlDialect {
    fn quote_ident(&self, name: &str) -> String {
        quote_ident(name)
    }

    fn table_sql(&self, obj: &common::ObjectRef) -> String {
        match obj.database.as_deref().or(obj.schema.as_deref()) {
            Some(database) => qualified(database, &obj.name),
            None => quote_ident(&obj.name),
        }
    }

    fn placeholder(&self, _n: usize, _target: &ParamTarget) -> String {
        "?".to_owned()
    }

    fn text_expr(&self, col_sql: &str, col: &ColumnInfo) -> String {
        match col.kind {
            crate::contract::ValueKind::Text => col_sql.to_owned(),
            _ => format!("CAST({col_sql} AS CHAR)"),
        }
    }

    /// Case-insensitive matches lower both sides, whatever the column's collation.
    fn like(&self, lhs: &str, rhs: &str, ci: bool, negate: bool) -> String {
        let not = if negate { "NOT " } else { "" };
        if ci {
            format!("LOWER({lhs}) {not}LIKE LOWER({rhs}) ESCAPE '\\\\'")
        } else {
            format!("{lhs} {not}LIKE {rhs} ESCAPE '\\\\'")
        }
    }

    fn null_safe_eq(&self) -> Option<&'static str> {
        Some("<=>")
    }

    /// MySQL compares a JSON column only with a JSON value; a single-precision column with a single-precision value.
    fn compare_param(&self, placeholder: String, col: &ColumnInfo) -> String {
        let type_name = col.type_name.to_ascii_lowercase();
        if col.kind == crate::contract::ValueKind::Json && !self.mariadb && type_name.starts_with("json") {
            format!("CAST({placeholder} AS JSON)")
        } else if type_name.starts_with("float") {
            format!("CAST({placeholder} AS FLOAT)")
        } else {
            placeholder
        }
    }

    fn single_row(&self) -> &'static str {
        " LIMIT 1"
    }
}

/// Bound parameters as wire values, user values coerced to their column's kind.
pub fn params_of(params: &[Param]) -> AppResult<Vec<mysql_async::Value>> {
    params
        .iter()
        .map(|param| {
            let value = match &param.target {
                ParamTarget::Column(column) => coerce(&param.value, column)?,
                ParamTarget::Text => param.value.clone(),
            };
            Ok(to_param(&value))
        })
        .collect()
}

/// Wraps an app-built SELECT so the server stops it after `seconds` (0 = no limit).
fn with_timeout(mariadb: bool, select: String, seconds: u32) -> String {
    if seconds == 0 {
        select
    } else if mariadb {
        format!("SET STATEMENT max_statement_time = {seconds} FOR {select}")
    } else {
        let millis = u64::from(seconds) * 1000;
        match select.strip_prefix("SELECT ") {
            Some(rest) => format!("SELECT /*+ MAX_EXECUTION_TIME({millis}) */ {rest}"),
            None => select,
        }
    }
}

fn rows_of(rows: Vec<Row>, columns: &[ColumnInfo]) -> Vec<Vec<Value>> {
    rows.into_iter()
        .map(|row| {
            row.unwrap()
                .into_iter()
                .zip(columns)
                .map(|(value, column)| to_value(value, column))
                .collect()
        })
        .collect()
}

impl MySqlConnection {
    pub(super) async fn edit_conn(&self) -> AppResult<Conn> {
        within(CHECKOUT_TIMEOUT, "Opening a connection", self.edit_pool.get_conn()).await
    }

    async fn browse_page(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        let started = Instant::now();
        let database = database_of(&request.object)?.to_owned();
        let mut conn = self.pooled().await?;
        let TableColumns { columns, primary_key } =
            catalog::table_columns(&mut conn, self.server.mariadb, &database, &request.object.name).await?;
        let columns = match &request.columns {
            Some(wanted) => columns.into_iter().filter(|column| wanted.contains(&column.name)).collect(),
            None => columns,
        };
        let raw = match &request.filter {
            Filter::Raw { text, .. } => Some(text.clone()),
            _ => None,
        };
        let mut core = adapt::browse_to_common(&BrowseRequest {
            filter: if raw.is_some() { Filter::None } else { request.filter.clone() },
            ..request.clone()
        })?;
        core.object.database = Some(database.clone());
        core.limit = request.limit.clamp(1, MAX_PAGE_SIZE);
        let dialect = MySqlDialect {
            mariadb: self.server.mariadb,
        };
        let built = sqlbuild::build_browse_with_raw(&dialect, &core, &columns, &primary_key, raw.as_deref())?;
        let timeout = ctx.limits.timeout_secs;
        let params = params_of(&built.params)?;
        let rows: Vec<Row> = conn
            .exec(with_timeout(self.server.mariadb, built.sql.clone(), timeout), params.clone())
            .await?;
        let rows = rows_of(rows, &columns);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let total = if request.exact_total || request.estimate_total {
            self.total(&mut conn, &database, request, &built, &params, timeout).await?
        } else {
            None
        };
        let mut page = adapt::page_to_contract(
            sqlbuild::finish_page(columns, rows, core.limit, &built, primary_key, None),
            elapsed_ms,
        );
        page.total = total;
        Ok(page)
    }

    /// The exact count when asked for or cheap (small tables), else the statistics' estimate for unfiltered tables.
    async fn total(
        &self,
        conn: &mut Conn,
        database: &str,
        request: &BrowseRequest,
        built: &sqlbuild::BrowseSql,
        params: &[mysql_async::Value],
        timeout: u32,
    ) -> AppResult<Option<Total>> {
        let estimate = catalog::table_estimate(conn, database, &request.object.name).await?;
        let filtered = !built.where_sql.is_empty();
        let count = request.exact_total || estimate.is_none_or(|rows| rows < EXACT_COUNT_BELOW);
        if !count {
            return Ok((!filtered).then(|| Total {
                value: estimate.unwrap_or(0),
                exact: false,
            }));
        }
        let where_clause = if filtered {
            format!(" WHERE {}", built.where_sql)
        } else {
            String::new()
        };
        // The keyset parameters come after the filter's.
        let filter_params = params.len() - request.after.as_ref().map_or(0, |after| after.len() * (after.len() + 1) / 2);
        let sql = format!("SELECT COUNT(*) FROM {}{where_clause}", qualified(database, &request.object.name));
        let value: Option<u64> = conn
            .exec_first(with_timeout(self.server.mariadb, sql, timeout), params[..filter_params].to_vec())
            .await?;
        Ok(Some(Total {
            value: value.unwrap_or(0),
            exact: true,
        }))
    }

    async fn apply_set(&self, set: &RowChangeSet) -> AppResult<ApplyResult> {
        let database = database_of(&set.object)?.to_owned();
        let mut conn = self.edit_conn().await?;
        let TableColumns { columns, .. } = catalog::table_columns(&mut conn, self.server.mariadb, &database, &set.object.name).await?;
        let mut object = adapt::object_to_common(&set.object)?;
        object.database = Some(database);
        let dialect = MySqlDialect {
            mariadb: self.server.mariadb,
        };
        let statements = set
            .changes
            .iter()
            .map(|change| {
                let built = sqlbuild::build_guarded(&dialect, &object, change, &columns, None)?;
                Ok((params_of(&built.params)?, built))
            })
            .collect::<AppResult<Vec<(Vec<mysql_async::Value>, ChangeSql)>>>()?;
        let mut result = ApplyResult::default();
        let mut missed = Vec::new();
        let mut tx = conn.start_transaction(TxOpts::default()).await?;
        let applied: AppResult<()> = async {
            for (index, (change, (params, built))) in set.changes.iter().zip(statements).enumerate() {
                let outcome = tx.exec_iter(built.sql.as_str(), params).await?;
                let affected = outcome.affected_rows();
                outcome.drop_result().await?;
                match change {
                    RowChange::Insert { .. } => result.inserted += affected,
                    RowChange::Update { .. } if affected == 0 => missed.push(index),
                    RowChange::Update { .. } => result.updated += affected,
                    RowChange::Delete { .. } if affected == 0 => missed.push(index),
                    RowChange::Delete { .. } => result.deleted += affected,
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = applied {
            let _ = tx.rollback().await;
            return Err(error);
        }
        if missed.is_empty() {
            tx.commit().await?;
            return Ok(result);
        }
        tx.rollback().await?;
        let mut conflicts = Vec::with_capacity(missed.len());
        for index in missed {
            let change = &set.changes[index];
            let current = self.reread(&mut conn, &dialect, &object, change.key(), &columns).await?;
            conflicts.push(RowConflict::new(index, change, current));
        }
        Err(conflict_error(&set.object, conflicts))
    }

    /// The row with this key as it is now (all columns), or `None` when it no longer exists.
    async fn reread(
        &self,
        conn: &mut Conn,
        dialect: &MySqlDialect,
        object: &common::ObjectRef,
        key: &[Field],
        columns: &[ColumnInfo],
    ) -> AppResult<Option<Vec<Field>>> {
        let built = sqlbuild::build_reread(dialect, object, key, columns)?;
        let row: Option<Row> = conn.exec_first(built.sql.as_str(), params_of(&built.params)?).await?;
        Ok(row.map(|row| {
            row.unwrap()
                .into_iter()
                .zip(columns)
                .map(|(value, column)| Field::new(&column.name, to_value(value, column)))
                .collect()
        }))
    }
}

#[async_trait]
impl TabularData for MySqlConnection {
    async fn browse(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        self.browse_page(request, ctx).await
    }

    async fn apply(&self, changes: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        if changes.changes.is_empty() {
            return Ok(ApplyResult::default());
        }
        self.apply_set(changes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{FilterCond, FilterOp, NodeKind, ObjectRef, SortKey, ValueKind};

    fn columns() -> Vec<ColumnInfo> {
        let mut id = ColumnInfo::result("id", "int(11)", ValueKind::Int);
        id.nullable = false;
        let mut meta = ColumnInfo::result("meta", "json", ValueKind::Json);
        meta.nullable = true;
        vec![
            id,
            ColumnInfo::result("name", "varchar(100)", ValueKind::Text),
            ColumnInfo::result("price", "decimal(12,2)", ValueKind::Decimal),
            ColumnInfo::result("ratio", "float", ValueKind::Float),
            meta,
        ]
    }

    fn object() -> common::ObjectRef {
        let mut object = common::ObjectRef::table(None, "items");
        object.database = Some("shop".into());
        object
    }

    const MYSQL: MySqlDialect = MySqlDialect { mariadb: false };

    #[test]
    fn browse_binds_filters_and_pages_by_keyset() {
        let mut request = BrowseRequest::new(ObjectRef::new(Some("shop"), None, "items", NodeKind::TABLE), 50);
        request.filter = Filter::Conditions {
            conditions: vec![
                FilterCond::new("name", FilterOp::Contains, &["50%"]),
                FilterCond::new("price", FilterOp::Between, &["1", "2"]),
                FilterCond::new("id", FilterOp::NotIn, &["3", "4"]),
            ],
            match_any: false,
        };
        request.sort = vec![SortKey::asc("id")];
        request.after = Some(vec![Value::Int(10)]);
        let mut core = adapt::browse_to_common(&request).unwrap();
        core.object.database = Some("shop".into());
        let built = sqlbuild::build_browse(&MYSQL, &core, &columns(), &["id".to_owned()]).unwrap();
        assert_eq!(
            built.sql,
            "SELECT `id`, `name`, `price`, `ratio`, `meta` FROM `shop`.`items` WHERE (LOWER(`name`) LIKE LOWER(?) ESCAPE '\\\\' \
             AND `price` BETWEEN ? AND ? AND `id` NOT IN (?, ?)) AND ((`id` > ?)) ORDER BY `id` ASC LIMIT 51"
        );
        let params = params_of(&built.params).unwrap();
        assert_eq!(params[0], mysql_async::Value::Bytes(b"%50\\%%".to_vec()));
        assert_eq!(params[1], mysql_async::Value::Bytes(b"1".to_vec()));
        assert_eq!(params[3], mysql_async::Value::Int(3));
        assert!(built.keyset_ok);
    }

    #[test]
    fn edits_are_guarded_null_safely_and_limited_to_one_row() {
        let update = RowChange::Update {
            key: vec![Field::new("id", Value::Int(1))],
            values: vec![Field::new("name", Value::Text("new".into()))],
            original: vec![
                Field::new("name", Value::Text("old".into())),
                Field::new("meta", Value::Json("{}".into())),
                Field::new("ratio", Value::Float(0.1)),
            ],
            version: None,
            force: false,
        };
        let built = sqlbuild::build_guarded(&MYSQL, &object(), &update, &columns(), None).unwrap();
        assert_eq!(
            built.sql,
            "UPDATE `shop`.`items` SET `name` = ? WHERE `id` = ? AND `name` <=> ? AND `meta` <=> CAST(? AS JSON) \
             AND `ratio` <=> CAST(? AS FLOAT) LIMIT 1"
        );
        let maria = sqlbuild::build_guarded(&MySqlDialect { mariadb: true }, &object(), &update, &columns(), None).unwrap();
        assert!(maria.sql.contains("`meta` <=> ? AND"), "{}", maria.sql);
        let delete = RowChange::Delete {
            key: vec![Field::new("id", Value::Int(1))],
            original: vec![Field::new("name", Value::Null)],
            version: None,
            force: false,
        };
        let built = sqlbuild::build_guarded(&MYSQL, &object(), &delete, &columns(), None).unwrap();
        assert_eq!(built.sql, "DELETE FROM `shop`.`items` WHERE `id` = ? AND `name` <=> ? LIMIT 1");
        assert_eq!(params_of(&built.params).unwrap()[1], mysql_async::Value::NULL);
        let forced = RowChange::Delete {
            key: vec![Field::new("id", Value::Int(1))],
            original: vec![Field::new("name", Value::Null)],
            version: Some(Value::Int(3)),
            force: true,
        };
        let built = sqlbuild::build_guarded(&MYSQL, &object(), &forced, &columns(), None).unwrap();
        assert_eq!(built.sql, "DELETE FROM `shop`.`items` WHERE `id` = ? LIMIT 1");
    }

    #[test]
    fn typed_values_are_checked_against_their_columns() {
        let insert = RowChange::Insert {
            values: vec![
                Field::new("id", Value::Text("abc".into())),
                Field::new("name", Value::Text("x".into())),
            ],
        };
        let built = sqlbuild::build_guarded(&MYSQL, &object(), &insert, &columns(), None).unwrap();
        assert_eq!(built.sql, "INSERT INTO `shop`.`items` (`id`, `name`) VALUES (?, ?)");
        assert!(params_of(&built.params).is_err(), "'abc' is not a whole number");
        assert_eq!(
            with_timeout(false, "SELECT 1".into(), 2),
            "SELECT /*+ MAX_EXECUTION_TIME(2000) */ 1"
        );
        assert_eq!(
            with_timeout(true, "SELECT 1".into(), 2),
            "SET STATEMENT max_statement_time = 2 FOR SELECT 1"
        );
    }
}
