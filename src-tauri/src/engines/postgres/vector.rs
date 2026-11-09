//! pgvector tooling: the [`VectorStore`] the shared `vector.*` operations (`crate::contract::vector`) run on, for
//! databases with the `vector` extension. Columns of type `vector` and `halfvec` (dimensions from the type modifier),
//! HNSW and IVFFlat indexes (metric from the operator class), searches ordered by the metric's operator so the index is
//! used. Every operation runs in a read-only transaction; exact top-k turns index scans off inside it. Vectors travel
//! as text (`'[1,2,3]'::vector`).

use std::time::Instant;

use async_trait::async_trait;
use tokio_postgres::{Client, Row, Transaction};

use super::{catalog::TEXT_CAST, map_err, types, PgDialect, PgEngine};
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

static PROFILE: VectorProfile = VectorProfile {
    metrics: &[Metric::Cosine, Metric::Euclidean, Metric::Dot],
    schemas: true,
    index_usage: "Searches must ORDER BY the operator of the index's operator class (<=> cosine, <-> L2, <#> inner product) and use LIMIT to use this index.",
    add_index: "Add one with: CREATE INDEX ON … USING hnsw (column vector_cosine_ops)",
    euclidean_advice: "Normalize them or use a vector_cosine_ops index.",
};

/// The vector types this module reads (`sparsevec` has no dense form).
const VECTOR_TYPES: [&str; 2] = ["vector", "halfvec"];
/// pgvector's default `hnsw.ef_search`: an HNSW scan returns at most this many rows unless raised.
const DEFAULT_EF_SEARCH: u32 = 40;

impl PgEngine {
    /// Whether the `vector` extension is installed in this database.
    pub(super) async fn has_vector_extension(&self) -> AppResult<bool> {
        has_extension(&*self.client.lock().await).await
    }

    /// Runs one `vector.*` operation on this database (holding its connection for the operation).
    pub(super) async fn vector(&self, name: &str, args: serde_json::Value) -> AppResult<serde_json::Value> {
        let mut client = self.client.lock().await;
        shared::invoke(&mut PgVectors { client: &mut client }, name, args).await
    }
}

async fn has_extension(client: &Client) -> AppResult<bool> {
    let row = client
        .query_one("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector')", &[])
        .await
        .map_err(map_err)?;
    Ok(row.get(0))
}

pub struct PgVectors<'a> {
    client: &'a mut Client,
}

/// A table column as the vector tooling needs it.
struct PgColumn {
    name: String,
    /// The base type name (`vector`, `int4`, …).
    base: String,
    /// `format_type`: what a value is cast to.
    type_name: String,
    dims: Option<u32>,
}

impl PgColumn {
    fn is_vector(&self) -> bool {
        VECTOR_TYPES.contains(&self.base.as_str())
    }
}

fn quote(name: &str) -> String {
    PgDialect.quote_ident(name)
}

fn target(table: &TableRef) -> String {
    match &table.schema {
        Some(schema) => format!("{}.{}", quote(schema), quote(&table.table)),
        None => quote(&table.table),
    }
}

fn key_list(key: &[String], suffix: &str) -> String {
    key.iter()
        .map(|name| format!("{}{suffix}", quote(name)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The distance operator of a metric.
fn operator(metric: Metric) -> &'static str {
    match metric {
        Metric::Euclidean => "<->",
        Metric::Dot => "<#>",
        Metric::Cosine | Metric::Auto => "<=>",
    }
}

/// The operator an index orders by (its operator class's).
fn index_operator(index: &VectorIndex) -> &'static str {
    match index.metric.as_str() {
        "l1" => "<+>",
        _ => operator(Metric::of_index(index)),
    }
}

/// The metric of an operator class (`vector_cosine_ops` → `cosine`).
fn opclass_metric(opclass: &str) -> String {
    let metric = opclass.split('_').find_map(|part| match part {
        "cosine" => Some("cosine"),
        "l2" => Some("euclidean"),
        "ip" => Some("dot"),
        "l1" => Some("l1"),
        "hamming" => Some("hamming"),
        "jaccard" => Some("jaccard"),
        _ => None,
    });
    metric.unwrap_or(opclass).to_owned()
}

/// The `Index Scan using <name> on …` of a text plan.
fn index_in_plan(lines: &[String]) -> Option<String> {
    lines.iter().find_map(|line| {
        let (_, rest) = line.split_once("Index Scan using ")?;
        Some(rest.split(" on ").next().unwrap_or(rest).trim().to_owned())
    })
}

/// `SET LOCAL` for an HNSW scan returning `k` rows (pgvector caps it at `hnsw.ef_search`).
fn ef_search(index: Option<&VectorIndex>, k: u32) -> String {
    match index {
        Some(index) if index.kind == "hnsw" => format!("SET LOCAL hnsw.ef_search = {};", k.max(DEFAULT_EF_SEARCH)),
        _ => String::new(),
    }
}

async fn read_only(client: &mut Client) -> AppResult<Transaction<'_>> {
    Ok(client.build_transaction().read_only(true).start().await.map_err(map_err)?)
}

