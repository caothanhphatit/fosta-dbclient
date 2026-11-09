//! ClickHouse vector tooling: the [`VectorStore`] the shared `vector.*` operations (`crate::contract::vector`) run on.
//! Vector columns are `Array(Float32)`, `Array(Float64)` or `Array(BFloat16)`; their dimensions come from a
//! `vector_similarity` index, a `CHECK length(column) = n` constraint or, failing both, the first non-empty row.
//! Searches `ORDER BY` the distance function and `LIMIT k`, so a matching `vector_similarity('hnsw', …)` index is
//! used; exact top-k is the same query with `SETTINGS use_skip_indexes = 0`. Every statement runs with `readonly = 2`.

use std::time::Instant;

use async_trait::async_trait;

use super::{param_type, types, ChDialect, ChEngine};
use crate::{
    contract::{
        vector::{
            self as shared, parse_vector, vector_text, KeyCell, Metric, NearestRequest, Neighbour, RowSet, SampleOrder, SearchRequest,
            TableRef, VectorColumn, VectorIndex, VectorProfile, VectorSearchResult, VectorStore, KEY_SEPARATOR,
        },
        ColumnInfo, Value, ValueKind,
    },
    engines::common::sqlbuild::SqlDialect,
    error::{AppError, AppResult},
};

pub use shared::OPERATIONS;

static PROFILE: VectorProfile = VectorProfile {
    metrics: &[Metric::Cosine, Metric::Euclidean, Metric::Dot],
    schemas: false,
    index_usage: "Searches must ORDER BY the index's distance function (cosineDistance or L2Distance) on a constant vector and use LIMIT to use this index.",
    add_index: "Add one with: ALTER TABLE … ADD INDEX idx column TYPE vector_similarity('hnsw', 'cosineDistance', dims), then MATERIALIZE INDEX idx",
    euclidean_advice: "Normalize them or use a cosineDistance index.",
};

/// The element types of a vector column.
const ELEMENT_TYPES: [&str; 3] = ["Float32", "Float64", "BFloat16"];
/// Every vector statement runs read-only (2: settings such as `use_skip_indexes` may still change).
const READ_ONLY: (&str, &str) = ("readonly", "2");

impl ChEngine {
    /// Runs one `vector.*` operation.
    pub async fn vector(&self, name: &str, args: serde_json::Value) -> AppResult<serde_json::Value> {
        shared::invoke(&mut ChVectors { engine: self }, name, args).await
    }
}

pub struct ChVectors<'a> {
    engine: &'a ChEngine,
}

/// A table column: its name and type.
struct ChColumn {
    name: String,
    type_name: String,
}

impl ChColumn {
    fn is_vector(&self) -> bool {
        vector_element(&self.type_name).is_some()
    }
}

/// `Float32` for `Array(Float32)`; `None` for other types.
fn vector_element(type_name: &str) -> Option<&'static str> {
    let inner = type_name.trim().strip_prefix("Array(")?.strip_suffix(')')?.trim();
    ELEMENT_TYPES.into_iter().find(|element| *element == inner)
}

fn quote(name: &str) -> String {
    ChDialect.quote_ident(name)
}

fn target(table: &TableRef) -> String {
    format!("{}.{}", quote(&table.database), quote(&table.table))
}

/// The distance expression of a metric (ascending: nearest first; inner product is negated, as pgvector's `<#>`).
fn distance(metric: Metric, column: &str, query: &str) -> String {
    let column = quote(column);
    match metric {
        Metric::Euclidean => format!("L2Distance({column}, {query})"),
        Metric::Dot => format!("-dotProduct({column}, {query})"),
        Metric::Cosine | Metric::Auto => format!("cosineDistance({column}, {query})"),
    }
}

/// The metric of a `vector_similarity` distance function.
fn function_metric(function: &str) -> String {
    match function {
        "cosineDistance" => "cosine".to_owned(),
        "L2Distance" => "euclidean".to_owned(),
        other => other.to_owned(),
    }
}

