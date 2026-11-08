//! Browsing and editing rows: T-SQL from `engines::common::sqlbuild` with every value bound as `@Pn`, run with
//! `sp_executesql` on a pooled client. Edits run in one transaction per call; each update and delete is guarded by the
//! row's `rowversion` when the table has one (the page returns it per row) and by the original values otherwise, and any
//! change that matches no row rolls everything back with a `CONFLICT` listing the rows as they are now.

use std::time::Instant;

use async_trait::async_trait;
use futures_util::TryStreamExt;
use tiberius::{QueryItem, ToSql};

use super::{
    adapter::MssqlConnection,
    catalog::{self, bracket, select_expr, target_of, TableColumns, Target},
    connect::{within, TdsClient},
    values::{base_type, coerce, to_param, to_value, Param},
};
use crate::{
    contract::{
        tabular::{conflict_error, RowConflict},
        ApplyResult, BrowseRequest, CallContext, ColumnInfo, Field, Filter, Page, RowChange, RowChangeSet, TabularData, Total, Value,
        ValueKind,
    },
    engines::common::{
        self, adapt,
        sqlbuild::{self, ChangeSql, ParamTarget, SqlDialect},
    },
    error::AppResult,
};

/// Pages are capped so one request cannot pull a whole table.
const MAX_PAGE_SIZE: u64 = 5000;
/// Below this many rows (by statistics) a total is counted exactly; above it the estimate is shown.
const EXACT_COUNT_BELOW: u64 = 50_000;

/// T-SQL: `[ident]` quoting, `@Pn` placeholders, `TOP (n)` for first and keyset pages and `OFFSET … FETCH` otherwise,
/// `LIKE … ESCAPE '\'` with `[` escaped too.
pub struct TsqlDialect;

impl SqlDialect for TsqlDialect {
    fn quote_ident(&self, name: &str) -> String {
        bracket(name)
    }

    fn table_sql(&self, obj: &common::ObjectRef) -> String {
        let schema = obj.schema.as_deref().unwrap_or(catalog::DEFAULT_SCHEMA);
        match obj.database.as_deref() {
            Some(database) => format!("{}.{}.{}", bracket(database), bracket(schema), bracket(&obj.name)),
            None => format!("{}.{}", bracket(schema), bracket(&obj.name)),
        }
    }

    fn placeholder(&self, n: usize, _target: &ParamTarget) -> String {
        format!("@P{n}")
    }

    fn select_expr(&self, col: &ColumnInfo) -> String {
        select_expr(col)
    }

    /// Binary as `0x…`, temporals in ODBC canonical form (style 121), the rest as `nvarchar(max)`.
    fn text_expr(&self, col_sql: &str, col: &ColumnInfo) -> String {
        match col.kind {
            ValueKind::Text if !matches!(base_type(col).as_str(), "text" | "ntext") => col_sql.to_owned(),
            ValueKind::Bytes => format!("CONVERT(NVARCHAR(MAX), {col_sql}, 1)"),
            ValueKind::Date | ValueKind::Time | ValueKind::Timestamp | ValueKind::TimestampTz => {
                format!("CONVERT(NVARCHAR(MAX), {col_sql}, 121)")
            }
            _ => format!("CAST({col_sql} AS NVARCHAR(MAX))"),
        }
    }

    /// Case-insensitive matches lower both sides, whatever the column's collation.
    fn like(&self, lhs: &str, rhs: &str, ci: bool, negate: bool) -> String {
        let not = if negate { "NOT " } else { "" };
        if ci {
            format!("LOWER({lhs}) {not}LIKE LOWER({rhs}) ESCAPE '\\'")
        } else {
            format!("{lhs} {not}LIKE {rhs} ESCAPE '\\'")
        }
    }

    fn limit_offset(&self, limit: u64, offset: u64) -> String {
        format!("OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY")
    }

    fn top(&self, limit: u64, offset: u64) -> Option<String> {
        (offset == 0).then(|| format!("TOP ({limit})"))
    }

