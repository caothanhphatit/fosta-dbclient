//! Vector column health checks and the index recall measurement.

use super::*;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheck {
    pub label: String,
    pub status: CheckStatus,
    pub value: String,
    pub detail: String,
}

impl HealthCheck {
    pub(super) fn new(label: &str, status: CheckStatus, value: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            status,
            value: value.into(),
            detail: detail.into(),
        }
    }

    pub(super) fn failed(label: &str, error: &AppError) -> Self {
        Self::new(label, CheckStatus::Error, "check failed", error.to_string())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallReport {
    pub index: String,
    pub metric: String,
    pub k: u32,
    pub queries: usize,
    pub recall: f64,
    pub worst_recall: f64,
    pub ann_ms: f64,
    pub exact_ms: f64,
}

#[derive(Default)]
pub(super) struct SampleStats {
    total: usize,
    invalid_length: usize,
    non_finite: usize,
    zero: usize,
    duplicates: usize,
    norm_min: f32,
    norm_max: f32,
    norm_sum: f64,
}

impl SampleStats {
    pub(super) fn collect(vectors: &[Vec<u8>], dims: u32) -> Self {
        let mut stats = Self {
            norm_min: f32::INFINITY,
            norm_max: 0.0,
            ..Self::default()
        };
        let mut seen = HashSet::new();
        for bytes in vectors {
            stats.total += 1;
            if !seen.insert(bytes.as_slice()) {
                stats.duplicates += 1;
            }
            let Some(values) = decode_vector(bytes).filter(|values| values.len() == dims as usize) else {
                stats.invalid_length += 1;
                continue;
            };
            if values.iter().any(|value| !value.is_finite()) {
                stats.non_finite += 1;
                continue;
            }
            let length = norm(&values);
            if length == 0.0 {
                stats.zero += 1;
            }
            stats.norm_min = stats.norm_min.min(length);
            stats.norm_max = stats.norm_max.max(length);
            stats.norm_sum += f64::from(length);
        }
        stats
    }

    pub(super) fn measured(&self) -> usize {
        self.total - self.invalid_length - self.non_finite
    }

    pub(super) fn normalized(&self) -> bool {
        (self.norm_min - 1.0).abs() <= NORM_TOLERANCE && (self.norm_max - 1.0).abs() <= NORM_TOLERANCE
    }
}

pub async fn health(conn: &mut Conn, database: &str, table: &str, column: &str, sample: u32) -> AppResult<Vec<HealthCheck>> {
    let dims = vector_columns(conn, database, table)
        .await?
        .get(column)
        .copied()
        .ok_or_else(|| AppError::new(format!("{column} is not a VECTOR column")))?;
    let index = vector_indexes(conn, database, table)
        .await?
        .into_iter()
        .find(|index| index.column == column);
    let mut checks = vec![
        HealthCheck::new("Column", CheckStatus::Info, format!("VECTOR({dims})"), ""),
        index_check(index.as_ref()),
        count_check(conn, database, table, column).await,
    ];
    match sample_vectors(conn, database, table, column, sample).await {
        Ok(vectors) => checks.extend(sample_checks(&SampleStats::collect(&vectors, dims), dims, index.as_ref())),
        Err(error) => checks.push(HealthCheck::failed("Sample", &error)),
    }
    Ok(checks)
}

pub async fn index_recall(conn: &mut Conn, database: &str, table: &str, column: &str, k: u32, queries: u32) -> AppResult<RecallReport> {
    let index = vector_indexes(conn, database, table)
        .await?
        .into_iter()
        .find(|index| index.column == column)
        .ok_or_else(|| AppError::new("There is no vector index on this column, so there is nothing to compare"))?;
    let key = primary_key(conn, database, table).await?;
    if key.is_empty() {
        return Err(AppError::new("Measuring recall needs a primary key to compare results"));
    }
    let k = k.clamp(1, MAX_RECALL_K);
    let target = qualified(database, table);
    let vector_column = quote_ident(column);
    let key_list = key.iter().map(|name| quote_ident(name)).collect::<Vec<_>>().join(", ");
    let function = Metric::of_index(&index).function();
    let samples: Vec<Row> = conn
        .query(format!(
            "SELECT {vector_column} FROM {target} WHERE {vector_column} IS NOT NULL ORDER BY RAND() LIMIT {}",
            queries.clamp(1, MAX_RECALL_QUERIES)
        ))
        .await?;
    let probes: Vec<Vec<f32>> = samples.into_iter().filter_map(first_vector).collect();
    if probes.is_empty() {
        return Err(AppError::new("The table has no vectors to sample"));
    }
    let mut recalls = Vec::with_capacity(probes.len());
    let (mut ann_total, mut exact_total) = (0.0, 0.0);
    for probe in &probes {
        let ordering = format!(
            "ORDER BY {function}({vector_column}, VEC_FromText('{}')) LIMIT {k}",
            vector_literal(probe)
        );
        let (approximate, ann_ms) = timed_keys(conn, format!("SELECT {key_list} FROM {target} {ordering}")).await?;
        let (exact, exact_ms) = timed_keys(
            conn,
            format!(
                "SELECT {key_list} FROM {target} IGNORE INDEX ({}) {ordering}",
                quote_ident(&index.name)
            ),
        )
        .await?;
        let hits = exact.iter().filter(|row_key| approximate.contains(*row_key)).count();
        recalls.push(if exact.is_empty() { 1.0 } else { hits as f64 / exact.len() as f64 });
        ann_total += ann_ms;
        exact_total += exact_ms;
    }
    let count = recalls.len() as f64;
    Ok(RecallReport {
        metric: index.metric.clone(),
        index: index.name,
        k,
        queries: recalls.len(),
        recall: recalls.iter().sum::<f64>() / count,
        worst_recall: recalls.iter().copied().fold(1.0, f64::min),
        ann_ms: ann_total / count,
        exact_ms: exact_total / count,
    })
}

pub(super) fn norm(values: &[f32]) -> f32 {
    values.iter().map(|value| value * value).sum::<f32>().sqrt()
}

pub(super) async fn timed_keys(conn: &mut Conn, sql: String) -> AppResult<(HashSet<String>, f64)> {
    let started = Instant::now();
    let rows: Vec<Row> = conn.query(sql).await?;
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let keys = rows
        .into_iter()
        .map(|row| {
            row.unwrap()
                .into_iter()
                .map(|value| key_text(value).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(KEY_SEPARATOR)
        })
        .collect();
    Ok((keys, elapsed))
}

pub(super) async fn sample_vectors(conn: &mut Conn, database: &str, table: &str, column: &str, sample: u32) -> AppResult<Vec<Vec<u8>>> {
    let vector_column = quote_ident(column);
    let rows: Vec<Row> = conn
        .query(format!(
            "SELECT {vector_column} FROM {} WHERE {vector_column} IS NOT NULL LIMIT {}",
            qualified(database, table),
            sample.clamp(1, MAX_SAMPLE)
        ))
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match row.unwrap().into_iter().next() {
            Some(Value::Bytes(bytes)) => Some(bytes),
            _ => None,
        })
        .collect())
}

