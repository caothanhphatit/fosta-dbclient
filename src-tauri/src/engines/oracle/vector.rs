//! Oracle AI Vector Search (23ai and newer): the [`VectorStore`] the shared `vector.*` operations
//! (`crate::contract::vector`) run on. Columns of type `VECTOR` (dimensions and format from `ALL_TAB_COLUMNS.VECTOR_INFO`,
//! or from a stored value for `VECTOR(*)`), vector indexes (IVF and HNSW, from `ALL_INDEXES`), searches ordered by
//! `VECTOR_DISTANCE` with `FETCH APPROX FIRST k ROWS ONLY` so an index is used, and exact top-k with `FETCH EXACT`.
//!
//! The `oracle` crate has no VECTOR type, so vectors travel as text: `FROM_VECTOR` out, `TO_VECTOR(:q)` in (bound as a
//! CLOB, which takes any number of dimensions). Searches and exact neighbours run in a read-only transaction; the plan is
//! explained (into PLAN_TABLE, rolled back) before it.
//!
//! An index's distance metric and build options live in `VECSYS.VECTOR$INDEX`, which ordinary users cannot read. Without
//! it the metric is found from the plan: the metric whose `FETCH APPROX` search uses the index.

use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use oracle::{
    sql_type::{OracleType, ToSql},
    Connection,
};

use super::{map_err, strings, tabular::app, types, OraDialect, OraEngine};
use crate::{
    contract::{
        vector::{
            self as shared, vector_text, KeyCell, Metric, NearestRequest, Neighbour, RowSet, SampleOrder, SearchRequest, TableRef,
            VectorColumn, VectorIndex, VectorProfile, VectorSearchResult, VectorStore, KEY_SEPARATOR,
        },
        ColumnInfo, Value, ValueKind,
    },
    engines::common::sqlbuild::SqlDialect,
    error::{AppError, AppResult},
};

static PROFILE: VectorProfile = VectorProfile {
    metrics: &[Metric::Cosine, Metric::Euclidean, Metric::Dot],
    // Oracle schemas are the "databases" of the shared UI.
    schemas: false,
    index_usage:
        "Searches must ORDER BY VECTOR_DISTANCE with the index's metric and end with FETCH APPROX FIRST k ROWS ONLY to use this index.",
    add_index: "Add one with: CREATE VECTOR INDEX … ON … (column) ORGANIZATION NEIGHBOR PARTITIONS DISTANCE COSINE",
    euclidean_advice: "Normalize them or use a DISTANCE COSINE index.",
};

/// Vectors up to this many dimensions are read as VARCHAR2(4000) text (about 25 characters a value); longer ones as CLOB.
const TEXT_DIMS: u32 = 150;
/// The metrics tried, in order, when the index's metric has to be found from the plan.
const PROBED_DISTANCES: [&str; 4] = ["COSINE", "EUCLIDEAN", "DOT", "MANHATTAN"];
/// Oracle's code for a read-only transaction reading a table whose definition changed after the transaction began.
const TABLE_DEFINITION_CHANGED: &str = "ORA-01466";
/// Retries (one second apart) of a read-only statement that hit ORA-01466 right after DDL on its table.
const READ_ONLY_RETRIES: u32 = 4;

/// Whether a server version (`23.26.3.0.0`) has AI Vector Search (23ai and newer).
pub(super) fn vector_ready(version: &str) -> bool {
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= 23)
}

/// Runs one `vector.*` operation on the connection's main session, as the login `login`.
pub(super) async fn invoke(engine: Arc<OraEngine>, login: String, name: &str, args: serde_json::Value) -> AppResult<serde_json::Value> {
    let ready = vector_ready(engine.server_version());
    shared::invoke(&mut OraVectors { engine, login, ready }, name, args).await
}

pub(super) struct OraVectors {
    engine: Arc<OraEngine>,
    login: String,
    ready: bool,
}

/// The `VECTOR_INFO` of a column (`VECTOR(4,FLOAT32,DENSE)`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct VectorType {
    /// `None` for `VECTOR(*)` (any number of dimensions).
    dims: Option<u32>,
    /// `FLOAT32`, `FLOAT64`, `INT8`, `BINARY` or `*`.
    format: String,
    sparse: bool,
}

impl VectorType {
    /// `VECTOR(4,FLOAT32,DENSE)`; `None` (an older release without `VECTOR_INFO`) is any dimensions, any format.
    fn parse(info: Option<&str>) -> Self {
        let inner = info
            .and_then(|info| info.trim().strip_prefix("VECTOR("))
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap_or("*");
        let mut parts = inner.split(',').map(str::trim);
        let dims = parts.next().and_then(|dims| dims.parse().ok()).filter(|dims| *dims > 0);
        let format = parts.next().unwrap_or("*").to_ascii_uppercase();
        let sparse = parts.next().is_some_and(|storage| storage.eq_ignore_ascii_case("SPARSE"));
        Self { dims, format, sparse }
    }

