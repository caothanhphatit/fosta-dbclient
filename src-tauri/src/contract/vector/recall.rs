//! Index recall: top-k through the index against exact top-k, for sampled query vectors.

use std::{collections::HashSet, time::Instant};

use serde::Serialize;

use super::{
    find_column, NearestRequest, Neighbour, RecallArgs, SampleOrder, TableRef, VectorStore, MAX_RECALL_K, MAX_RECALL_QUERIES, RECALL_POOL,
};
use crate::error::{AppError, AppResult};

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

/// SplitMix64: a small seeded generator, so seeded recall runs pick the same queries everywhere.
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound` (`bound > 0`).
    pub fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }
}

/// `count` distinct items of `pool`, chosen with `seed` (a partial Fisher-Yates shuffle).
pub fn pick_probes<T>(mut pool: Vec<T>, count: usize, seed: u64) -> Vec<T> {
    let mut rng = SplitMix64::new(seed);
    let count = count.min(pool.len());
    for i in 0..count {
        let j = i + rng.below(pool.len() - i);
        pool.swap(i, j);
    }
    pool.truncate(count);
    pool
}

/// The share of the exact top-k the approximate top-k found. Ties count: an approximate row as close as the exact
/// k-th row is as good as the row it displaced (with duplicate vectors, any of the tied rows is a correct answer).
pub fn recall_at_k(approximate: &[Neighbour], exact: &[Neighbour]) -> f64 {
    let Some(last) = exact.last() else {
        return 1.0;
    };
    let threshold = last.distance + 1e-6 * last.distance.abs().max(1.0);
    let keys: HashSet<&str> = exact.iter().map(|neighbour| neighbour.key.as_str()).collect();
    let hits = approximate
        .iter()
        .filter(|neighbour| keys.contains(neighbour.key.as_str()) || neighbour.distance <= threshold)
        .count();
    hits.min(exact.len()) as f64 / exact.len() as f64
}

pub async fn index_recall<S: VectorStore + ?Sized>(store: &mut S, args: &RecallArgs) -> AppResult<RecallReport> {
    let table = TableRef::parse(&args.database, &args.table, store.profile().schemas);
    let column = find_column(store, &table, &args.column).await?;
    let index = store
        .indexes(&table)
        .await?
        .into_iter()
        .find(|index| index.column == column.name)
        .ok_or_else(|| AppError::new("There is no vector index on this column, so there is nothing to compare"))?;
    let key = store.primary_key(&table).await?;
    if key.is_empty() {
        return Err(AppError::new("Measuring recall needs a primary key to compare results"));
    }
    let k = args.k.clamp(1, MAX_RECALL_K);
    let queries = args.queries.clamp(1, MAX_RECALL_QUERIES);
    let probes: Vec<Vec<f32>> = match args.seed {
        Some(seed) => {
            let pool = store.sample(&table, &column, RECALL_POOL, SampleOrder::ByKey(&key)).await?;
            pick_probes(pool.into_iter().flatten().collect(), queries as usize, seed)
        }
        None => store
            .sample(&table, &column, queries, SampleOrder::Random)
            .await?
            .into_iter()
            .flatten()
            .collect(),
    };
    if probes.is_empty() {
        return Err(AppError::new("The table has no vectors to sample"));
    }
    let mut recalls = Vec::with_capacity(probes.len());
    let (mut ann_total, mut exact_total) = (0.0, 0.0);
    for probe in &probes {
        let request = |exact| NearestRequest {
            column: &column,
            key: &key,
            index: &index,
            query: probe,
            k,
            exact,
        };
        let started = Instant::now();
        let approximate = store.nearest(&table, &request(args.self_check)).await?;
        ann_total += started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let exact = store.nearest(&table, &request(true)).await?;
        exact_total += started.elapsed().as_secs_f64() * 1000.0;
        recalls.push(recall_at_k(&approximate, &exact));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn found(rows: &[(&str, f64)]) -> Vec<Neighbour> {
        rows.iter()
            .map(|(key, distance)| Neighbour {
                key: (*key).to_owned(),
                distance: *distance,
            })
            .collect()
    }

    #[test]
    fn recall_counts_keys_and_ties() {
        let exact = found(&[("1", 0.0), ("2", 0.1), ("3", 0.2)]);
        assert_eq!(recall_at_k(&exact, &exact), 1.0);
        assert_eq!(recall_at_k(&found(&[("1", 0.0), ("9", 0.5), ("8", 0.6)]), &exact), 1.0 / 3.0);
        // Row 7 is as close as the exact third row: a tie, not a miss.
        assert_eq!(recall_at_k(&found(&[("1", 0.0), ("2", 0.1), ("7", 0.2)]), &exact), 1.0);
        assert_eq!(recall_at_k(&[], &exact), 0.0);
        assert_eq!(recall_at_k(&[], &[]), 1.0);
    }

    #[test]
    fn seeded_probes_repeat() {
        let pool: Vec<u32> = (0..50).collect();
        let first = pick_probes(pool.clone(), 5, 42);
        assert_eq!(first, pick_probes(pool.clone(), 5, 42));
        assert_ne!(first, pick_probes(pool.clone(), 5, 43));
        assert_eq!(first.iter().collect::<HashSet<_>>().len(), 5);
        assert_eq!(pick_probes(vec![1, 2], 5, 1).len(), 2);
        assert_eq!(SplitMix64::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
    }
}