    fn unordered(&self) -> Option<&'static str> {
        Some("(SELECT NULL)")
    }

    fn like_specials(&self) -> &'static str {
        "%_\\["
    }

    /// `datetime` keeps 1/300 s: the value read back is compared after the same rounding.
    fn compare_param(&self, placeholder: String, col: &ColumnInfo) -> String {
        match base_type(col).as_str() {
            "datetime" => format!("CAST({placeholder} AS DATETIME)"),
            "smalldatetime" => format!("CAST({placeholder} AS SMALLDATETIME)"),
            _ => placeholder,
        }
    }

    /// `xml`, `text`, `ntext` and `image` have no `=`; they are compared as their (n)varchar(max) form.
    fn compare_column(&self, col_sql: String, col: &ColumnInfo) -> String {
        match base_type(col).as_str() {
            "xml" | "ntext" => format!("CAST({col_sql} AS NVARCHAR(MAX))"),
            "text" => format!("CAST({col_sql} AS VARCHAR(MAX))"),
            "image" => format!("CAST({col_sql} AS VARBINARY(MAX))"),
            _ => col_sql,
        }
    }
}

/// Bound parameters as TDS values, user values coerced to their column's kind.
pub fn params_of(params: &[sqlbuild::Param]) -> AppResult<Vec<Param>> {
    params
        .iter()
        .map(|param| match &param.target {
            ParamTarget::Column(column) => Ok(to_param(&coerce(&param.value, column)?, Some(column))),
            ParamTarget::Text => Ok(to_param(&param.value, None)),
        })
        .collect()
}

fn refs(params: &[Param]) -> Vec<&dyn ToSql> {
    params.iter().map(|param| param as &dyn ToSql).collect()
}

/// Every row of the first result set as values of `columns`.
async fn fetch_rows(client: &mut TdsClient, sql: &str, params: &[Param], columns: &[ColumnInfo]) -> AppResult<Vec<Vec<Value>>> {
    let mut stream = client.query(sql, &refs(params)).await?;
    let mut rows = Vec::new();
    let mut sets = 0;
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(_) => sets += 1,
            QueryItem::Row(row) if sets <= 1 => {
                rows.push(row.into_iter().zip(columns).map(|(data, column)| to_value(data, column)).collect())
            }
            QueryItem::Row(_) => {}
        }
    }
    Ok(rows)
}

/// The first value of the last result set as a number (`SELECT @@ROWCOUNT`, `COUNT_BIG(*)`).
async fn fetch_count(client: &mut TdsClient, sql: &str, params: &[Param]) -> AppResult<u64> {
    let mut stream = client.query(sql, &refs(params)).await?;
    let mut count = 0;
    while let Some(item) = stream.try_next().await? {
        if let QueryItem::Row(row) = item {
            if let Some(Value::Int(n)) = row
                .into_iter()
                .next()
                .map(|data| to_value(data, &ColumnInfo::result("", "", ValueKind::Int)))
            {
                count = n.max(0) as u64;
            }
        }
    }
    Ok(count)
}

fn core_object(target: &Target, object: &crate::contract::ObjectRef) -> AppResult<common::ObjectRef> {
    let mut core = adapt::object_to_common(object)?;
    core.database = Some(target.database.clone());
    core.schema = Some(target.schema.clone());
    core.name = target.name.clone();
    Ok(core)
}

impl MssqlConnection {
    async fn browse_page(&self, client: &mut TdsClient, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        let started = Instant::now();
        let target = target_of(&request.object)?;
        let TableColumns {
            columns,
            primary_key,
            version_column,
        } = catalog::table_columns(client, &target).await?;
        let wanted = |column: &ColumnInfo| request.columns.as_ref().is_none_or(|wanted| wanted.contains(&column.name));
        // The rowversion column is read even when not shown: the page returns it per row.
        let selected: Vec<ColumnInfo> = columns
            .iter()
            .filter(|column| wanted(column) || version_column.as_deref() == Some(column.name.as_str()))
            .cloned()
            .collect();
        let raw = match &request.filter {
            Filter::Raw { text, .. } => Some(text.clone()),
            _ => None,
        };
        let mut core = adapt::browse_to_common(&BrowseRequest {
            filter: if raw.is_some() { Filter::None } else { request.filter.clone() },
            ..request.clone()
        })?;
        core.object = core_object(&target, &request.object)?;
        core.limit = request.limit.clamp(1, MAX_PAGE_SIZE);
        let built = sqlbuild::build_browse_with_raw(&TsqlDialect, &core, &selected, &primary_key, raw.as_deref())?;
        let params = params_of(&built.params)?;
        let timeout = ctx.limits.timeout_secs;
        let rows = within(timeout, fetch_rows(client, &built.sql, &params, &selected)).await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let total = if request.exact_total || request.estimate_total {
            self.total(client, &target, request, &built, &params, timeout).await?
        } else {
            None
        };
        let mut page = adapt::page_to_contract(
            sqlbuild::finish_page(selected, rows, core.limit, &built, primary_key, None),
            elapsed_ms,
        );
        page.total = total;
        if let Some(version) = version_column {
            if let Some(index) = page.columns.iter().position(|column| column.name == version) {
                page.row_versions = Some(page.rows.iter().map(|row| row[index].clone()).collect());
                if !columns.iter().any(|column| column.name == version && wanted(column)) {
                    page.columns.remove(index);
                    for row in &mut page.rows {
                        row.remove(index);
                    }
                }
            }
        }
        Ok(page)
    }