/// The arguments of `vector_similarity('hnsw', 'cosineDistance', 4, 'bf16', 32, 128)`, unquoted.
fn index_args(type_full: &str) -> Vec<String> {
    let Some(inner) = type_full
        .trim()
        .strip_prefix("vector_similarity(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return Vec::new();
    };
    inner.split(',').map(|part| part.trim().trim_matches('\'').to_owned()).collect()
}

/// The dimensions of an index from its [`VectorIndex::params`] (`dims=4, …`).
fn index_dims(params: &str) -> Option<u32> {
    let rest = params.strip_prefix("dims=")?;
    rest.split(',').next()?.trim().parse().ok()
}

/// `n` from `CONSTRAINT … CHECK length(column) = n` in a `CREATE TABLE` statement.
fn constraint_dims(create: &str, column: &str) -> Option<u32> {
    let compact: String = create.chars().filter(|character| !character.is_whitespace()).collect();
    [format!("length({column})="), format!("length(`{column}`)=")]
        .iter()
        .find_map(|needle| {
            let rest = &compact[compact.find(needle.as_str())? + needle.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
}

/// The index of a plan: the `Name:` line before a `Description: vector_similarity` line of `EXPLAIN indexes = 1`.
fn index_in_plan(lines: &[String]) -> Option<String> {
    let position = lines
        .iter()
        .position(|line| line.trim().starts_with("Description: vector_similarity"))?;
    lines[..position]
        .iter()
        .rev()
        .find_map(|line| line.trim().strip_prefix("Name: ").map(str::to_owned))
}

/// A cell of `CAST(column, 'Array(Float64)')` as floats.
fn floats(value: &Value) -> Option<Vec<f32>> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Float(number) => Some(*number as f32),
                Value::Int(number) => Some(*number as f32),
                other => other.display()?.parse().ok(),
            })
            .collect(),
        Value::Text(text) => parse_vector(text).ok(),
        _ => None,
    }
}

impl ChVectors<'_> {
    async fn query(&self, sql: &str, params: &[(String, String)]) -> AppResult<(Vec<ColumnInfo>, Vec<Vec<Value>>)> {
        let mut params = params.to_vec();
        params.push((READ_ONLY.0.to_owned(), READ_ONLY.1.to_owned()));
        Ok(self.engine.http().query(sql, &params).await?)
    }

    async fn strings(&self, sql: &str, params: &[(&str, &str)]) -> AppResult<Vec<Vec<String>>> {
        let params: Vec<(String, String)> = params
            .iter()
            .map(|(name, value)| (format!("param_{name}"), (*value).to_owned()))
            .collect();
        let (_, rows) = self.query(sql, &params).await?;
        Ok(rows
            .into_iter()
            .map(|row| row.into_iter().map(|value| value.display().unwrap_or_default()).collect())
            .collect())
    }

    async fn table_columns(&self, table: &TableRef) -> AppResult<Vec<ChColumn>> {
        let rows = self
            .strings(
                "SELECT name, type FROM system.columns WHERE database = {db:String} AND `table` = {t:String} ORDER BY position",
                &[("db", &table.database), ("t", &table.table)],
            )
            .await?;
        if rows.is_empty() {
            return Err(AppError::new(format!("Table {}.{} not found", table.database, table.table)));
        }
        Ok(rows
            .into_iter()
            .map(|mut row| ChColumn {
                type_name: row.pop().unwrap_or_default(),
                name: row.pop().unwrap_or_default(),
            })
            .collect())
    }

    async fn create_statement(&self, table: &TableRef) -> AppResult<String> {
        let rows = self
            .strings(
                "SELECT create_table_query FROM system.tables WHERE database = {db:String} AND name = {t:String}",
                &[("db", &table.database), ("t", &table.table)],
            )
            .await?;
        Ok(rows.into_iter().next().and_then(|row| row.into_iter().next()).unwrap_or_default())
    }

    async fn vector_rows(&self, sql: &str) -> AppResult<Vec<Option<Vec<f32>>>> {
        let (_, rows) = self.query(sql, &[]).await?;
        Ok(rows.iter().map(|row| row.first().and_then(floats)).collect())
    }
}

