//! Vector column health checks: the column, its index, rows without a vector, and statistics on sampled vectors
//! (wrong length, NaN or infinity, zero vectors, duplicates, lengths).

use std::collections::HashSet;

use serde::Serialize;

use super::{find_column, norm, Metric, SampleOrder, TableRef, VectorIndex, VectorProfile, VectorStore, MAX_SAMPLE};
use crate::error::{AppError, AppResult};

const NORM_TOLERANCE: f32 = 0.01;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
    pub fn new(label: &str, status: CheckStatus, value: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.to_owned(),
            status,
            value: value.into(),
            detail: detail.into(),
        }
    }

    pub fn failed(label: &str, error: &AppError) -> Self {
        Self::new(label, CheckStatus::Error, "check failed", error.to_string())
    }
}

/// Statistics of sampled vectors (`None` stands for a value that did not decode as a vector).
#[derive(Debug, Default)]
pub struct SampleStats {
    pub total: usize,
    pub invalid_length: usize,
    pub non_finite: usize,
    pub zero: usize,
    pub duplicates: usize,
    pub norm_min: f32,
    pub norm_max: f32,
    pub norm_sum: f64,
}

impl SampleStats {
    pub fn collect(vectors: &[Option<Vec<f32>>], dims: u32) -> Self {
        let mut stats = Self {
            norm_min: f32::INFINITY,
            norm_max: 0.0,
            ..Self::default()
        };
        let mut seen: HashSet<Vec<u32>> = HashSet::new();
        for vector in vectors {
            stats.total += 1;
            let Some(values) = vector else {
                stats.invalid_length += 1;
                continue;
            };
            // Bit patterns: the same comparison as the stored bytes.
            if !seen.insert(values.iter().map(|value| value.to_bits()).collect()) {
                stats.duplicates += 1;
            }
            if values.len() != dims as usize {
                stats.invalid_length += 1;
                continue;
            }
            if values.iter().any(|value| !value.is_finite()) {
                stats.non_finite += 1;
                continue;
            }
            let length = norm(values);
            if length == 0.0 {
                stats.zero += 1;
            }
            stats.norm_min = stats.norm_min.min(length);
            stats.norm_max = stats.norm_max.max(length);
            stats.norm_sum += f64::from(length);
        }
        stats
    }

    pub fn measured(&self) -> usize {
        self.total - self.invalid_length - self.non_finite
    }

    pub fn normalized(&self) -> bool {
        (self.norm_min - 1.0).abs() <= NORM_TOLERANCE && (self.norm_max - 1.0).abs() <= NORM_TOLERANCE
    }
}

pub async fn health<S: VectorStore + ?Sized>(store: &mut S, table: &TableRef, column: &str, sample: u32) -> AppResult<Vec<HealthCheck>> {
    let profile = store.profile();
    let column = find_column(store, table, column).await?;
    let index = store.indexes(table).await?.into_iter().find(|index| index.column == column.name);
    let mut checks = vec![
        HealthCheck::new("Column", CheckStatus::Info, column.type_name.clone(), ""),
        index_check(profile, index.as_ref()),
    ];
    checks.push(match store.count(table, &column).await {
        Ok((total, missing)) => count_check(total, missing),
        Err(error) => HealthCheck::failed("Rows", &error),
    });
    match store.sample(table, &column, sample.clamp(1, MAX_SAMPLE), SampleOrder::Any).await {
        Ok(vectors) => checks.extend(sample_checks(
            profile,
            &SampleStats::collect(&vectors, column.dims),
            column.dims,
            index.as_ref(),
        )),
        Err(error) => checks.push(HealthCheck::failed("Sample", &error)),
    }
    Ok(checks)
}

fn count_check(total: u64, missing: u64) -> HealthCheck {
    if missing > 0 {
        HealthCheck::new(
            "Rows without a vector",
            CheckStatus::Warn,
            format!("{missing} of {total}"),
            "These rows never appear in similarity search results.",
        )
    } else {
        HealthCheck::new("Rows", CheckStatus::Ok, format!("{total} rows, all with a vector"), "")
    }
}

fn index_check(profile: &VectorProfile, index: Option<&VectorIndex>) -> HealthCheck {
    match index {
        Some(index) => {
            let params = if index.params.is_empty() {
                String::new()
            } else {
                format!(" · {}", index.params)
            };
            HealthCheck::new(
                "Vector index",
                CheckStatus::Ok,
                format!("{} · {} · {}{params}", index.name, index.kind, index.metric),
                profile.index_usage,
            )
        }
        None => HealthCheck::new(
            "Vector index",
            CheckStatus::Warn,
            "none",
            format!("Every similarity search scans the whole table. {}", profile.add_index),
        ),
    }
}