    /// Dense vectors of numbers (sparse vectors have no dense text form, and binary ones hold bits).
    fn readable(&self) -> bool {
        !self.sparse && self.format != "BINARY"
    }

    fn type_name(&self) -> String {
        let dims = self.dims.map_or_else(|| "*".to_owned(), |dims| dims.to_string());
        format!("VECTOR({dims}, {})", self.format)
    }
}

/// A table column as the vector tooling needs it.
#[derive(Debug, Clone)]
struct OraColumn {
    name: String,
    data_type: String,
    precision: Option<i64>,
    scale: Option<i64>,
    /// Set for `VECTOR` columns.
    vector: Option<VectorType>,
}

impl OraColumn {
    fn is_vector(&self) -> bool {
        self.vector.as_ref().is_some_and(VectorType::readable)
    }

    fn info(&self) -> ColumnInfo {
        ColumnInfo::result(
            &self.name,
            &self.data_type,
            types::kind_by_name(&self.data_type, self.precision, self.scale),
        )
    }
}

fn ora(error: oracle::Error) -> AppError {
    app(map_err(error))
}

fn quote(name: &str) -> String {
    OraDialect.quote_ident(name)
}

fn target(owner: &str, table: &str) -> String {
    format!("{}.{}", quote(owner), quote(table))
}

fn key_list(key: &[String]) -> String {
    key.iter().map(|name| quote(name)).collect::<Vec<_>>().join(", ")
}

/// The text of a vector column (`FROM_VECTOR`), as VARCHAR2 when it fits.
fn text_expr(column: &str, dims: u32) -> String {
    let returning = if dims <= TEXT_DIMS { "VARCHAR2(4000)" } else { "CLOB" };
    format!("FROM_VECTOR({} RETURNING {returning})", quote(column))
}

/// The `VECTOR_DISTANCE` metric of a search metric (`Auto` without an index is Oracle's default, COSINE).
fn distance(metric: Metric) -> &'static str {
    match metric {
        Metric::Euclidean => "EUCLIDEAN",
        Metric::Dot => "DOT",
        Metric::Cosine | Metric::Auto => "COSINE",
    }
}

/// The metric name of an Oracle distance (`EUCLIDEAN` → `euclidean`, `L2_SQUARED` → `euclidean_squared`).
fn metric_name(oracle: &str) -> String {
    match oracle.trim().to_ascii_uppercase().as_str() {
        "COSINE" => "cosine",
        "EUCLIDEAN" | "L2" => "euclidean",
        "DOT" | "INNER_PRODUCT" => "dot",
        "EUCLIDEAN_SQUARED" | "L2_SQUARED" => "euclidean_squared",
        "MANHATTAN" | "L1" => "manhattan",
        "HAMMING" => "hamming",
        "JACCARD" => "jaccard",
        other => return other.to_ascii_lowercase(),
    }
    .to_owned()
}

/// The `VECTOR_DISTANCE` metric an index orders by.
fn index_distance(index: &VectorIndex) -> &'static str {
    match index.metric.as_str() {
        "euclidean_squared" => "EUCLIDEAN_SQUARED",
        "manhattan" => "MANHATTAN",
        "hamming" => "HAMMING",
        "jaccard" => "JACCARD",
        _ => distance(Metric::of_index(index)),
    }
}

/// The index method of an `INDEX_SUBTYPE` (`NEIGHBOR_PARTITIONS_IVF` → `ivf`, `INMEMORY_NEIGHBOR_GRAPH_HNSW` → `hnsw`).
fn index_kind(subtype: &str) -> String {
    let upper = subtype.to_ascii_uppercase();
    if upper.contains("HNSW") || upper.contains("GRAPH") {
        "hnsw".to_owned()
    } else if upper.contains("IVF") || upper.contains("PARTITIONS") {
        "ivf".to_owned()
    } else if upper.is_empty() {
        "vector".to_owned()
    } else {
        upper.to_ascii_lowercase()
    }
}

/// What `VECSYS.VECTOR$INDEX.IDX_PARAMS` says about an index: its metric, HNSW neighbours and the options for display.
fn read_params(json: &str) -> Option<(String, Option<u32>, String)> {
    let params: serde_json::Value = serde_json::from_str(json).ok()?;
    let metric = metric_name(params.get("distance")?.as_str()?);
    let number = |key: &str| params.get(key).and_then(serde_json::Value::as_u64);
    let m = ["num_neighbors", "neighbors", "m"]
        .iter()
        .find_map(|key| number(key))
        .and_then(|m| u32::try_from(m).ok());
    let shown: Vec<String> = [
        ("type", "type"),
        ("accuracy", "accuracy"),
        ("num_centroids", "partitions"),
        ("num_neighbors", "neighbors"),
        ("efConstruction", "efConstruction"),
    ]
    .iter()
    .filter_map(|(key, label)| {
        let value = params.get(*key)?;
        let text = value.as_str().map_or_else(|| value.to_string(), str::to_owned);
        Some(format!("{label}={text}"))
    })
    .collect();
    Some((
        metric,
        m,
        if shown.is_empty() {
            "defaults".to_owned()
        } else {
            shown.join(", ")
        },
    ))
}