    /// The exact count when asked for or cheap (small tables), else the statistics' estimate for unfiltered tables.
    async fn total(
        &self,
        client: &mut TdsClient,
        target: &Target,
        request: &BrowseRequest,
        built: &sqlbuild::BrowseSql,
        params: &[Param],
        timeout: u32,
    ) -> AppResult<Option<Total>> {
        let estimate = catalog::table_estimate(client, target).await?;
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
        let sql = format!("SELECT COUNT_BIG(*) FROM {}{where_clause}", target.qualified());
        let value = within(timeout, fetch_count(client, &sql, &params[..filter_params])).await?;
        Ok(Some(Total { value, exact: true }))
    }

    async fn apply_set(&self, client: &mut TdsClient, set: &RowChangeSet) -> AppResult<ApplyResult> {
        let target = target_of(&set.object)?;
        let TableColumns {
            columns, version_column, ..
        } = catalog::table_columns(client, &target).await?;
        let object = core_object(&target, &set.object)?;
        let statements = set
            .changes
            .iter()
            .map(|change| {
                let built = sqlbuild::build_guarded(&TsqlDialect, &object, change, &columns, version_column.as_deref())?;
                Ok((params_of(&built.params)?, built))
            })
            .collect::<AppResult<Vec<(Vec<Param>, ChangeSql)>>>()?;
        let mut result = ApplyResult::default();
        let mut missed = Vec::new();
        client
            .simple_query("SET XACT_ABORT OFF; BEGIN TRANSACTION")
            .await?
            .into_results()
            .await?;
        let applied: AppResult<()> = async {
            for (index, (change, (params, built))) in set.changes.iter().zip(statements).enumerate() {
                // @@ROWCOUNT counts this statement only (not rows changed by triggers) and does not depend on NOCOUNT.
                let sql = format!("{}; SELECT CAST(@@ROWCOUNT AS BIGINT)", built.sql);
                let affected = fetch_count(client, &sql, &params).await?;
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
            rollback(client).await?;
            return Err(error);
        }
        if missed.is_empty() {
            client.simple_query("COMMIT TRANSACTION").await?.into_results().await?;
            return Ok(result);
        }
        rollback(client).await?;
        let mut conflicts = Vec::with_capacity(missed.len());
        for index in missed {
            let change = &set.changes[index];
            let current = reread(client, &object, change.key(), &columns).await?;
            conflicts.push(RowConflict::new(index, change, current));
        }
        Err(conflict_error(&set.object, conflicts))
    }
}

async fn rollback(client: &mut TdsClient) -> AppResult<()> {
    client
        .simple_query("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION")
        .await?
        .into_results()
        .await?;
    Ok(())
}

/// The row with this key as it is now (all columns), or `None` when it no longer exists.
async fn reread(
    client: &mut TdsClient,
    object: &common::ObjectRef,
    key: &[Field],
    columns: &[ColumnInfo],
) -> AppResult<Option<Vec<Field>>> {
    let built = sqlbuild::build_reread(&TsqlDialect, object, key, columns)?;
    let rows = fetch_rows(client, &built.sql, &params_of(&built.params)?, columns).await?;
    Ok(rows.into_iter().next().map(|row| {
        columns
            .iter()
            .zip(row)
            .map(|(column, value)| Field::new(&column.name, value))
            .collect()
    }))
}

#[async_trait]
impl TabularData for MssqlConnection {
    async fn browse(&self, request: &BrowseRequest, ctx: &CallContext) -> AppResult<Page> {
        let mut client = self.pool.take().await?;
        let outcome = self.browse_page(&mut client, request, ctx).await;
        self.pool.release(client, &outcome);
        outcome
    }

