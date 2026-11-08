//! Browsing a table: SQL from `engines::common::sqlbuild` with every value bound as a `{pN:Type}` query parameter.
//! The key is the table's sorting key (the primary key, which is not unique), so keyset paging follows the table's order.

use std::time::Instant;

use super::{params_of, ChDialect, ChEngine};
use crate::{
    contract::{self, Filter, Total},
    engines::common::{
        adapt,
        sqlbuild::{self, SqlDialect},
        BrowseRequest, NodeKind, ObjectDescription, Page, Result, Value,
    },
    error::AppResult,
};

/// Pages are capped so one request cannot pull a whole table.
const MAX_PAGE_SIZE: u64 = 5000;
/// An unfiltered table is counted exactly (`count()` reads only part metadata) below this many rows by statistics.
const EXACT_COUNT_BELOW: i64 = 5_000_000;
/// A filtered table is counted exactly below this many rows by statistics.
const EXACT_FILTERED_COUNT_BELOW: i64 = 50_000;

/// What a browse needs beyond the structured request.
#[derive(Default)]
pub struct BrowseExtras<'a> {
    /// A raw filter, appended as one parenthesised condition (the gateway has parsed it as one).
    pub raw: Option<&'a str>,
    /// Only these columns.
    pub columns: Option<&'a [String]>,
    /// The rules' statement timeout in seconds (0 = none), enforced by the server.
    pub timeout_secs: u32,
    /// Count the rows exactly, whatever the statistics say.
    pub exact_total: bool,
}

impl ChEngine {
    /// A page for the core's request shape (used by the core's own tests): the estimate, never an exact count.
    pub async fn browse(&self, request: &BrowseRequest) -> Result<Page> {
        let (mut page, total) = self.browse_core(request, &BrowseExtras::default()).await?;
        page.estimated_total = total.map(|total| total.value as i64);
        Ok(page)
    }

    /// One page of the contract's request shape.
    pub async fn browse_page(&self, request: &contract::BrowseRequest, timeout_secs: u32) -> AppResult<contract::Page> {
        let started = Instant::now();
        let mut object = adapt::object_to_common(&request.object)?;
        if object.database.is_none() {
            object.database = Some(self.default_database().to_owned());
        }
        let raw = match &request.filter {
            Filter::Raw { text, .. } => Some(text.as_str()),
            _ => None,
        };
        let mut core = adapt::browse_to_common(&contract::BrowseRequest {
            filter: if raw.is_some() { Filter::None } else { request.filter.clone() },
            ..request.clone()
        })?;
        core.object = object;
        let extras = BrowseExtras {
            raw,
            columns: request.columns.as_deref(),
            timeout_secs,
            exact_total: request.exact_total,
        };
        let (page, total) = self.browse_core(&core, &extras).await?;
        let mut page = adapt::page_to_contract(page, started.elapsed().as_millis() as u64);
        page.total = total;
        Ok(page)
    }

    async fn browse_core(&self, request: &BrowseRequest, extras: &BrowseExtras<'_>) -> Result<(Page, Option<Total>)> {
        let description = self.describe(&request.object).await?;
        let key = if description.object.kind == NodeKind::Table {
            description.primary_key.as_ref().map(|key| key.columns.clone()).unwrap_or_default()
        } else {
            Vec::new()
        };
        let columns = match extras.columns {
            Some(wanted) => description
                .columns
                .iter()
                .filter(|column| wanted.contains(&column.name))
                .cloned()
                .collect(),
            None => description.columns.clone(),
        };
        let mut request = request.clone();
        request.object = description.object.clone();
        request.limit = request.limit.clamp(1, MAX_PAGE_SIZE);
        let built = sqlbuild::build_browse_with_raw(&ChDialect, &request, &columns, &key, extras.raw)?;
        let settings = if extras.timeout_secs > 0 {
            vec![
                ("max_execution_time".to_owned(), extras.timeout_secs.to_string()),
                ("timeout_overflow_mode".to_owned(), "throw".to_owned()),
            ]
        } else {
            Vec::new()
        };
        let mut params = params_of(&built.params);
        params.extend(settings.iter().cloned());
        let (_, rows) = self.http().query(&built.sql, &params).await?;
        let total = if request.estimate_total || extras.exact_total {
            self.total(&description, &request, &built, &settings, extras.exact_total).await
        } else {
            None
        };
        Ok((sqlbuild::finish_page(columns, rows, request.limit, &built, key, None), total))
    }

    /// The row count: exact when asked for or cheap (by the statistics); otherwise the statistics for an unfiltered table
    /// and `EXPLAIN ESTIMATE` (rows read from the parts, an upper bound) for a filtered one.
    async fn total(
        &self,
        description: &ObjectDescription,
        request: &BrowseRequest,
        built: &sqlbuild::BrowseSql,
        settings: &[(String, String)],
        exact: bool,
    ) -> Option<Total> {
        let table = ChDialect.table_sql(&description.object);
        // The keyset parameters come after the filter's.
        let filter_params = built.params.len() - request.after.as_ref().map_or(0, |after| after.len() * (after.len() + 1) / 2);
        let mut params = params_of(&built.params[..filter_params]);
        params.extend(settings.iter().cloned());
        let filtered = !built.where_sql.is_empty();
        let estimate = description.row_estimate;
        let cheap = estimate.is_some_and(|rows| rows < if filtered { EXACT_FILTERED_COUNT_BELOW } else { EXACT_COUNT_BELOW });
        if exact || cheap {
            let sql = if filtered {
                format!("SELECT count() FROM {table} WHERE {}", built.where_sql)
            } else {
                format!("SELECT count() FROM {table}")
            };
            if let Ok((_, rows)) = self.http().query(&sql, &params).await {
                let count = rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(Value::display)
                    .and_then(|text| text.parse::<u64>().ok());
                if let Some(value) = count {
                    return Some(Total { value, exact: true });
                }
            }
        }
        if !filtered {
            return estimate.map(|rows| Total {
                value: rows.max(0) as u64,
                exact: false,
            });
        }
        let sql = format!("EXPLAIN ESTIMATE SELECT 1 FROM {table} WHERE {}", built.where_sql);
        let (columns, rows) = self.http().query(&sql, &params).await.ok()?;
        let index = columns.iter().position(|column| column.name == "rows")?;
        let value: i64 = rows.iter().filter_map(|row| row[index].display()?.parse::<i64>().ok()).sum();
        Some(Total {
            value: value.max(0) as u64,
            exact: false,
        })
    }
}
