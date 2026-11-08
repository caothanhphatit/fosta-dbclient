//! The engine operations the ClickHouse views call through `invoke`: parts, mutations, running queries, the query log and
//! KILL. Every result is `{ "columns": [names], "rows": [[text or null]], "note": text? }`, so one grid renders them all.
//! Arguments are bound as query parameters, never spliced into the statement.

use serde_json::{json, Value as Json};

use super::ChEngine;
use crate::{
    engines::common::{ColumnInfo, Value},
    error::{AppError, AppResult},
};

/// The `invoke` names of this module.
pub const OPERATIONS: [&str; 5] = [
    "clickhouse.parts",
    "clickhouse.mutations",
    "clickhouse.processes",
    "clickhouse.queryLog",
    "clickhouse.kill",
];

fn table_json(columns: &[ColumnInfo], rows: Vec<Vec<Value>>, note: Option<&str>) -> Json {
    json!({
        "columns": columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>(),
        "rows": rows.iter().map(|row| row.iter().map(|value| value.display().map(Json::from).unwrap_or(Json::Null)).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "note": note,
    })
}

fn text_arg(args: &Json, name: &str) -> Option<String> {
    args.get(name)
        .and_then(Json::as_str)
        .map(str::to_owned)
        .filter(|text| !text.is_empty())
}

fn limit_arg(args: &Json, default: u64, max: u64) -> u64 {
    args.get("limit").and_then(Json::as_u64).unwrap_or(default).clamp(1, max)
}

impl ChEngine {
    pub async fn invoke_operation(&self, name: &str, args: &Json) -> AppResult<Json> {
        match name {
            "clickhouse.parts" => {
                let (database, table) = table_args(args)?;
                self.table_query(
                    "SELECT name AS part, partition, rows, bytes_on_disk, modification_time, level, part_type, active \
                     FROM system.parts WHERE database = {db:String} AND `table` = {t:String} \
                     ORDER BY active DESC, modification_time DESC LIMIT 500",
                    &[("db", &database), ("t", &table)],
                    None,
                )
                .await
            }
            "clickhouse.mutations" => {
                let (database, table) = table_args(args)?;
                self.table_query(
                    "SELECT mutation_id, command, create_time, parts_to_do, is_done, latest_fail_reason, latest_fail_time \
                     FROM system.mutations WHERE database = {db:String} AND `table` = {t:String} \
                     ORDER BY create_time DESC LIMIT 200",
                    &[("db", &database), ("t", &table)],
                    None,
                )
                .await
            }
            "clickhouse.processes" => {
                self.table_query(
                    "SELECT query_id, user, round(elapsed, 1) AS elapsed_s, read_rows, formatReadableSize(memory_usage) AS memory, \
                     substring(query, 1, 300) AS query FROM system.processes WHERE query_id != currentQueryID() \
                     ORDER BY elapsed DESC",
                    &[],
                    None,
                )
                .await
            }
            "clickhouse.queryLog" => {
                let limit = limit_arg(args, 100, 500).to_string();
                let failed_only = args.get("failedOnly").and_then(Json::as_bool).unwrap_or(false);
                let filter = if failed_only {
                    "type IN ('ExceptionBeforeStart', 'ExceptionWhileProcessing')"
                } else {
                    "type != 'QueryStart'"
                };
                let sql = format!(
                    "SELECT event_time, type, query_duration_ms, formatReadableSize(memory_usage) AS memory, read_rows, written_rows, \
                     user, substring(query, 1, 400) AS query, exception_code, query_id \
                     FROM system.query_log WHERE {filter} AND event_date >= today() - 1 AND query_kind != 'SystemFlushLogs' \
                     AND query NOT LIKE '%FROM system.query_log%' ORDER BY event_time DESC LIMIT {{n:UInt32}}"
                );
                match self.table_query(&sql, &[("n", &limit)], None).await {
                    Err(error) if error.to_string().contains("UNKNOWN_TABLE") || error.to_string().contains("(60)") => Ok(json!({
                        "columns": [],
                        "rows": [],
                        "note": "system.query_log does not exist on this server. Enable the query_log section of the server configuration to record queries.",
                    })),
                    other => other.map(|mut table| {
                        if table["rows"].as_array().is_some_and(Vec::is_empty) {
                            table["note"] = json!("No finished queries in the last day yet. The server writes the log every few seconds.");
                        }
                        table
                    }),
                }
            }
            "clickhouse.kill" => {
                if let Some(mutation) = text_arg(args, "mutationId") {
                    let (database, table) = table_args(args)?;
                    self.http()
                        .execute(
                            "KILL MUTATION WHERE database = {db:String} AND `table` = {t:String} AND mutation_id = {m:String}",
                            &[
                                ("param_db".into(), database),
                                ("param_t".into(), table),
                                ("param_m".into(), mutation),
                            ],
                        )
                        .await?;
                } else {
                    let query_id = text_arg(args, "queryId").ok_or_else(|| AppError::new("Say which query to stop (queryId)."))?;
                    self.http().kill(&query_id, false).await?;
                }
                Ok(json!({ "ok": true }))
            }
            other => Err(crate::contract::unsupported_capability(other)),
        }
    }

    async fn table_query(&self, sql: &str, params: &[(&str, &str)], note: Option<&str>) -> AppResult<Json> {
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(name, value)| (format!("param_{name}"), (*value).to_owned()))
            .collect();
        let (columns, rows) = self.http().query(sql, &params).await?;
        Ok(table_json(&columns, rows, note))
    }
}

fn table_args(args: &Json) -> AppResult<(String, String)> {
    match (text_arg(args, "database"), text_arg(args, "table")) {
        (Some(database), Some(table)) => Ok((database, table)),
        _ => Err(AppError::new("Say which table (database and table).")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_text_grids() {
        let columns = vec![ColumnInfo::result("n", "UInt64", crate::engines::common::ValueKind::Int)];
        let table = table_json(&columns, vec![vec![Value::Int(3)], vec![Value::Null]], Some("x"));
        assert_eq!(table["columns"][0], "n");
        assert_eq!(table["rows"][0][0], "3");
        assert!(table["rows"][1][0].is_null());
        assert_eq!(table["note"], "x");
    }

    #[test]
    fn arguments_are_checked() {
        assert!(table_args(&json!({ "database": "d" })).is_err());
        assert_eq!(
            table_args(&json!({ "database": "d", "table": "t" })).unwrap(),
            ("d".into(), "t".into())
        );
        assert_eq!(limit_arg(&json!({ "limit": 99999 }), 100, 500), 500);
        assert_eq!(limit_arg(&json!({}), 100, 500), 100);
    }
}
