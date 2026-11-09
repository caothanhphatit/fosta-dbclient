//! Browsing and editing rows for the contract. Browse SQL comes from `engines::common::sqlbuild` with every value bound
//! (as text, cast by the server to the column's type). Tables carry their row version: `xmin::text` is selected next to
//! the columns and returned as `Page::row_versions`; updates and deletes compare it (or, for views and partitioned
//! tables, the original values). Edits run in one transaction; a change that matches no row rolls everything back with
//! a `CONFLICT` listing the rows as they are now.

use std::time::Instant;

use super::{as_sql, catalog, catalog::RELATION_KIND, map_err, text_params, PgDialect, PgEngine};
use crate::{
    contract::{
        cancelled,
        tabular::{conflict_error, RowConflict},
        ApplyResult, BrowseRequest, CallContext, ColumnInfo, Field, Filter, ObjectDescription, Page, RowChange, RowChangeSet, Total, Value,
        ValueKind,
    },
    engines::common::{
        self, adapt,
        sqlbuild::{self, BrowseSql, ParamTarget, SqlDialect},
    },
    error::AppResult,
};

/// Pages are capped so one request cannot pull a whole table.
const MAX_PAGE_SIZE: u64 = 5000;
/// Below this many rows (by statistics) a total is counted exactly; above it the estimate is shown.
const EXACT_COUNT_BELOW: i64 = 50_000;
/// The version column as the SQL builder names it; the edit dialect writes it as `xmin::text`.
const VERSION: &str = "xmin::text";

/// [`PgDialect`] that writes the pseudo column [`VERSION`] unquoted.
struct EditDialect;

impl SqlDialect for EditDialect {
    fn quote_ident(&self, name: &str) -> String {
        if name == VERSION {
            VERSION.to_owned()
        } else {
            PgDialect.quote_ident(name)
        }
    }
    fn table_sql(&self, obj: &common::ObjectRef) -> String {
        PgDialect.table_sql(obj)
    }
    fn placeholder(&self, n: usize, target: &ParamTarget) -> String {
        PgDialect.placeholder(n, target)
    }
    fn select_expr(&self, col: &ColumnInfo) -> String {
        if col.name == VERSION {
            VERSION.to_owned()
        } else {
            PgDialect.select_expr(col)
        }
    }
    fn text_expr(&self, col_sql: &str, col: &ColumnInfo) -> String {
        PgDialect.text_expr(col_sql, col)
    }
    fn like(&self, lhs: &str, rhs: &str, ci: bool, negate: bool) -> String {
        PgDialect.like(lhs, rhs, ci, negate)
    }
    fn null_safe_eq(&self) -> Option<&'static str> {
        PgDialect.null_safe_eq()
    }
    fn compare_column(&self, col_sql: String, col: &ColumnInfo) -> String {
        PgDialect.compare_column(col_sql, col)
    }
    fn compare_param(&self, placeholder: String, col: &ColumnInfo) -> String {
        PgDialect.compare_param(placeholder, col)
    }
}

fn version_column() -> ColumnInfo {
    ColumnInfo::result(VERSION, "text", ValueKind::Text)
}

/// Only ordinary tables have a trustworthy `xmin` per row (a partitioned parent has none of its own).
fn has_version(description: &ObjectDescription) -> bool {
    description.object.kind == crate::contract::NodeKind::TABLE
        && description
            .extra
            .iter()
            .any(|(key, value)| key == RELATION_KIND && value == "table")
}

fn convert(description: common::ObjectDescription) -> ObjectDescription {
    adapt::description_to_contract(description, None)
}

impl PgEngine {
    pub(super) async fn describe_contract(&self, object: &crate::contract::ObjectRef) -> AppResult<ObjectDescription> {
        let core = adapt::object_to_common(object)?;
        let mut description = convert(self.describe(&core).await?);
        description.object.database = object.database.clone().or_else(|| Some(self.database().to_owned()));
        Ok(description)
    }

    /// Runs `work` with the rules' statement timeout on the shared connection (reset afterwards).
    async fn timed<T>(
        &self,
        client: &tokio_postgres::Client,
        seconds: u32,
        work: impl std::future::Future<Output = AppResult<T>>,
    ) -> AppResult<T> {
        if seconds == 0 {
            return work.await;
        }
        client
            .batch_execute(&format!("SET statement_timeout = {}", u64::from(seconds) * 1000))
            .await
            .map_err(map_err)?;
        let result = work.await;
        let _ = client.batch_execute("RESET statement_timeout").await;
        result
    }