impl PgVectors<'_> {
    async fn table_columns(&mut self, table: &TableRef) -> AppResult<Vec<PgColumn>> {
        let rows = self
            .client
            .query(
                "SELECT a.attname::text, t.typname::text, format_type(a.atttypid, a.atttypmod), a.atttypmod \
                 FROM pg_attribute a \
                 JOIN pg_class c ON c.oid = a.attrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_type t ON t.oid = a.atttypid \
                 WHERE n.nspname = COALESCE($1::text, current_schema()) AND c.relname = $2 AND a.attnum > 0 AND NOT a.attisdropped \
                 ORDER BY a.attnum",
                &[&table.schema, &table.table],
            )
            .await
            .map_err(map_err)?;
        if rows.is_empty() {
            return Err(AppError::new(format!("Table {} not found", table.table)));
        }
        let mut columns: Vec<PgColumn> = rows
            .iter()
            .map(|row| {
                let typmod: i32 = row.get(3);
                PgColumn {
                    name: row.get(0),
                    base: row.get(1),
                    type_name: row.get(2),
                    dims: u32::try_from(typmod).ok().filter(|dims| *dims > 0),
                }
            })
            .collect();
        // A column declared without dimensions (`vector`) takes them from a stored value.
        for column in columns.iter_mut().filter(|column| column.is_vector() && column.dims.is_none()) {
            let sql = format!(
                "SELECT vector_dims({0}) FROM {1} WHERE {0} IS NOT NULL LIMIT 1",
                quote(&column.name),
                target(table)
            );
            let row = self.client.query_opt(&sql, &[]).await.map_err(map_err)?;
            column.dims = row.and_then(|row| u32::try_from(row.get::<_, i32>(0)).ok());
        }
        Ok(columns)
    }

    async fn vector_column(&mut self, table: &TableRef, name: &str) -> AppResult<PgColumn> {
        self.table_columns(table)
            .await?
            .into_iter()
            .find(|column| column.name == name && column.is_vector())
            .ok_or_else(|| AppError::new(format!("{name} is not a vector column")))
    }
}