/// The index a plan reads, from its objects (`OPERATION`, `OBJECT_NAME`): an HNSW scan names the index itself (`VECTOR
/// INDEX HNSW SCAN`), an IVF search reads the index's centroid tables (`VECTOR$<INDEX>$<ids>$IVF_FLAT_CENTROIDS`).
fn index_in_plan(objects: &[(String, String)], known: &[&str]) -> Option<String> {
    let names = |object: &str| -> bool {
        known
            .iter()
            .any(|name| object == *name || object.starts_with(&format!("VECTOR${name}$")))
    };
    if let Some((_, object)) = objects.iter().find(|(_, object)| names(object)) {
        return known
            .iter()
            .find(|name| object == *name || object.starts_with(&format!("VECTOR${name}$")))
            .map(|name| (*name).to_owned());
    }
    objects.iter().find_map(|(operation, object)| {
        if operation.starts_with("VECTOR INDEX") {
            return Some(object.clone());
        }
        let rest = object.strip_prefix("VECTOR$")?;
        rest.split('$').next().filter(|name| !name.is_empty()).map(str::to_owned)
    })
}

/// The statement id of this process's rows in PLAN_TABLE.
fn plan_id() -> String {
    format!("fosta-vector-{}", std::process::id())
}

/// A plan: its objects (`OPERATION`, `OBJECT_NAME`) and its DBMS_XPLAN text.
type Plan = (Vec<(String, String)>, Vec<String>);

/// The [`Plan`] of a statement. EXPLAIN PLAN takes no bind values: the placeholders stay in the text and the optimizer
/// plans without them. The plan rows are rolled back.
fn explain(c: &Connection, sql: &str, with_text: bool) -> AppResult<Plan> {
    let id = plan_id();
    let outcome = (|| {
        c.execute(&format!("EXPLAIN PLAN SET STATEMENT_ID = '{id}' FOR {sql}"), &[])
            .map_err(ora)?;
        let objects = strings(
            c,
            "SELECT operation, object_name FROM plan_table WHERE statement_id = :1 AND object_name IS NOT NULL ORDER BY id",
            &[&id],
        )
        .map_err(app)?
        .into_iter()
        .map(|row| {
            let mut cells = row.into_iter().map(Option::unwrap_or_default);
            (cells.next().unwrap_or_default(), cells.next().unwrap_or_default())
        })
        .collect();
        let text = if with_text {
            strings(
                c,
                "SELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY('PLAN_TABLE', :1, 'TYPICAL'))",
                &[&id],
            )
            .map_err(app)?
            .into_iter()
            .map(|row| row.into_iter().next().flatten().unwrap_or_default())
            .collect()
        } else {
            Vec::new()
        };
        Ok((objects, text))
    })();
    let end = c.rollback().map_err(ora);
    outcome.and_then(|plan| end.map(|()| plan))
}

/// Runs `work` in a read-only transaction (rolled back after). A table created or altered in the last seconds fails with
/// ORA-01466 inside one, so that is retried.
fn read_only<T>(c: &Connection, mut work: impl FnMut(&Connection) -> AppResult<T>) -> AppResult<T> {
    for attempt in 0.. {
        c.execute("SET TRANSACTION READ ONLY", &[]).map_err(ora)?;
        let outcome = work(c);
        let end = c.rollback().map_err(ora);
        let changed =
            matches!(&outcome, Err(error) if error.field("vendorCode").and_then(|code| code.as_str()) == Some(TABLE_DEFINITION_CHANGED));
        if changed && attempt < READ_ONLY_RETRIES {
            end?;
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }
        return outcome.and_then(|value| end.map(|()| value));
    }
    unreachable!("the retry loop returns")
}

fn not_found(table: &str) -> AppError {
    AppError::new(format!("Table {table} not found"))
}

fn table_columns(c: &Connection, owner: &str, table: &str) -> AppResult<Vec<OraColumn>> {
    let sql = |info: &str| {
        format!(
            "SELECT column_name, data_type, data_precision, data_scale, {info} FROM all_tab_columns \
             WHERE owner = :1 AND table_name = :2 ORDER BY column_id"
        )
    };
    // VECTOR_INFO is newer than the VECTOR type; without it, dimensions come from a stored value.
    let rows = match strings(c, &sql("vector_info"), &[&owner, &table]) {
        Err(error) if error.code.as_deref() == Some("ORA-00904") => strings(c, &sql("NULL"), &[&owner, &table]),
        other => other,
    }
    .map_err(app)?;
    if rows.is_empty() {
        return Err(not_found(table));
    }
    let mut columns: Vec<OraColumn> = rows
        .into_iter()
        .map(|row| {
            let cell = |index: usize| row.get(index).cloned().flatten();
            let data_type = cell(1).unwrap_or_default();
            OraColumn {
                name: cell(0).unwrap_or_default(),
                precision: cell(2).and_then(|value| value.parse().ok()),
                scale: cell(3).and_then(|value| value.parse().ok()),
                vector: (data_type == "VECTOR").then(|| VectorType::parse(cell(4).as_deref())),
                data_type,
            }
        })
        .collect();
    for column in columns.iter_mut().filter(|column| column.is_vector()) {
        let Some(vector) = column.vector.as_mut().filter(|vector| vector.dims.is_none()) else {
            continue;
        };
        let sql = format!(
            "SELECT VECTOR_DIMENSION_COUNT({0}) FROM {1} WHERE {0} IS NOT NULL FETCH FIRST 1 ROW ONLY",
            quote(&column.name),
            target(owner, table)
        );
        vector.dims = strings(c, &sql, &[])
            .map_err(app)?
            .into_iter()
            .next()
            .and_then(|row| row.into_iter().next().flatten())
            .and_then(|dims| dims.parse().ok());
    }
    Ok(columns)
}

