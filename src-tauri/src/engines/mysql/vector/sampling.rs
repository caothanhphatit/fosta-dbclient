//! Counts, samples and top-k keys for the shared health checks and recall measurement.

use super::*;

pub(super) async fn count(conn: &mut Conn, table: &TableRef, column: &str) -> AppResult<(u64, u64)> {
    let sql = format!(
        "SELECT COUNT(*), COALESCE(SUM({} IS NULL), 0) FROM {}",
        quote_ident(column),
        qualified(&table.database, &table.table)
    );
    let counts: Option<(u64, u64)> = conn.query_first(sql).await?;
    Ok(counts.unwrap_or_default())
}

pub(super) async fn sample(
    conn: &mut Conn,
    table: &TableRef,
    column: &str,
    limit: u32,
    order: SampleOrder<'_>,
) -> AppResult<Vec<Option<Vec<f32>>>> {
    let vector_column = quote_ident(column);
    let order = match order {
        SampleOrder::Any => String::new(),
        SampleOrder::Random => " ORDER BY RAND()".to_owned(),
        SampleOrder::ByKey(key) => format!(" ORDER BY {}", key_list(key)),
    };
    let rows: Vec<Row> = conn
        .query(format!(
            "SELECT {vector_column} FROM {} WHERE {vector_column} IS NOT NULL{order} LIMIT {limit}",
            qualified(&table.database, &table.table),
        ))
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match row.unwrap().into_iter().next() {
            Some(Value::Bytes(bytes)) => Some(decode_vector(&bytes)),
            _ => None,
        })
        .collect())
}

pub(super) async fn nearest(conn: &mut Conn, table: &TableRef, request: &NearestRequest<'_>) -> AppResult<Vec<Neighbour>> {
    let index = request.exact.then(|| quote_ident(&request.index.name));
    let sql = nearest_sql(
        &qualified(&table.database, &table.table),
        &quote_ident(&request.column.name),
        &key_list(request.key),
        distance_function(Metric::of_index(request.index)),
        &vector_literal(request.query),
        request.k,
        index.as_deref(),
    );
    let rows: Vec<Row> = conn.query(sql).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let mut cells = row.unwrap().into_iter();
            let distance = cells.next().map_or(f64::NAN, float_of);
            let key = cells
                .map(|value| key_text(value).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(KEY_SEPARATOR);
            Neighbour { key, distance }
        })
        .collect())
}

/// Top-k through the index, or exact with `ignore_index` (ordered by key among equal distances).
pub(super) fn nearest_sql(
    target: &str,
    column: &str,
    key_list: &str,
    function: &str,
    literal: &str,
    k: u32,
    ignore_index: Option<&str>,
) -> String {
    let distance = format!("{function}({column}, VEC_FromText('{literal}'))");
    match ignore_index {
        Some(index) => {
            format!("SELECT {distance}, {key_list} FROM {target} IGNORE INDEX ({index}) ORDER BY {distance}, {key_list} LIMIT {k}")
        }
        None => format!("SELECT {distance}, {key_list} FROM {target} ORDER BY {distance} LIMIT {k}"),
    }
}

fn key_list(key: &[String]) -> String {
    key.iter().map(|name| quote_ident(name)).collect::<Vec<_>>().join(", ")
}

fn float_of(value: Value) -> f64 {
    match value {
        Value::Double(value) => value,
        Value::Float(value) => f64::from(value),
        other => plain_text(other).and_then(|text| text.parse().ok()).unwrap_or(f64::NAN),
    }
}