#[async_trait]
impl VectorStore for PgVectors<'_> {
    fn profile(&self) -> &'static VectorProfile {
        &PROFILE
    }

    async fn supported(&mut self, _table: &TableRef) -> AppResult<bool> {
        has_extension(self.client).await
    }

    async fn columns(&mut self, table: &TableRef) -> AppResult<Vec<VectorColumn>> {
        Ok(self
            .table_columns(table)
            .await?
            .into_iter()
            .filter(PgColumn::is_vector)
            .filter_map(|column| {
                Some(VectorColumn {
                    dims: column.dims?,
                    name: column.name,
                    type_name: column.type_name,
                    metrics: Vec::new(),
                })
            })
            .collect())
    }

    async fn indexes(&mut self, table: &TableRef) -> AppResult<Vec<VectorIndex>> {
        let rows = self
            .client
            .query(
                "SELECT i.relname::text, a.attname::text, am.amname::text, opc.opcname::text, \
                        COALESCE(array_to_string(i.reloptions, ', '), '') \
                 FROM pg_index x \
                 JOIN pg_class i ON i.oid = x.indexrelid \
                 JOIN pg_class c ON c.oid = x.indrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_am am ON am.oid = i.relam \
                 JOIN pg_attribute a ON a.attrelid = x.indrelid AND a.attnum = x.indkey[0] \
                 JOIN pg_opclass opc ON opc.oid = x.indclass[0] \
                 WHERE n.nspname = COALESCE($1::text, current_schema()) AND c.relname = $2 AND am.amname IN ('hnsw', 'ivfflat') \
                 ORDER BY i.relname",
                &[&table.schema, &table.table],
            )
            .await
            .map_err(map_err)?;
        Ok(rows
            .iter()
            .map(|row| {
                let options: String = row.get(4);
                let m = options
                    .split(", ")
                    .find_map(|option| option.strip_prefix("m="))
                    .and_then(|value| value.parse().ok());
                VectorIndex {
                    name: row.get(0),
                    column: row.get(1),
                    kind: row.get(2),
                    metric: opclass_metric(row.get(3)),
                    m,
                    params: if options.is_empty() { "defaults".to_owned() } else { options },
                }
            })
            .collect())
    }

    async fn primary_key(&mut self, table: &TableRef) -> AppResult<Vec<String>> {
        let rows = self
            .client
            .query(
                "SELECT a.attname::text FROM pg_index x \
                 JOIN pg_class c ON c.oid = x.indrelid \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY (x.indkey) \
                 WHERE x.indisprimary AND n.nspname = COALESCE($1::text, current_schema()) AND c.relname = $2 \
                 ORDER BY array_position(x.indkey::int2[], a.attnum)",
                &[&table.schema, &table.table],
            )
            .await
            .map_err(map_err)?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    async fn fetch(&mut self, table: &TableRef, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
        if key.is_empty() {
            return Err(AppError::new("Cannot identify the row"));
        }
        let columns = self.table_columns(table).await?;
        let vector = columns
            .iter()
            .find(|candidate| candidate.name == column && candidate.is_vector())
            .ok_or_else(|| AppError::new(format!("{column} is not a vector column")))?;
        let mut params: Vec<Option<String>> = Vec::new();
        let mut conditions = Vec::new();
        for cell in key {
            let key_column = columns
                .iter()
                .find(|candidate| candidate.name == cell.column)
                .ok_or_else(|| AppError::new(format!("Unknown key column {}", cell.column)))?;
            match types::to_param_text(&cell.value) {
                None => conditions.push(format!("{} IS NULL", quote(&cell.column))),
                Some(text) => {
                    params.push(Some(text));
                    conditions.push(format!(
                        "{} = (${}::text)::{}",
                        quote(&cell.column),
                        params.len(),
                        key_column.type_name
                    ));
                }
            }
        }
        let sql = format!(
            "SELECT {}::text FROM {} WHERE {} LIMIT 1",
            quote(&vector.name),
            target(table),
            conditions.join(" AND ")
        );
        let row = self.client.query_opt(&sql, &super::as_sql(&params)).await.map_err(map_err)?;
        match row.map(|row| row.get::<_, Option<String>>(0)) {
            Some(Some(text)) => parse_vector(&text),
            Some(None) => Err(AppError::new("This row has no vector (NULL)")),
            None => Err(AppError::new("Row not found")),
        }
    }

    async fn count(&mut self, table: &TableRef, column: &VectorColumn) -> AppResult<(u64, u64)> {
        let sql = format!(
            "SELECT count(*), count(*) FILTER (WHERE {} IS NULL) FROM {}",
            quote(&column.name),
            target(table)
        );
        let row = self.client.query_one(&sql, &[]).await.map_err(map_err)?;
        let count = |index: usize| u64::try_from(row.get::<_, i64>(index)).unwrap_or_default();
        Ok((count(0), count(1)))
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
            SampleOrder::Random => " ORDER BY random()".to_owned(),
            SampleOrder::ByKey(key) => format!(" ORDER BY {}", key_list(key, "")),
        };
        let vector = quote(&column.name);
        let sql = format!(
            "SELECT {vector}::text FROM {} WHERE {vector} IS NOT NULL{order} LIMIT {limit}",
            target(table)
        );
        let rows = self.client.query(&sql, &[]).await.map_err(map_err)?;
        Ok(rows
            .iter()
            .map(|row| row.get::<_, Option<String>>(0).and_then(|text| parse_vector(&text).ok()))
            .collect())
    }

    async fn search(&mut self, table: &TableRef, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult> {
        let columns = self.table_columns(table).await?;
        let vector = columns
            .iter()
            .find(|column| column.name == request.column.name)
            .ok_or_else(|| AppError::new(format!("{} is not a vector column", request.column.name)))?;
        let select_list: Vec<String> = columns
            .iter()
            .filter(|column| !column.is_vector() && column.base != "sparsevec")
            .map(|column| {
                let name = quote(&column.name);
                if TEXT_CAST.contains(&column.base.as_str()) {
                    format!("{name}::text AS {name}")
                } else {
                    name
                }
            })
            .collect();
        let operator = match (request.metric, request.index) {
            (Metric::Auto, Some(index)) => index_operator(index),
            (metric, _) => operator(metric),
        };
        let render = |query: &str| {
            let distance = format!("{} {operator} {query}", quote(&vector.name));
            let separator = if select_list.is_empty() { "" } else { ", " };
            format!(
                "SELECT {distance} AS vec_distance{separator}{} FROM {}{} ORDER BY {distance} LIMIT {}",
                select_list.join(", "),
                target(table),
                shared::where_clause(request.filter),
                request.k
            )
        };
        let runnable_sql = render(&format!("'{}'::{}", vector_text(request.query), vector.base));
        let primary_key = self.primary_key(table).await?;
        let tx = read_only(self.client).await?;
        let settings = ef_search(request.index, request.k);
        if !settings.is_empty() {
            tx.batch_execute(&settings).await.map_err(map_err)?;
        }
        let plan: Vec<String> = tx
            .query(&format!("EXPLAIN {runnable_sql}"), &[])
            .await
            .map_err(map_err)?
            .iter()
            .map(|row| row.get(0))
            .collect();
        let statement = tx.prepare(&runnable_sql).await.map_err(map_err)?;
        let started = Instant::now();
        let rows = tx.query(&statement, &[]).await.map_err(map_err)?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        tx.rollback().await.map_err(map_err)?;
        let result = RowSet {
            columns: statement
                .columns()
                .iter()
                .map(|column| ColumnInfo::result(column.name(), column.type_().name(), types::kind_of(column.type_())))
                .collect(),
            rows: rows.iter().map(types::row_values).collect(),
        };
        Ok(VectorSearchResult {
            index_used: index_in_plan(&plan),
            plan: RowSet {
                columns: vec![ColumnInfo::result("QUERY PLAN", "text", ValueKind::Text)],
                rows: plan.into_iter().map(|line| vec![Value::Text(line)]).collect(),
            },
            result,
            sql: render(&format!("$1::{}", vector.base)),
            runnable_sql,
            elapsed_ms,
            query_dims: request.query.len(),
            primary_key,
        })
    }

    async fn nearest(&mut self, table: &TableRef, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>> {
        let base = if request.column.type_name.starts_with("halfvec") {
            "halfvec"
        } else {
            "vector"
        };
        let distance = format!(
            "{} {} '{}'::{base}",
            quote(&request.column.name),
            index_operator(request.index),
            vector_text(request.query)
        );
        let (settings, order) = if request.exact {
            (
                "SET LOCAL enable_indexscan = off;".to_owned(),
                format!("{distance}, {}", key_list(request.key, "")),
            )
        } else {
            (
                format!("SET LOCAL enable_seqscan = off; {}", ef_search(Some(request.index), request.k)),
                distance.clone(),
            )
        };
        let sql = format!(
            "SELECT ({distance})::float8, {} FROM {} ORDER BY {order} LIMIT {}",
            key_list(request.key, "::text"),
            target(table),
            request.k
        );
        let tx = read_only(self.client).await?;
        tx.batch_execute(&settings).await.map_err(map_err)?;
        let rows = tx.query(&sql, &[]).await.map_err(map_err)?;
        tx.rollback().await.map_err(map_err)?;
        Ok(rows.iter().map(neighbour).collect())
    }
}

fn neighbour(row: &Row) -> Neighbour {
    let key = (1..row.len())
        .map(|index| row.get::<_, Option<String>>(index).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(KEY_SEPARATOR);
    Neighbour {
        key,
        distance: row.get::<_, Option<f64>>(0).unwrap_or(f64::NAN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(metric: &str, kind: &str) -> VectorIndex {
        VectorIndex {
            name: "i".into(),
            column: "v".into(),
            metric: metric.into(),
            kind: kind.into(),
            m: None,
            params: String::new(),
        }
    }

    #[test]
    fn operator_classes_map_to_metrics_and_operators() {
        assert_eq!(opclass_metric("vector_cosine_ops"), "cosine");
        assert_eq!(opclass_metric("halfvec_l2_ops"), "euclidean");
        assert_eq!(opclass_metric("vector_ip_ops"), "dot");
        assert_eq!(opclass_metric("bit_hamming_ops"), "hamming");
        assert_eq!(opclass_metric("custom"), "custom");
        assert_eq!(index_operator(&index("cosine", "hnsw")), "<=>");
        assert_eq!(index_operator(&index("euclidean", "ivfflat")), "<->");
        assert_eq!(index_operator(&index("dot", "hnsw")), "<#>");
        assert_eq!(index_operator(&index("l1", "hnsw")), "<+>");
        assert_eq!(operator(Metric::Auto), "<=>");
    }

    #[test]
    fn plans_settings_and_names_render() {
        let plan = vec![
            "Limit  (cost=1.00..2.00 rows=5 width=12)".to_owned(),
            "  ->  Index Scan using products_embedding_idx on products  (cost=1.00..2.00 rows=500 width=12)".to_owned(),
        ];
        assert_eq!(index_in_plan(&plan).as_deref(), Some("products_embedding_idx"));
        assert_eq!(index_in_plan(&plan[..1]), None);
        assert_eq!(ef_search(Some(&index("cosine", "hnsw")), 100), "SET LOCAL hnsw.ef_search = 100;");
        assert_eq!(ef_search(Some(&index("cosine", "hnsw")), 5), "SET LOCAL hnsw.ef_search = 40;");
        assert_eq!(ef_search(Some(&index("cosine", "ivfflat")), 100), "");
        let table = TableRef::parse("shop", "public.products", true);
        assert_eq!(target(&table), "\"public\".\"products\"");
        assert_eq!(target(&TableRef::parse("shop", "products", true)), "\"products\"");
        assert_eq!(key_list(&["id".into(), "line".into()], "::text"), "\"id\"::text, \"line\"::text");
    }
}