fn vector_columns(columns: &[OraColumn]) -> Vec<VectorColumn> {
    columns
        .iter()
        .filter(|column| column.is_vector())
        .filter_map(|column| {
            let vector = column.vector.as_ref()?;
            Some(VectorColumn {
                name: column.name.clone(),
                dims: vector.dims?,
                type_name: vector.type_name(),
            })
        })
        .collect()
}

fn find_vector<'a>(columns: &'a [OraColumn], name: &str) -> AppResult<&'a OraColumn> {
    columns
        .iter()
        .find(|column| column.name == name && column.is_vector())
        .ok_or_else(|| AppError::new(format!("{name} is not a vector column")))
}

/// The metric an index serves, found from the plan of a `FETCH APPROX` search per metric (`None` when no plan uses it).
fn probe_metric(c: &Connection, owner: &str, table: &str, index: &str, column: &str, dims: u32) -> Option<String> {
    let ones = vector_text(&vec![1.0; dims as usize]);
    PROBED_DISTANCES.iter().find_map(|metric| {
        let sql = format!(
            "SELECT 1 FROM {} ORDER BY VECTOR_DISTANCE({}, TO_VECTOR('{ones}'), {metric}) FETCH APPROX FIRST 1 ROWS ONLY",
            target(owner, table),
            quote(column)
        );
        let (objects, _) = explain(c, &sql, false).ok()?;
        index_in_plan(&objects, &[index]).map(|_| metric_name(metric))
    })
}

fn table_indexes(c: &Connection, owner: &str, table: &str) -> AppResult<Vec<VectorIndex>> {
    let rows = strings(
        c,
        "SELECT i.index_name, ic.column_name, i.index_subtype, o.object_id \
         FROM all_indexes i \
         JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name AND ic.column_position = 1 \
         LEFT JOIN all_objects o ON o.owner = i.owner AND o.object_name = i.index_name AND o.object_type = 'INDEX' \
         WHERE i.table_owner = :1 AND i.table_name = :2 AND i.index_type = 'VECTOR' \
         ORDER BY i.index_name",
        &[&owner, &table],
    )
    .map_err(app)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let columns = table_columns(c, owner, table)?;
    let mut indexes = Vec::new();
    for row in rows {
        let cell = |index: usize| row.get(index).cloned().flatten().unwrap_or_default();
        let (name, column, subtype, object_id) = (cell(0), cell(1), cell(2), cell(3));
        // Readable by DBAs and users granted SELECT on it; others get ORA-00942.
        let params = strings(
            c,
            "SELECT JSON_SERIALIZE(idx_params RETURNING VARCHAR2(4000)) FROM vecsys.vector$index WHERE idx_objn = :1",
            &[&object_id],
        )
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .and_then(|row| row.into_iter().next().flatten())
        .and_then(|json| read_params(&json));
        let (metric, m, params) = match params {
            Some(found) => found,
            None => {
                let dims = columns
                    .iter()
                    .find(|candidate| candidate.name == column)
                    .and_then(|column| column.vector.as_ref()?.dims);
                match dims.and_then(|dims| probe_metric(c, owner, table, &name, &column, dims)) {
                    Some(metric) => (
                        metric,
                        None,
                        "metric from the plan (build options need SELECT on VECSYS.VECTOR$INDEX)".to_owned(),
                    ),
                    None => (
                        "unknown".to_owned(),
                        None,
                        "metric unknown (needs SELECT on VECSYS.VECTOR$INDEX)".to_owned(),
                    ),
                }
            }
        };
        indexes.push(VectorIndex {
            name,
            column,
            metric,
            kind: index_kind(&subtype),
            m,
            params,
        });
    }
    Ok(indexes)
}

fn primary_key(c: &Connection, owner: &str, table: &str) -> AppResult<Vec<String>> {
    Ok(strings(
        c,
        "SELECT cc.column_name FROM all_constraints k \
         JOIN all_cons_columns cc ON cc.owner = k.owner AND cc.constraint_name = k.constraint_name \
         WHERE k.constraint_type = 'P' AND k.owner = :1 AND k.table_name = :2 ORDER BY cc.position",
        &[&owner, &table],
    )
    .map_err(app)?
    .into_iter()
    .filter_map(|row| row.into_iter().next().flatten())
    .collect())
}