pub(super) async fn count_check(conn: &mut Conn, database: &str, table: &str, column: &str) -> HealthCheck {
    let sql = format!(
        "SELECT COUNT(*), COALESCE(SUM({} IS NULL), 0) FROM {}",
        quote_ident(column),
        qualified(database, table)
    );
    let counts: AppResult<Option<(u64, u64)>> = conn.query_first(sql).await.map_err(AppError::from);
    match counts {
        Ok(Some((total, missing))) if missing > 0 => HealthCheck::new(
            "Rows without a vector",
            CheckStatus::Warn,
            format!("{missing} of {total}"),
            "These rows never appear in similarity search results.",
        ),
        Ok(Some((total, _))) => HealthCheck::new("Rows", CheckStatus::Ok, format!("{total} rows, all with a vector"), ""),
        Ok(None) => HealthCheck::new("Rows", CheckStatus::Info, "0", "The table is empty."),
        Err(error) => HealthCheck::failed("Rows", &error),
    }
}

pub(super) fn index_check(index: Option<&VectorIndex>) -> HealthCheck {
    match index {
        Some(index) => {
            let m = index.m.map_or_else(|| "default".to_owned(), |m| m.to_string());
            HealthCheck::new(
                "Vector index",
                CheckStatus::Ok,
                format!("{} · {} · M={m}", index.name, index.metric),
                "Searches must ORDER BY the matching distance function and use LIMIT to use this index.",
            )
        }
        None => HealthCheck::new(
            "Vector index",
            CheckStatus::Warn,
            "none",
            "Every similarity search scans the whole table. Add one with: ALTER TABLE … ADD VECTOR INDEX (column) DISTANCE=cosine",
        ),
    }
}

pub(super) fn sample_checks(stats: &SampleStats, dims: u32, index: Option<&VectorIndex>) -> Vec<HealthCheck> {
    let mut checks = vec![HealthCheck::new(
        "Sample",
        CheckStatus::Info,
        format!("{} vectors read", stats.total),
        "",
    )];
    checks.push(counted_check(
        "Wrong length",
        stats.invalid_length,
        CheckStatus::Error,
        &format!("Vectors whose size is not {dims} floats."),
    ));
    checks.push(counted_check(
        "NaN or Infinity",
        stats.non_finite,
        CheckStatus::Error,
        "Distances to these vectors are meaningless.",
    ));
    checks.push(counted_check(
        "Zero vectors",
        stats.zero,
        CheckStatus::Error,
        "Cosine distance is undefined for all-zero vectors.",
    ));
    checks.push(counted_check(
        "Identical vectors",
        stats.duplicates,
        CheckStatus::Warn,
        "Exact duplicates in the sample, usually the same text embedded twice.",
    ));
    if stats.measured() > 0 {
        checks.push(norm_check(stats, index));
    }
    checks
}

pub(super) fn counted_check(label: &str, count: usize, failing: CheckStatus, detail: &str) -> HealthCheck {
    if count == 0 {
        HealthCheck::new(label, CheckStatus::Ok, "none", "")
    } else {
        HealthCheck::new(label, failing, count.to_string(), detail)
    }
}

pub(super) fn norm_check(stats: &SampleStats, index: Option<&VectorIndex>) -> HealthCheck {
    let mean = stats.norm_sum / stats.measured() as f64;
    let value = format!("min {:.3} · mean {mean:.3} · max {:.3}", stats.norm_min, stats.norm_max);
    let euclidean_index = index.is_some_and(|index| Metric::of_index(index) == Metric::Euclidean);
    if stats.normalized() {
        HealthCheck::new(
            "Vector length",
            CheckStatus::Ok,
            value,
            "Unit length: cosine and euclidean rank the same way.",
        )
    } else if euclidean_index {
        HealthCheck::new(
            "Vector length",
            CheckStatus::Warn,
            value,
            "Vectors are not unit length and the index uses euclidean distance, so longer vectors are penalised. Normalize them or use DISTANCE=cosine.",
        )
    } else {
        HealthCheck::new(
            "Vector length",
            CheckStatus::Info,
            value,
            "Not unit length. Fine for cosine distance.",
        )
    }
}