#[async_trait]
impl VectorStore for ChVectors<'_> {
    fn profile(&self) -> &'static VectorProfile {
        &PROFILE
    }

    /// Distance functions exist on every supported server; `vector_similarity` indexes are optional.
    async fn supported(&mut self, _table: &TableRef) -> AppResult<bool> {
        Ok(true)
    }

    async fn columns(&mut self, table: &TableRef) -> AppResult<Vec<VectorColumn>> {
        let vectors: Vec<ChColumn> = self.table_columns(table).await?.into_iter().filter(ChColumn::is_vector).collect();
        if vectors.is_empty() {
            return Ok(Vec::new());
        }
        let indexes = self.indexes(table).await?;
        let create = self.create_statement(table).await?;
        let mut columns = Vec::new();
        for column in vectors {
            let indexed = indexes
                .iter()
                .find(|index| index.column == column.name)
                .and_then(|index| index_dims(&index.params));
            let dims = match indexed.or_else(|| constraint_dims(&create, &column.name)) {
                Some(dims) => Some(dims),
                None => {
                    let sql = format!(
                        "SELECT length({0}) FROM {1} WHERE notEmpty({0}) LIMIT 1",
                        quote(&column.name),
                        target(table)
                    );
                    let rows = self.strings(&sql, &[]).await?;
                    rows.first().and_then(|row| row.first()).and_then(|dims| dims.parse().ok())
                }
            };
            if let Some(dims) = dims {
                columns.push(VectorColumn {
                    name: column.name,
                    dims,
                    type_name: column.type_name,
                    // The distance function is the query's choice; an index serves its own.
                    metrics: Vec::new(),
                });
            }
        }
        Ok(columns)
    }

    async fn indexes(&mut self, table: &TableRef) -> AppResult<Vec<VectorIndex>> {
        let rows = self
            .strings(
                "SELECT name, expr, type_full FROM system.data_skipping_indices \
                 WHERE database = {db:String} AND `table` = {t:String} AND type = 'vector_similarity' ORDER BY name",
                &[("db", &table.database), ("t", &table.table)],
            )
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let [name, expr, type_full]: [String; 3] = row.try_into().ok()?;
                let args = index_args(&type_full);
                let options = args.get(3..).map(|rest| rest.join(", ")).unwrap_or_default();
                Some(VectorIndex {
                    name,
                    column: expr.trim().trim_matches('`').to_owned(),
                    metric: function_metric(args.get(1).map_or("", String::as_str)),
                    kind: args.first().cloned().unwrap_or_else(|| "vector_similarity".to_owned()),
                    m: args.get(4).and_then(|m| m.parse().ok()),
                    params: if options.is_empty() {
                        format!("dims={}", args.get(2).map_or("?", String::as_str))
                    } else {
                        format!("dims={}, {options}", args.get(2).map_or("?", String::as_str))
                    },
                })
            })
            .collect())
    }

    async fn primary_key(&mut self, table: &TableRef) -> AppResult<Vec<String>> {
        let rows = self
            .strings(
                "SELECT primary_key FROM system.tables WHERE database = {db:String} AND name = {t:String}",
                &[("db", &table.database), ("t", &table.table)],
            )
            .await?;
        let key = rows.into_iter().next().and_then(|row| row.into_iter().next()).unwrap_or_default();
        let columns = self.table_columns(table).await?;
        Ok(key
            .split(',')
            .map(|part| part.trim().trim_matches('`').to_owned())
            .filter(|part| columns.iter().any(|column| &column.name == part))
            .collect())
    }

    async fn fetch(&mut self, table: &TableRef, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
        if key.is_empty() {
            return Err(AppError::new("Cannot identify the row"));
        }
        let columns = self.table_columns(table).await?;
        if !columns.iter().any(|candidate| candidate.name == column && candidate.is_vector()) {
            return Err(AppError::new(format!("{column} is not a vector column")));
        }
        let mut params = Vec::new();
        let mut conditions = Vec::new();
        for (position, cell) in key.iter().enumerate() {
            let key_column = columns
                .iter()
                .find(|candidate| candidate.name == cell.column)
                .ok_or_else(|| AppError::new(format!("Unknown key column {}", cell.column)))?;
            if matches!(cell.value, Value::Null) {
                conditions.push(format!("isNull({})", quote(&cell.column)));
                continue;
            }
            conditions.push(format!(
                "{} = {{k{position}:{}}}",
                quote(&cell.column),
                param_type(&key_column.type_name)
            ));
            params.push((format!("param_k{position}"), types::to_param_text(&cell.value)));
        }
        let sql = format!(
            "SELECT CAST({}, 'Array(Float64)') FROM {} WHERE {} LIMIT 1",
            quote(column),
            target(table),
            conditions.join(" AND ")
        );
        let (_, rows) = self.query(&sql, &params).await?;
        match rows.first().map(|row| row.first().and_then(floats)) {
            Some(Some(values)) if !values.is_empty() => Ok(values),
            Some(_) => Err(AppError::new("This row has no vector (an empty array)")),
            None => Err(AppError::new("Row not found")),
        }
    }

    async fn count(&mut self, table: &TableRef, column: &VectorColumn) -> AppResult<(u64, u64)> {
        let sql = format!("SELECT count(), countIf(empty({})) FROM {}", quote(&column.name), target(table));
        let rows = self.strings(&sql, &[]).await?;
        let cell = |index: usize| {
            rows.first()
                .and_then(|row| row.get(index))
                .and_then(|text| text.parse().ok())
                .unwrap_or_default()
        };
        Ok((cell(0), cell(1)))
    }

    async fn sample(
        &mut self,
        table: &TableRef,
        column: &VectorColumn,
        limit: u32,
        order: SampleOrder<'_>,
    ) -> AppResult<Vec<Option<Vec<f32>>>> {
        let order = match order {
            SampleOrder::Any => String::new(),
            SampleOrder::Random => " ORDER BY rand()".to_owned(),
            SampleOrder::ByKey(key) => format!(" ORDER BY {}", key.iter().map(|name| quote(name)).collect::<Vec<_>>().join(", ")),
        };
        let vector = quote(&column.name);
        let sql = format!(
            "SELECT CAST({vector}, 'Array(Float64)') FROM {} WHERE notEmpty({vector}){order} LIMIT {limit}",
            target(table)
        );
        self.vector_rows(&sql).await
    }

    async fn search(&mut self, table: &TableRef, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult> {
        let columns = self.table_columns(table).await?;
        let select_list: Vec<String> = columns
            .iter()
            .filter(|column| !column.is_vector())
            .map(|column| quote(&column.name))
            .collect();
        let metric = request.metric.resolve(request.index, Metric::Cosine);
        let render = |query: &str| {
            let separator = if select_list.is_empty() { "" } else { ", " };
            format!(
                "SELECT {} AS vec_distance{separator}{} FROM {}{} ORDER BY vec_distance LIMIT {}",
                distance(metric, &request.column.name, query),
                select_list.join(", "),
                target(table),
                shared::where_clause(request.filter),
                request.k
            )
        };
        let runnable_sql = render(&vector_text(request.query));
        let primary_key = self.primary_key(table).await?;
        let plan: Vec<String> = self
            .strings(&format!("EXPLAIN indexes = 1 {runnable_sql}"), &[])
            .await?
            .into_iter()
            .filter_map(|row| row.into_iter().next())
            .collect();
        let started = Instant::now();
        let (result_columns, rows) = self.query(&runnable_sql, &[]).await?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(VectorSearchResult {
            index_used: index_in_plan(&plan),
            plan: RowSet {
                columns: vec![ColumnInfo::result("explain", "String", ValueKind::Text)],
                rows: plan.into_iter().map(|line| vec![Value::Text(line)]).collect(),
            },
            result: RowSet {
                columns: result_columns,
                rows,
            },
            sql: render("{query:Array(Float32)}"),
            runnable_sql,
            elapsed_ms,
            query_dims: request.query.len(),
            primary_key,
        })
    }

    async fn nearest(&mut self, table: &TableRef, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>> {
        let metric = Metric::of_index(request.index);
        let keys: Vec<String> = request.key.iter().map(|name| format!("toString({})", quote(name))).collect();
        let (order, settings) = if request.exact {
            let key_order = request.key.iter().map(|name| quote(name)).collect::<Vec<_>>().join(", ");
            (format!("vec_distance, {key_order}"), " SETTINGS use_skip_indexes = 0")
        } else {
            ("vec_distance".to_owned(), "")
        };
        let sql = format!(
            "SELECT toFloat64({}) AS vec_distance, {} FROM {} ORDER BY {order} LIMIT {}{settings}",
            distance(metric, &request.column.name, &vector_text(request.query)),
            keys.join(", "),
            target(table),
            request.k
        );
        let rows = self.strings(&sql, &[]).await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let mut cells = row.into_iter();
                let distance = cells.next().and_then(|text| text.parse().ok()).unwrap_or(f64::NAN);
                Neighbour {
                    key: cells.collect::<Vec<_>>().join(KEY_SEPARATOR),
                    distance,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vector_types_and_index_definitions_parse() {
        assert_eq!(vector_element("Array(Float32)"), Some("Float32"));
        assert_eq!(vector_element("Array(BFloat16)"), Some("BFloat16"));
        assert_eq!(vector_element("Array(String)"), None);
        assert_eq!(vector_element("Float32"), None);
        assert_eq!(
            index_args("vector_similarity('hnsw', 'cosineDistance', 4, 'bf16', 32, 128)"),
            ["hnsw", "cosineDistance", "4", "bf16", "32", "128"]
        );
        assert!(index_args("minmax").is_empty());
        assert_eq!(index_dims("dims=4, bf16, 32"), Some(4));
        assert_eq!(function_metric("L2Distance"), "euclidean");
        assert_eq!(function_metric("cosineDistance"), "cosine");
        let create =
            "CREATE TABLE t (`id` Int32, `embedding` Array(Float32), CONSTRAINT dims CHECK length(embedding) = 4) ENGINE = MergeTree";
        assert_eq!(constraint_dims(create, "embedding"), Some(4));
        assert_eq!(constraint_dims(create, "other"), None);
    }

    #[test]
    fn distances_plans_and_cells_render() {
        assert_eq!(distance(Metric::Cosine, "v", "[1,2]"), "cosineDistance(`v`, [1,2])");
        assert_eq!(distance(Metric::Euclidean, "v", "[1,2]"), "L2Distance(`v`, [1,2])");
        assert_eq!(distance(Metric::Dot, "v", "[1,2]"), "-dotProduct(`v`, [1,2])");
        let plan: Vec<String> = [
            "Indexes:",
            "  Skip",
            "    Name: embedding_idx",
            "    Description: vector_similarity GRANULARITY 100000000",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(index_in_plan(&plan).as_deref(), Some("embedding_idx"));
        assert_eq!(index_in_plan(&plan[..3]), None);
        assert_eq!(floats(&Value::Array(vec![Value::Float(0.5), Value::Int(1)])), Some(vec![0.5, 1.0]));
        assert_eq!(floats(&Value::Null), None);
    }
}