/// The first column of each row, as vector text.
fn vectors(c: &Connection, sql: &str, params: &[&dyn ToSql]) -> AppResult<Vec<Option<Vec<f32>>>> {
    let rows = c.query(sql, params).map_err(ora)?;
    let mut out = Vec::new();
    for row in rows {
        let text: Option<String> = row.map_err(ora)?.get(0).map_err(ora)?;
        out.push(text.and_then(|text| shared::parse_vector(&text).ok()));
    }
    Ok(out)
}

fn fetch(c: &Connection, owner: &str, table: &str, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
    if key.is_empty() {
        return Err(AppError::new("Cannot identify the row"));
    }
    let columns = table_columns(c, owner, table)?;
    let vector = find_vector(&columns, column)?;
    let dims = vector.vector.as_ref().and_then(|vector| vector.dims).unwrap_or(u32::MAX);
    let mut binds: Vec<Box<dyn ToSql>> = Vec::new();
    let mut conditions = Vec::new();
    for cell in key {
        if !columns.iter().any(|candidate| candidate.name == cell.column) {
            return Err(AppError::new(format!("Unknown key column {}", cell.column)));
        }
        if matches!(cell.value, Value::Null) {
            conditions.push(format!("{} IS NULL", quote(&cell.column)));
        } else {
            binds.push(types::to_bind(&cell.value));
            conditions.push(format!("{} = :{}", quote(&cell.column), binds.len()));
        }
    }
    let sql = format!(
        "SELECT {} FROM {} WHERE {} FETCH FIRST 1 ROW ONLY",
        text_expr(&vector.name, dims),
        target(owner, table),
        conditions.join(" AND ")
    );
    let params: Vec<&dyn ToSql> = binds.iter().map(|bind| bind.as_ref()).collect();
    let rows = c.query(&sql, &params).map_err(ora)?;
    let Some(row) = rows.into_iter().next() else {
        return Err(AppError::new("Row not found"));
    };
    match row.map_err(ora)?.get::<usize, Option<String>>(0).map_err(ora)? {
        Some(text) => shared::parse_vector(&text),
        None => Err(AppError::new("This row has no vector (NULL)")),
    }
}

fn count(c: &Connection, owner: &str, table: &str, column: &str) -> AppResult<(u64, u64)> {
    let sql = format!("SELECT COUNT(*), COUNT(*) - COUNT({}) FROM {}", quote(column), target(owner, table));
    let row = strings(c, &sql, &[]).map_err(app)?.into_iter().next().unwrap_or_default();
    let number = |index: usize| {
        row.get(index)
            .cloned()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or_default()
    };
    Ok((number(0), number(1)))
}

fn order_clause(order: SampleOrder<'_>) -> String {
    match order {
        SampleOrder::Any => String::new(),
        SampleOrder::Random => " ORDER BY DBMS_RANDOM.VALUE".to_owned(),
        SampleOrder::ByKey(key) => format!(" ORDER BY {}", key_list(key)),
    }
}

/// The SQL of a search; `query` is the vector expression (`TO_VECTOR(:q)` or a literal).
fn search_sql(columns: &[OraColumn], owner: &str, table: &str, request: &SearchRequest<'_>, metric: &str, query: &str) -> String {
    let distance = format!("VECTOR_DISTANCE({}, {query}, {metric})", quote(&request.column.name));
    let select_list: String = columns
        .iter()
        .filter(|column| column.vector.is_none())
        .map(|column| format!(", {}", OraDialect.select_expr(&column.info())))
        .collect();
    format!(
        "SELECT {distance} AS \"vec_distance\"{select_list} FROM {}{} ORDER BY {distance} FETCH APPROX FIRST {} ROWS ONLY",
        target(owner, table),
        shared::where_clause(request.filter),
        request.k
    )
}

/// The SQL of [`VectorStore::nearest`]: distance and key columns, exact (ties broken by the key) or approximate.
fn nearest_sql(owner: &str, table: &str, request: &NearestRequest<'_>) -> String {
    let distance = format!(
        "VECTOR_DISTANCE({}, TO_VECTOR(:1), {})",
        quote(&request.column.name),
        index_distance(request.index)
    );
    let (order, fetch) = if request.exact {
        (format!("{distance}, {}", key_list(request.key)), "EXACT")
    } else {
        (distance.clone(), "APPROX")
    };
    format!(
        "SELECT {distance}, {} FROM {} ORDER BY {order} FETCH {fetch} FIRST {} ROWS ONLY",
        key_list(request.key),
        target(owner, table),
        request.k
    )
}

/// A query vector bound as a CLOB (any length).
fn clob(text: &String) -> (&String, &OracleType) {
    (text, &OracleType::CLOB)
}