    /// One page on the shared connection. Cancelling (`ctx.cancel`) while it waits for the connection gives up the wait;
    /// while it runs, PostgreSQL's CancelRequest stops the statement, so the connection is free at once.
    pub(super) async fn browse_page(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        let client = tokio::select! {
            client = self.client.lock() => client,
            () = ctx.cancel.cancelled() => return Err(cancelled()),
        };
        let token = client.cancel_token();
        let work = self.read_page(&client, request, ctx);
        tokio::pin!(work);
        tokio::select! {
            outcome = &mut work => outcome,
            () = ctx.cancel.cancelled() => {
                let _ = token.cancel_query(self.tls.clone()).await;
                let _ = work.await;
                absorb_late_cancel(&client).await;
                Err(cancelled())
            }
        }
    }

    async fn read_page(&self, client: &tokio_postgres::Client, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        let started = Instant::now();
        let object = adapt::object_to_common(&request.object)?;
        let description = convert(catalog::describe(client, &object).await.map_err(AppError::from)?);
        let versioned = has_version(&description);
        let primary_key: Vec<String> = description.primary_key.as_ref().map(|key| key.columns.clone()).unwrap_or_default();
        let shown: Vec<ColumnInfo> = match &request.columns {
            Some(wanted) => description
                .columns
                .iter()
                .filter(|column| wanted.contains(&column.name))
                .cloned()
                .collect(),
            None => description.columns.clone(),
        };
        let mut selected = shown.clone();
        if versioned {
            selected.push(version_column());
        }
        let raw = match &request.filter {
            Filter::Raw { text, .. } => Some(text.clone()),
            _ => None,
        };
        let mut core = adapt::browse_to_common(&BrowseRequest {
            filter: if raw.is_some() { Filter::None } else { request.filter.clone() },
            ..request.clone()
        })?;
        core.object = adapt::object_to_common(&description.object)?;
        core.limit = request.limit.clamp(1, MAX_PAGE_SIZE);
        // Table columns without the version, for the filter and sort lookups; the select list adds the version.
        let built = sqlbuild::build_browse_with_raw(&EditDialect, &core, &selected, &primary_key, raw.as_deref())?;
        let params = text_params(&built.params);
        let rows = self
            .timed(client, ctx.limits.timeout_secs, async {
                client
                    .query(&built.sql, &as_sql(&params))
                    .await
                    .map_err(|error| map_err(error).into())
            })
            .await?;
        let mut rows: Vec<Vec<Value>> = rows.iter().map(super::types::row_values).collect();
        let versions = versioned.then(|| rows.iter_mut().map(|row| row.pop().unwrap_or(Value::Null)).collect::<Vec<_>>());
        let total = if request.exact_total || request.estimate_total {
            self.total(client, &description, request, &built, &params, ctx.limits.timeout_secs)
                .await?
        } else {
            None
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let mut page = adapt::page_to_contract(
            sqlbuild::finish_page(shown, rows, core.limit, &built, primary_key, None),
            elapsed_ms,
        );
        if let Some(versions) = versions {
            page.row_versions = Some(sqlbuild::page_order(versions, page.rows.len() as u64, &built));
        }
        page.total = total;
        Ok(page)
    }

    /// The exact count when asked for or the result is small; otherwise an estimate (statistics, else the planner).
    async fn total(
        &self,
        client: &tokio_postgres::Client,
        description: &ObjectDescription,
        request: &BrowseRequest,
        built: &BrowseSql,
        params: &[Option<String>],
        timeout: u32,
    ) -> AppResult<Option<Total>> {
        let filtered = !built.where_sql.is_empty();
        // The keyset parameters come after the filter's.
        let filter_params = params.len() - request.after.as_ref().map_or(0, |after| after.len() * (after.len() + 1) / 2);
        let table = EditDialect.table_sql(&adapt::object_to_common(&description.object)?);
        let where_clause = if filtered {
            format!(" WHERE {}", built.where_sql)
        } else {
            String::new()
        };
        // How big the result is likely to be: the statistics for a whole table, else the planner's estimate (also when
        // the statistics are missing, as on a table that was never analyzed, so no unbounded COUNT(*) runs by default).
        let hint = if request.exact_total {
            None
        } else if filtered || description.row_estimate.is_none() {
            let sql = format!("EXPLAIN (FORMAT JSON) SELECT 1 FROM {table}{where_clause}");
            client
                .query_one(&sql, &as_sql(&params[..filter_params]))
                .await
                .ok()
                .and_then(|row| super::plan_rows(&super::types::row_values(&row)[0]))
        } else {
            description.row_estimate
        };
        let count = request.exact_total || hint.is_none_or(|rows| rows < EXACT_COUNT_BELOW);
        if count {
            let sql = format!("SELECT count(*) FROM {table}{where_clause}");
            let value: i64 = self
                .timed(client, timeout, async {
                    client
                        .query_one(&sql, &as_sql(&params[..filter_params]))
                        .await
                        .map(|row| row.get(0))
                        .map_err(|error| map_err(error).into())
                })
                .await?;
            return Ok(Some(Total {
                value: value.max(0) as u64,
                exact: true,
            }));
        }
        Ok(hint.map(|rows| Total {
            value: rows.max(0) as u64,
            exact: false,
        }))
    }

    pub(super) async fn apply_set(&self, set: &RowChangeSet) -> AppResult<ApplyResult> {
        let mut client = self.client.lock().await;
        let object = adapt::object_to_common(&set.object)?;
        let description = convert(catalog::describe(&client, &object).await.map_err(AppError::from)?);
        let version = has_version(&description).then_some(VERSION);
        let mut object = adapt::object_to_common(&description.object)?;
        object.database = set.object.database.clone();
        let statements = set
            .changes
            .iter()
            .map(|change| {
                let built = sqlbuild::build_guarded(&EditDialect, &object, change, &description.columns, version)?;
                Ok((text_params(&built.params), built))
            })
            .collect::<AppResult<Vec<_>>>()?;
        let mut result = ApplyResult::default();
        let mut missed = Vec::new();
        let tx = client.transaction().await.map_err(map_err)?;
        for (index, (change, (params, built))) in set.changes.iter().zip(&statements).enumerate() {
            let affected = tx.execute(&built.sql, &as_sql(params)).await.map_err(map_err)?;
            match change {
                RowChange::Insert { .. } => result.inserted += affected,
                RowChange::Update { .. } if affected == 0 => missed.push(index),
                RowChange::Update { .. } => result.updated += affected,
                RowChange::Delete { .. } if affected == 0 => missed.push(index),
                RowChange::Delete { .. } => result.deleted += affected,
            }
        }
        if missed.is_empty() {
            tx.commit().await.map_err(map_err)?;
            return Ok(result);
        }
        tx.rollback().await.map_err(map_err)?;
        let mut conflicts = Vec::with_capacity(missed.len());
        for index in missed {
            let change = &set.changes[index];
            let current = self.reread(&client, &object, change.key(), &description.columns).await?;
            conflicts.push(RowConflict::new(index, change, current));
        }
        Err(conflict_error(&set.object, conflicts))
    }

    /// The row with this key as it is now (all columns), or `None` when it no longer exists.
    async fn reread(
        &self,
        client: &tokio_postgres::Client,
        object: &common::ObjectRef,
        key: &[Field],
        columns: &[ColumnInfo],
    ) -> AppResult<Option<Vec<Field>>> {
        let built = sqlbuild::build_reread(&EditDialect, object, key, columns)?;
        let params = text_params(&built.params);
        let row = client.query_opt(&built.sql, &as_sql(&params)).await.map_err(map_err)?;
        Ok(row.map(|row| {
            let values = super::types::row_values(&row);
            columns
                .iter()
                .zip(values)
                .map(|(column, value)| Field::new(&column.name, value))
                .collect()
        }))
    }
}

use crate::error::AppError;

/// The backend gets a CancelRequest asynchronously (the postmaster signals it), and the connection is shared with the
/// tree and the other tabs: when the page read ended before the signal arrived, it could stop the next statement sent on
/// it. One statement sent while the lock is still held takes it instead (a signal arriving while the backend is idle is
/// ignored by the server).
async fn absorb_late_cancel(client: &tokio_postgres::Client) {
    for _ in 0..2 {
        match client.simple_query("SELECT 1").await {
            Err(error) if error.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED) => continue,
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{FilterCond, FilterOp, NodeKind, ObjectRef, SortKey};

    fn columns() -> Vec<ColumnInfo> {
        let mut id = ColumnInfo::result("id", "integer", ValueKind::Int);
        id.nullable = false;
        vec![
            id,
            ColumnInfo::result("name", "text", ValueKind::Text),
            ColumnInfo::result("meta", "json", ValueKind::Json),
            ColumnInfo::result("tags", "text[]", ValueKind::Array(Box::new(ValueKind::Text))),
        ]
    }

    fn object() -> common::ObjectRef {
        common::ObjectRef::new(Some("shop"), Some("public"), "items", common::NodeKind::Table)
    }

    #[test]
    fn browse_selects_the_version_and_binds_filters_as_text() {
        let mut with_version = columns();
        with_version.push(version_column());
        let mut request = BrowseRequest::new(ObjectRef::new(Some("shop"), Some("public"), "items", NodeKind::TABLE), 50);
        request.filter = Filter::Conditions {
            conditions: vec![
                FilterCond::new("name", FilterOp::Contains, &["50%"]),
                FilterCond::new("id", FilterOp::Between, &["1", "2"]),
            ],
            match_any: false,
        };
        request.sort = vec![SortKey::desc("id")];
        let mut core = adapt::browse_to_common(&request).unwrap();
        core.object = object();
        let built = sqlbuild::build_browse(&EditDialect, &core, &with_version, &["id".to_owned()]).unwrap();
        assert_eq!(
            built.sql,
            "SELECT \"id\", \"name\", \"meta\", \"tags\", xmin::text FROM \"public\".\"items\" \
             WHERE (\"name\"::text ILIKE $1::text AND \"id\" BETWEEN ($2::text)::integer AND ($3::text)::integer) \
             ORDER BY \"id\" DESC LIMIT 51"
        );
    }

    #[test]
    fn versioned_edits_compare_xmin_as_text_and_others_the_original_values() {
        let version = RowChange::Update {
            key: vec![Field::new("id", Value::Int(1))],
            values: vec![Field::new("name", Value::Text("new".into()))],
            original: vec![Field::new("name", Value::Text("old".into()))],
            version: Some(Value::Text("812".into())),
            force: false,
        };
        let built = sqlbuild::build_guarded(&EditDialect, &object(), &version, &columns(), Some(VERSION)).unwrap();
        assert_eq!(
            built.sql,
            "UPDATE \"public\".\"items\" SET \"name\" = ($1::text)::text WHERE \"id\" = ($2::text)::integer AND xmin::text = $3::text"
        );
        assert_eq!(
            text_params(&built.params),
            vec![Some("new".into()), Some("1".into()), Some("812".into())]
        );
        let original = RowChange::Delete {
            key: vec![Field::new("id", Value::Int(1))],
            original: vec![Field::new("name", Value::Null), Field::new("meta", Value::Json("{}".into()))],
            version: None,
            force: false,
        };
        let built = sqlbuild::build_guarded(&EditDialect, &object(), &original, &columns(), None).unwrap();
        assert_eq!(
            built.sql,
            "DELETE FROM \"public\".\"items\" WHERE \"id\" = ($1::text)::integer AND \"name\" IS NOT DISTINCT FROM ($2::text)::text \
             AND \"meta\"::text IS NOT DISTINCT FROM (($3::text)::json)::text"
        );
        let forced = RowChange::Delete {
            key: vec![Field::new("id", Value::Int(1))],
            original: vec![Field::new("name", Value::Null)],
            version: Some(Value::Text("9".into())),
            force: true,
        };
        let built = sqlbuild::build_guarded(&EditDialect, &object(), &forced, &columns(), Some(VERSION)).unwrap();
        assert_eq!(built.sql, "DELETE FROM \"public\".\"items\" WHERE \"id\" = ($1::text)::integer");
    }

    #[test]
    fn only_ordinary_tables_have_a_row_version() {
        let mut description = convert(common::ObjectDescription::new(object()));
        assert!(!has_version(&description), "no relkind recorded");
        description.extra.push((RELATION_KIND.into(), "table".into()));
        assert!(has_version(&description));
        description.extra = vec![(RELATION_KIND.into(), "partitioned table".into())];
        assert!(!has_version(&description));
    }
}
