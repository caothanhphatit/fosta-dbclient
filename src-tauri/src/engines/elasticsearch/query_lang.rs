//! Tabular query languages: ES|QL (`POST /_query`, Elasticsearch 8.11+) and SQL (`/_sql` on Elasticsearch,
//! `/_plugins/_sql` on OpenSearch) with cursor paging. Both return a [`TypedTable`].

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{client::EsRequest, connection::EsEngine};
use crate::error::{AppError, AppResult};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypedColumn {
    pub name: String,
    /// The engine's type name (`keyword`, `long`, `date`, `double`, `geo_point`...).
    pub column_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypedTable {
    pub columns: Vec<TypedColumn>,
    /// Row-major JSON values; dates are strings in the engine's format, multi-values are arrays.
    pub rows: Vec<Vec<Value>>,
    /// SQL: pass to [`EsEngine::sql_next`] for the next page; `None` when there is no more.
    pub cursor: Option<String>,
    pub took_ms: Option<u64>,
    /// ES|QL: warnings from the `Warning` headers (truncated results, type conflicts...).
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EsqlRequest {
    pub query: String,
    /// Query DSL filter applied before the ES|QL pipeline (e.g. a time range).
    pub filter: Option<Value>,
    /// Positional or named parameters (`?` / `?name`).
    pub params: Option<Value>,
    pub locale: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SqlRequest {
    pub query: String,
    /// Rows per page (default 1000).
    pub fetch_size: Option<u32>,
    pub filter: Option<Value>,
    pub params: Option<Vec<Value>>,
    pub time_zone: Option<String>,
    pub timeout_ms: Option<u64>,
}

/// Parses a row-wise ES|QL response (`columns` + `values`).
pub fn parse_esql(body: &Value) -> TypedTable {
    let columns: Vec<TypedColumn> = body["columns"]
        .as_array()
        .map(|columns| {
            columns
                .iter()
                .map(|column| TypedColumn {
                    name: column["name"].as_str().unwrap_or_default().to_string(),
                    column_type: column["type"].as_str().unwrap_or("unknown").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    // Requests are sent row-wise (`columnar: false`, the default).
    let rows = body["values"]
        .as_array()
        .map(|rows| rows.iter().map(|row| row.as_array().cloned().unwrap_or_default()).collect())
        .unwrap_or_default();
    TypedTable {
        columns,
        rows,
        cursor: None,
        took_ms: body["took"].as_u64(),
        warnings: Vec::new(),
    }
}

/// Parses an Elasticsearch SQL (`columns` + `rows`) or OpenSearch SQL JDBC (`schema` + `datarows`) response.
pub fn parse_sql(body: &Value) -> TypedTable {
    let (columns_key, type_key, rows_key) = if body.get("schema").is_some() {
        ("schema", "type", "datarows")
    } else {
        ("columns", "type", "rows")
    };
    let columns: Vec<TypedColumn> = body[columns_key]
        .as_array()
        .map(|columns| {
            columns
                .iter()
                .map(|column| TypedColumn {
                    name: column["alias"].as_str().or(column["name"].as_str()).unwrap_or_default().to_string(),
                    column_type: column[type_key].as_str().unwrap_or("unknown").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let rows = body[rows_key]
        .as_array()
        .map(|rows| rows.iter().map(|row| row.as_array().cloned().unwrap_or_default()).collect())
        .unwrap_or_default();
    TypedTable {
        columns,
        rows,
        cursor: body["cursor"].as_str().filter(|cursor| !cursor.is_empty()).map(str::to_string),
        took_ms: None,
        warnings: Vec::new(),
    }
}

impl EsEngine {
    pub async fn esql(&self, request: &EsqlRequest) -> AppResult<TypedTable> {
        if !self.features().esql {
            return Err(AppError::new(
                "ES|QL needs Elasticsearch 8.11 or newer (OpenSearch has PPL and SQL instead).",
            ));
        }
        let mut body = json!({ "query": request.query });
        if let Some(filter) = &request.filter {
            body["filter"] = filter.clone();
        }
        if let Some(params) = &request.params {
            body["params"] = params.clone();
        }
        if let Some(locale) = &request.locale {
            body["locale"] = json!(locale);
        }
        let mut http = EsRequest::new(Method::POST, "/_query?format=json").json(body);
        if let Some(timeout) = request.timeout_ms {
            http = http.timeout(std::time::Duration::from_millis(timeout));
        }
        let response = self.client.send(&http).await?;
        if response.status >= 400 {
            return Err(AppError::new(super::client::error_message(response.status, &response.body)));
        }
        let mut table = parse_esql(&response.body);
        table.warnings = response.warnings;
        Ok(table)
    }

    fn sql_path(&self) -> &'static str {
        if self.info.is_opensearch() {
            "/_plugins/_sql?format=jdbc"
        } else {
            "/_sql?format=json"
        }
    }

    /// Runs a SQL query and returns the first page; follow `cursor` with [`Self::sql_next`].
    pub async fn sql(&self, request: &SqlRequest) -> AppResult<TypedTable> {
        if !self.features().sql {
            return Err(AppError::new("This cluster has no SQL API."));
        }
        let mut body = json!({ "query": request.query, "fetch_size": request.fetch_size.unwrap_or(1000) });
        if let Some(filter) = &request.filter {
            body["filter"] = filter.clone();
        }
        if let Some(params) = &request.params {
            body["params"] = json!(params);
        }
        if let Some(time_zone) = &request.time_zone {
            body["time_zone"] = json!(time_zone);
        }
        let mut http = EsRequest::new(Method::POST, self.sql_path()).json(body);
        if let Some(timeout) = request.timeout_ms {
            http = http.timeout(std::time::Duration::from_millis(timeout));
        }
        Ok(parse_sql(&self.client.call(&http).await?))
    }

    /// The next SQL page. Elasticsearch omits the columns on cursor pages, so pass the first page's columns.
    pub async fn sql_next(&self, cursor: &str, columns: &[TypedColumn]) -> AppResult<TypedTable> {
        let response = self.client.post(self.sql_path(), json!({ "cursor": cursor })).await?;
        let mut table = parse_sql(&response);
        if table.columns.is_empty() {
            table.columns = columns.to_vec();
        }
        Ok(table)
    }

    /// Frees a SQL cursor before reading it to the end.
    pub async fn sql_close(&self, cursor: &str) -> AppResult<()> {
        let path = if self.info.is_opensearch() {
            "/_plugins/_sql/close"
        } else {
            "/_sql/close"
        };
        self.client.post(path, json!({ "cursor": cursor })).await.map(|_| ())
    }

    /// The Query DSL a SQL statement turns into (Elasticsearch `_sql/translate`, OpenSearch `_plugins/_sql/_explain`).
    pub async fn sql_translate(&self, query: &str) -> AppResult<Value> {
        let path = if self.info.is_opensearch() {
            "/_plugins/_sql/_explain"
        } else {
            "/_sql/translate"
        };
        self.client.post(path, json!({ "query": query })).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_esql_tables() {
        let rows = json!({
            "took": 4,
            "columns": [{ "name": "brand", "type": "keyword" }, { "name": "c", "type": "long" }],
            "values": [["acme", 5], ["globex", 2]]
        });
        let table = parse_esql(&rows);
        assert_eq!(
            table.columns[1],
            TypedColumn {
                name: "c".into(),
                column_type: "long".into()
            }
        );
        assert_eq!(table.rows, vec![vec![json!("acme"), json!(5)], vec![json!("globex"), json!(2)]]);
    }

    #[test]
    fn parses_sql_tables_from_both_distributions() {
        let es = json!({ "columns": [{ "name": "a", "type": "text" }], "rows": [["x"]], "cursor": "c1" });
        let table = parse_sql(&es);
        assert_eq!(
            (table.columns[0].column_type.as_str(), table.cursor.as_deref()),
            ("text", Some("c1"))
        );
        let os = json!({ "schema": [{ "name": "a", "alias": "A", "type": "keyword" }], "datarows": [["x"], ["y"]], "total": 2, "size": 2, "status": 200 });
        let table = parse_sql(&os);
        assert_eq!((table.columns[0].name.as_str(), table.rows.len(), table.cursor), ("A", 2, None));
    }
}