fn search(c: &Connection, owner: &str, table: &str, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult> {
    let columns = table_columns(c, owner, table)?;
    find_vector(&columns, &request.column.name)?;
    let metric = match (request.metric, request.index) {
        (Metric::Auto, Some(index)) => index_distance(index),
        (metric, _) => distance(metric),
    };
    let text = vector_text(request.query);
    let sql = search_sql(&columns, owner, table, request, metric, "TO_VECTOR(:q)");
    let runnable_sql = search_sql(&columns, owner, table, request, metric, &format!("TO_VECTOR('{text}')"));
    let primary_key = primary_key(c, owner, table)?;
    let (objects, plan) = explain(c, &sql, true)?;
    let known: Vec<&str> = request.index.map(|index| index.name.as_str()).into_iter().collect();
    let mut result_columns = vec![ColumnInfo::result("vec_distance", "BINARY_DOUBLE", ValueKind::Float)];
    result_columns.extend(columns.iter().filter(|column| column.vector.is_none()).map(OraColumn::info));
    let (rows, elapsed_ms) = read_only(c, |c| {
        let bind = clob(&text);
        let started = Instant::now();
        let rows = c.query(&sql, &[&bind]).map_err(ora)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(types::read_row(&result_columns, row.map_err(ora)?.sql_values()));
        }
        Ok((out, started.elapsed().as_millis() as u64))
    })?;
    Ok(VectorSearchResult {
        index_used: index_in_plan(&objects, &known),
        plan: RowSet {
            columns: vec![ColumnInfo::result("PLAN_TABLE_OUTPUT", "VARCHAR2", ValueKind::Text)],
            rows: plan.into_iter().map(|line| vec![Value::Text(line)]).collect(),
        },
        result: RowSet {
            columns: result_columns,
            rows,
        },
        sql,
        runnable_sql,
        elapsed_ms,
        query_dims: request.query.len(),
        primary_key,
    })
}

fn nearest(c: &Connection, owner: &str, table: &str, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>> {
    let sql = nearest_sql(owner, table, request);
    let text = vector_text(request.query);
    read_only(c, |c| {
        let bind = clob(&text);
        let rows = c.query(&sql, &[&bind]).map_err(ora)?;
        let mut out = Vec::new();
        for row in rows {
            let values = super::row_values(&row.map_err(ora)?);
            let distance = match values.first() {
                Some(Value::Float(distance)) => *distance,
                Some(other) => other.display().and_then(|text| text.parse().ok()).unwrap_or(f64::NAN),
                None => f64::NAN,
            };
            let key = values[1..]
                .iter()
                .map(|value| value.display().unwrap_or_default())
                .collect::<Vec<_>>()
                .join(KEY_SEPARATOR);
            out.push(Neighbour { key, distance });
        }
        Ok(out)
    })
}

impl OraVectors {
    fn owner(&self, table: &TableRef) -> String {
        if table.database.is_empty() {
            self.login.clone()
        } else {
            table.database.clone()
        }
    }

    /// Runs `work` on the main session with the table's owner and name.
    async fn on_table<T: Send + 'static>(
        &self,
        table: &TableRef,
        work: impl FnOnce(&Connection, &str, &str) -> AppResult<T> + Send + 'static,
    ) -> AppResult<T> {
        let (owner, name) = (self.owner(table), table.table.clone());
        self.engine.on_conn(move |c| work(c, &owner, &name)).await
    }
}