    async fn apply(&self, changes: &RowChangeSet, _ctx: &CallContext) -> AppResult<ApplyResult> {
        if changes.changes.is_empty() {
            return Ok(ApplyResult::default());
        }
        let mut client = self.pool.take().await?;
        let outcome = self.apply_set(&mut client, changes).await;
        // A conflict leaves the connection clean (rolled back); other errors keep it only when it is a statement error.
        let clean: AppResult<()> = match &outcome {
            Err(error) if error.code() == Some(crate::contract::errors::CONFLICT) => Ok(()),
            Err(error) => Err(error.clone()),
            Ok(_) => Ok(()),
        };
        self.pool.release(client, &clean);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{FilterCond, FilterOp, NodeKind, ObjectRef, SortKey};

    fn columns() -> Vec<ColumnInfo> {
        let mut id = ColumnInfo::result("id", "int", ValueKind::Int);
        id.nullable = false;
        let mut rv = ColumnInfo::result("rv", "rowversion", ValueKind::Bytes);
        rv.nullable = false;
        vec![
            id,
            ColumnInfo::result("name", "nvarchar(100)", ValueKind::Text),
            ColumnInfo::result("price", "decimal(12,2)", ValueKind::Decimal),
            ColumnInfo::result("seen", "datetime", ValueKind::Timestamp),
            ColumnInfo::result("doc", "xml", ValueKind::Other),
            rv,
        ]
    }

    fn object() -> common::ObjectRef {
        common::ObjectRef::new(Some("shop"), Some("dbo"), "items", common::NodeKind::Table)
    }

    fn request() -> BrowseRequest {
        BrowseRequest::new(ObjectRef::new(Some("shop"), Some("dbo"), "items", NodeKind::TABLE), 50)
    }

    fn build(request: &BrowseRequest) -> sqlbuild::BrowseSql {
        let core = adapt::browse_to_common(request).unwrap();
        sqlbuild::build_browse(&TsqlDialect, &core, &columns(), &["id".to_owned()]).unwrap()
    }

    #[test]
    fn first_and_keyset_pages_use_top_and_others_offset_fetch() {
        let mut request = request();
        request.filter = Filter::Conditions {
            conditions: vec![
                FilterCond::new("name", FilterOp::Contains, &["50%[a]"]),
                FilterCond::new("price", FilterOp::Between, &["1", "2"]),
                FilterCond::new("id", FilterOp::NotIn, &["3", "4"]),
            ],
            match_any: false,
        };
        request.sort = vec![SortKey::asc("id")];
        let first = build(&request);
        assert_eq!(
            first.sql,
            "SELECT TOP (51) [id], [name], [price], [seen], [doc], [rv] FROM [shop].[dbo].[items] \
             WHERE (LOWER([name]) LIKE LOWER(@P1) ESCAPE '\\' AND [price] BETWEEN @P2 AND @P3 AND [id] NOT IN (@P4, @P5)) \
             ORDER BY [id] ASC"
        );
        let params = params_of(&first.params).unwrap();
        assert_eq!(params[0], Param(tiberius::ColumnData::String(Some("%50\\%\\[a]%".into()))));
        assert_eq!(params[3], Param(tiberius::ColumnData::I32(Some(3))));
        request.after = Some(vec![Value::Int(10)]);
        let keyset = build(&request);
        assert!(keyset.sql.starts_with("SELECT TOP (51) "), "{}", keyset.sql);
        assert!(keyset.sql.contains("AND (([id] > @P6)) ORDER BY [id] ASC"), "{}", keyset.sql);
        request.after = None;
        request.offset = 100;
        request.filter = Filter::None;
        request.sort = Vec::new();
        let offset = build(&request);
        assert_eq!(
            offset.sql,
            "SELECT [id], [name], [price], [seen], [doc], [rv] FROM [shop].[dbo].[items] ORDER BY [id] ASC \
             OFFSET 100 ROWS FETCH NEXT 51 ROWS ONLY"
        );
        let core = adapt::browse_to_common(&request).unwrap();
        let unkeyed = sqlbuild::build_browse(&TsqlDialect, &core, &columns(), &[]).unwrap();
        assert!(
            unkeyed
                .sql
                .ends_with("ORDER BY (SELECT NULL) OFFSET 100 ROWS FETCH NEXT 51 ROWS ONLY"),
            "{}",
            unkeyed.sql
        );
    }

    #[test]
    fn versioned_edits_compare_the_rowversion() {
        let update = RowChange::Update {
            key: vec![Field::new("id", Value::Int(1))],
            values: vec![Field::new("name", Value::Text("new".into()))],
            original: vec![Field::new("name", Value::Text("old".into()))],
            version: Some(Value::Bytes(vec![0, 0, 0, 0, 0, 0, 7, 209])),
            force: false,
        };
        let built = sqlbuild::build_guarded(&TsqlDialect, &object(), &update, &columns(), Some("rv")).unwrap();
        assert_eq!(
            built.sql,
            "UPDATE [shop].[dbo].[items] SET [name] = @P1 WHERE [id] = @P2 AND [rv] = @P3"
        );
        let params = params_of(&built.params).unwrap();
        assert_eq!(params[1], Param(tiberius::ColumnData::I32(Some(1))));
        assert_eq!(
            params[2],
            Param(tiberius::ColumnData::Binary(Some(vec![0, 0, 0, 0, 0, 0, 7, 209].into())))
        );
        let delete = RowChange::Delete {
            key: vec![Field::new("id", Value::Int(1))],
            original: Vec::new(),
            version: Some(Value::Bytes(vec![1])),
            force: false,
        };
        let built = sqlbuild::build_guarded(&TsqlDialect, &object(), &delete, &columns(), Some("rv")).unwrap();
        assert_eq!(built.sql, "DELETE FROM [shop].[dbo].[items] WHERE [id] = @P1 AND [rv] = @P2");
    }

    #[test]
    fn tables_without_rowversion_compare_original_values_null_safely() {
        let update = RowChange::Update {
            key: vec![Field::new("id", Value::Int(1))],
            values: vec![Field::new("name", Value::Text("new".into()))],
            original: vec![
                Field::new("name", Value::Null),
                Field::new("seen", Value::Text("2026-01-01 08:00:00.003".into())),
                Field::new("doc", Value::Text("<a/>".into())),
            ],
            version: None,
            force: false,
        };
        let built = sqlbuild::build_guarded(&TsqlDialect, &object(), &update, &columns(), None).unwrap();
        assert_eq!(
            built.sql,
            "UPDATE [shop].[dbo].[items] SET [name] = @P1 WHERE [id] = @P2 AND ([name] = @P3 OR ([name] IS NULL AND @P4 IS NULL)) \
             AND ([seen] = CAST(@P5 AS DATETIME) OR ([seen] IS NULL AND @P6 IS NULL)) \
             AND (CAST([doc] AS NVARCHAR(MAX)) = @P7 OR (CAST([doc] AS NVARCHAR(MAX)) IS NULL AND @P8 IS NULL))"
        );
        let params = params_of(&built.params).unwrap();
        assert_eq!(params[2], Param(tiberius::ColumnData::String(None)));
        assert!(matches!(params[4].0, tiberius::ColumnData::DateTime2(Some(_))), "{:?}", params[4]);
        // A forced overwrite checks only the key; a re-read takes one row.
        let forced = RowChange::Delete {
            key: vec![Field::new("id", Value::Int(1))],
            original: vec![Field::new("name", Value::Null)],
            version: None,
            force: true,
        };
        let built = sqlbuild::build_guarded(&TsqlDialect, &object(), &forced, &columns(), Some("rv")).unwrap();
        assert_eq!(built.sql, "DELETE FROM [shop].[dbo].[items] WHERE [id] = @P1");
        let reread = sqlbuild::build_reread(&TsqlDialect, &object(), &[Field::new("id", Value::Int(1))], &columns()).unwrap();
        assert_eq!(
            reread.sql,
            "SELECT TOP (1) [id], [name], [price], [seen], [doc], [rv] FROM [shop].[dbo].[items] WHERE [id] = @P1"
        );
    }

    #[test]
    fn typed_values_are_checked_against_their_columns() {
        let insert = RowChange::Insert {
            values: vec![
                Field::new("id", Value::Text("abc".into())),
                Field::new("name", Value::Text("x".into())),
            ],
        };
        let built = sqlbuild::build_guarded(&TsqlDialect, &object(), &insert, &columns(), None).unwrap();
        assert_eq!(built.sql, "INSERT INTO [shop].[dbo].[items] ([id], [name]) VALUES (@P1, @P2)");
        assert!(params_of(&built.params).is_err(), "'abc' is not a whole number");
        let blob = ColumnInfo::result("b", "varbinary(4)", ValueKind::Bytes);
        assert_eq!(TsqlDialect.text_expr("[b]", &blob), "CONVERT(NVARCHAR(MAX), [b], 1)");
        assert_eq!(TsqlDialect.text_expr("[name]", &columns()[1]), "[name]");
    }
}