fn sample_checks(profile: &VectorProfile, stats: &SampleStats, dims: u32, index: Option<&VectorIndex>) -> Vec<HealthCheck> {
    let mut checks = vec![
        HealthCheck::new("Sample", CheckStatus::Info, format!("{} vectors read", stats.total), ""),
        counted_check(
            "Wrong length",
            stats.invalid_length,
            CheckStatus::Error,
            &format!("Vectors whose size is not {dims} floats."),
        ),
        counted_check(
            "NaN or Infinity",
            stats.non_finite,
            CheckStatus::Error,
            "Distances to these vectors are meaningless.",
        ),
        counted_check(
            "Zero vectors",
            stats.zero,
            CheckStatus::Error,
            "Cosine distance is undefined for all-zero vectors.",
        ),
        counted_check(
            "Identical vectors",
            stats.duplicates,
            CheckStatus::Warn,
            "Exact duplicates in the sample, usually the same text embedded twice.",
        ),
    ];
    if stats.measured() > 0 {
        checks.push(norm_check(profile, stats, index));
    }
    checks
}

fn counted_check(label: &str, count: usize, failing: CheckStatus, detail: &str) -> HealthCheck {
    if count == 0 {
        HealthCheck::new(label, CheckStatus::Ok, "none", "")
    } else {
        HealthCheck::new(label, failing, count.to_string(), detail)
    }
}

fn norm_check(profile: &VectorProfile, stats: &SampleStats, index: Option<&VectorIndex>) -> HealthCheck {
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
            format!(
                "Vectors are not unit length and the index uses euclidean distance, so longer vectors are penalised. {}",
                profile.euclidean_advice
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: VectorProfile = VectorProfile {
        metrics: &[Metric::Cosine],
        schemas: false,
        index_usage: "usage",
        add_index: "Add one.",
        euclidean_advice: "Normalize.",
    };

    fn index(metric: &str) -> VectorIndex {
        VectorIndex {
            name: "idx".into(),
            column: "v".into(),
            metric: metric.into(),
            kind: "hnsw".into(),
            m: Some(6),
            params: "M=6".into(),
        }
    }

    #[test]
    fn sampled_vectors_are_counted_by_problem() {
        let samples = vec![
            Some(vec![3.0, 4.0]),
            Some(vec![3.0, 4.0]),
            Some(vec![0.0, 0.0]),
            Some(vec![1.0]),
            Some(vec![f32::NAN, 1.0]),
            None,
        ];
        let stats = SampleStats::collect(&samples, 2);
        assert_eq!(stats.total, 6);
        assert_eq!(stats.duplicates, 1);
        assert_eq!(stats.invalid_length, 2);
        assert_eq!(stats.non_finite, 1);
        assert_eq!(stats.zero, 1);
        assert_eq!(stats.measured(), 3);
        assert_eq!((stats.norm_min, stats.norm_max), (0.0, 5.0));
        assert!(!stats.normalized());
        let unit = SampleStats::collect(&[Some(vec![1.0, 0.0]), Some(vec![0.0, 1.0])], 2);
        assert!(unit.normalized());
    }

    #[test]
    fn checks_use_the_engine_texts() {
        let missing = index_check(&PROFILE, None);
        assert_eq!(missing.status, CheckStatus::Warn);
        assert!(missing.detail.ends_with("Add one."));
        assert_eq!(index_check(&PROFILE, Some(&index("cosine"))).value, "idx · hnsw · cosine · M=6");
        let stats = SampleStats::collect(&[Some(vec![3.0, 4.0])], 2);
        let euclidean = norm_check(&PROFILE, &stats, Some(&index("euclidean")));
        assert_eq!(euclidean.status, CheckStatus::Warn);
        assert!(euclidean.detail.ends_with("Normalize."));
        assert_eq!(norm_check(&PROFILE, &stats, Some(&index("cosine"))).status, CheckStatus::Info);
        let checks = sample_checks(&PROFILE, &stats, 2, None);
        assert_eq!(checks.len(), 6);
        assert_eq!(count_check(10, 2).status, CheckStatus::Warn);
        assert_eq!(count_check(10, 0).value, "10 rows, all with a vector");
    }
}