#[async_trait]
impl VectorStore for OraVectors {
    fn profile(&self) -> &'static VectorProfile {
        &PROFILE
    }

    async fn supported(&mut self, _table: &TableRef) -> AppResult<bool> {
        Ok(self.ready)
    }

    async fn columns(&mut self, table: &TableRef) -> AppResult<Vec<VectorColumn>> {
        self.on_table(table, |c, owner, name| Ok(vector_columns(&table_columns(c, owner, name)?)))
            .await
    }

    async fn indexes(&mut self, table: &TableRef) -> AppResult<Vec<VectorIndex>> {
        self.on_table(table, table_indexes).await
    }

    async fn primary_key(&mut self, table: &TableRef) -> AppResult<Vec<String>> {
        self.on_table(table, primary_key).await
    }

    async fn fetch(&mut self, table: &TableRef, column: &str, key: &[KeyCell]) -> AppResult<Vec<f32>> {
        let (column, key) = (column.to_owned(), key.to_vec());
        self.on_table(table, move |c, owner, name| fetch(c, owner, name, &column, &key))
            .await
    }

    async fn count(&mut self, table: &TableRef, column: &VectorColumn) -> AppResult<(u64, u64)> {
        let column = column.name.clone();
        self.on_table(table, move |c, owner, name| count(c, owner, name, &column)).await
    }

    async fn sample(
        &mut self,
        table: &TableRef,
        column: &VectorColumn,
        limit: u32,
        order: SampleOrder<'_>,
    ) -> AppResult<Vec<Option<Vec<f32>>>> {
        let (vector, expr) = (quote(&column.name), text_expr(&column.name, column.dims));
        let order = order_clause(order);
        self.on_table(table, move |c, owner, name| {
            let sql = format!(
                "SELECT {expr} FROM {} WHERE {vector} IS NOT NULL{order} FETCH FIRST {limit} ROWS ONLY",
                target(owner, name)
            );
            vectors(c, &sql, &[])
        })
        .await
    }

    async fn search(&mut self, table: &TableRef, request: &SearchRequest<'_>) -> AppResult<VectorSearchResult> {
        let (column, query, index, filter) = (
            request.column.clone(),
            request.query.to_vec(),
            request.index.cloned(),
            request.filter.map(str::to_owned),
        );
        let (k, metric) = (request.k, request.metric);
        self.on_table(table, move |c, owner, name| {
            let request = SearchRequest {
                column: &column,
                query: &query,
                k,
                metric,
                index: index.as_ref(),
                filter: filter.as_deref(),
            };
            search(c, owner, name, &request)
        })
        .await
    }

    async fn nearest(&mut self, table: &TableRef, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>> {
        let (column, key, index, query) = (
            request.column.clone(),
            request.key.to_vec(),
            request.index.clone(),
            request.query.to_vec(),
        );
        let (k, exact) = (request.k, request.exact);
        self.on_table(table, move |c, owner, name| {
            let request = NearestRequest {
                column: &column,
                key: &key,
                index: &index,
                query: &query,
                k,
                exact,
            };
            nearest(c, owner, name, &request)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(name: &str, metric: &str) -> VectorIndex {
        VectorIndex {
            name: name.into(),
            column: "EMBEDDING".into(),
            metric: metric.into(),
            kind: "ivf".into(),
            m: None,
            params: String::new(),
        }
    }

    fn column(name: &str) -> VectorColumn {
        VectorColumn {
            name: name.into(),
            dims: 4,
            type_name: "VECTOR(4, FLOAT32)".into(),
        }
    }

    #[test]
    fn versions_and_vector_types_parse() {
        assert!(vector_ready("23.26.3.0.0"));
        assert!(vector_ready("23.4.0.24.5"));
        assert!(!vector_ready("21.3.0.0.0"));
        assert!(!vector_ready(""));
        let dense = VectorType::parse(Some("VECTOR(4,FLOAT32,DENSE)"));
        assert_eq!((dense.dims, dense.format.as_str(), dense.sparse), (Some(4), "FLOAT32", false));
        assert!(dense.readable());
        assert_eq!(dense.type_name(), "VECTOR(4, FLOAT32)");
        let flexible = VectorType::parse(Some("VECTOR(*,*,DENSE)"));
        assert_eq!((flexible.dims, flexible.format.as_str()), (None, "*"));
        assert_eq!(flexible.type_name(), "VECTOR(*, *)");
        assert!(!VectorType::parse(Some("VECTOR(1000,FLOAT32,SPARSE)")).readable());
        assert!(!VectorType::parse(Some("VECTOR(64,BINARY,DENSE)")).readable());
        assert_eq!(VectorType::parse(None).dims, None);
    }

    #[test]
    fn oracle_vector_text_parses() {
        let values = shared::parse_vector("[2.00000003E-001,4.54545468E-001,-5.38461566E-001,1.0E+000]").unwrap();
        assert_eq!(values, vec![0.2, 0.454_545_47, -0.538_461_57, 1.0]);
        assert_eq!(text_expr("v", 4), "FROM_VECTOR(\"v\" RETURNING VARCHAR2(4000))");
        assert_eq!(text_expr("v", 1536), "FROM_VECTOR(\"v\" RETURNING CLOB)");
    }

    #[test]
    fn metrics_and_index_kinds_map() {
        assert_eq!(metric_name("COSINE"), "cosine");
        assert_eq!(metric_name("L2_SQUARED"), "euclidean_squared");
        assert_eq!(metric_name("EUCLIDEAN"), "euclidean");
        assert_eq!(metric_name("DOT"), "dot");
        assert_eq!(index_distance(&index("i", "cosine")), "COSINE");
        assert_eq!(index_distance(&index("i", "euclidean")), "EUCLIDEAN");
        assert_eq!(index_distance(&index("i", "dot")), "DOT");
        assert_eq!(index_distance(&index("i", "manhattan")), "MANHATTAN");
        assert_eq!(index_distance(&index("i", "unknown")), "EUCLIDEAN");
        assert_eq!(distance(Metric::Auto), "COSINE");
        assert_eq!(index_kind("NEIGHBOR_PARTITIONS_IVF"), "ivf");
        assert_eq!(index_kind("INMEMORY_NEIGHBOR_GRAPH_HNSW"), "hnsw");
        assert_eq!(index_kind(""), "vector");
    }

    #[test]
    fn index_params_are_read() {
        let json = r#"{"target_centroids":34,"type":"IVF_FLAT","vector_dimension":4,"distance":"COSINE","accuracy":90,"num_centroids":23}"#;
        let (metric, m, params) = read_params(json).unwrap();
        assert_eq!((metric.as_str(), m), ("cosine", None));
        assert_eq!(params, "type=IVF_FLAT, accuracy=90, partitions=23");
        let hnsw = r#"{"type":"HNSW","distance":"EUCLIDEAN","num_neighbors":32,"efConstruction":300}"#;
        let (metric, m, params) = read_params(hnsw).unwrap();
        assert_eq!((metric.as_str(), m), ("euclidean", Some(32)));
        assert_eq!(params, "type=HNSW, neighbors=32, efConstruction=300");
        assert!(read_params("{}").is_none());
        assert!(read_params("not json").is_none());
    }

    #[test]
    fn plans_name_the_index_they_read() {
        let ivf = vec![
            ("VIEW".to_owned(), "VW_IVPSR_11E7D7DE".to_owned()),
            (
                "TABLE ACCESS".to_owned(),
                "VECTOR$VPROBE_IVF$76650_76672_0$IVF_FLAT_CENTROIDS".to_owned(),
            ),
            ("TABLE ACCESS".to_owned(), "VPROBE".to_owned()),
        ];
        assert_eq!(index_in_plan(&ivf, &["VPROBE_IVF"]).as_deref(), Some("VPROBE_IVF"));
        assert_eq!(index_in_plan(&ivf, &[]).as_deref(), Some("VPROBE_IVF"));
        let hnsw = vec![
            ("VECTOR INDEX".to_owned(), "DOCS_HNSW".to_owned()),
            ("TABLE ACCESS".to_owned(), "DOCS".to_owned()),
        ];
        assert_eq!(index_in_plan(&hnsw, &["DOCS_HNSW"]).as_deref(), Some("DOCS_HNSW"));
        assert_eq!(index_in_plan(&hnsw, &[]).as_deref(), Some("DOCS_HNSW"));
        let full = vec![("TABLE ACCESS".to_owned(), "VPROBE".to_owned())];
        assert_eq!(index_in_plan(&full, &["VPROBE_IVF"]), None);
    }

    #[test]
    fn search_and_nearest_sql_render() {
        let columns = vec![
            OraColumn {
                name: "id".into(),
                data_type: "NUMBER".into(),
                precision: Some(10),
                scale: Some(0),
                vector: None,
            },
            OraColumn {
                name: "meta".into(),
                data_type: "JSON".into(),
                precision: None,
                scale: None,
                vector: None,
            },
            OraColumn {
                name: "embedding".into(),
                data_type: "VECTOR".into(),
                precision: None,
                scale: None,
                vector: Some(VectorType::parse(Some("VECTOR(4,FLOAT32,DENSE)"))),
            },
        ];
        let vector = column("embedding");
        let request = SearchRequest {
            column: &vector,
            query: &[1.0, 0.0, 0.0, 0.0],
            k: 5,
            metric: Metric::Cosine,
            index: None,
            filter: Some("\"id\" > 3"),
        };
        assert_eq!(
            search_sql(&columns, "SHOP", "T", &request, "COSINE", "TO_VECTOR(:q)"),
            "SELECT VECTOR_DISTANCE(\"embedding\", TO_VECTOR(:q), COSINE) AS \"vec_distance\", \"id\", JSON_SERIALIZE(\"meta\") AS \"meta\" \
             FROM \"SHOP\".\"T\" WHERE \"id\" > 3 ORDER BY VECTOR_DISTANCE(\"embedding\", TO_VECTOR(:q), COSINE) FETCH APPROX FIRST 5 ROWS ONLY"
        );
        let idx = index("T_IDX", "dot");
        let key = vec!["id".to_owned()];
        let mut nearest = NearestRequest {
            column: &vector,
            key: &key,
            index: &idx,
            query: &[1.0, 0.0, 0.0, 0.0],
            k: 3,
            exact: true,
        };
        assert_eq!(
            nearest_sql("SHOP", "T", &nearest),
            "SELECT VECTOR_DISTANCE(\"embedding\", TO_VECTOR(:1), DOT), \"id\" FROM \"SHOP\".\"T\" \
             ORDER BY VECTOR_DISTANCE(\"embedding\", TO_VECTOR(:1), DOT), \"id\" FETCH EXACT FIRST 3 ROWS ONLY"
        );
        nearest.exact = false;
        assert!(nearest_sql("SHOP", "T", &nearest)
            .ends_with("ORDER BY VECTOR_DISTANCE(\"embedding\", TO_VECTOR(:1), DOT) FETCH APPROX FIRST 3 ROWS ONLY"));
        assert_eq!(order_clause(SampleOrder::Random), " ORDER BY DBMS_RANDOM.VALUE");
        assert_eq!(order_clause(SampleOrder::ByKey(&key)), " ORDER BY \"id\"");
        assert_eq!(order_clause(SampleOrder::Any), "");
        assert_eq!(target("SHOP", "T"), "\"SHOP\".\"T\"");
    }
}
