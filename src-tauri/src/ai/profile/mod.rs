//! "Learn this database": a stored profile of one database of one connection, built up front so the assistant does not
//! rediscover the schema in every chat.
//!
//! - [`model`]: the stored shape (version 1)
//! - [`build`]: the structural build through the engine contract (objects, columns, keys, relations), with progress
//!   and cancellation
//! - [`fingerprint`]: per-object and database fingerprints from metadata, and the cheap stale checks
//! - [`infer`]: relations inferred from column names
//! - [`redact`]: system and credential objects left out, sensitive columns and secret-looking text redacted
//! - [`store`]: the files, caps, refresh merge and cleanup
//! - [`digest`]: what a chat sees: the prompt digest and the `search_profile` / `get_object_profile` tools
//! - [`summary`]: the model's summary and object descriptions, on request, with a cost estimate first
//!
//! - [`extras`]: what an engine adds after the walk (MongoDB field paths and references, Elasticsearch mappings and
//!   aliases, Redis key patterns), its reads of data through the gateway as the assistant
//!
//! [`values`] reads value hints, only under the read gate. The structural walk reads no rows itself (document engines
//! sample documents to describe a collection); [`extras`] reads sampled documents and key names only through the
//! gateway, audited. Nothing is sent anywhere while learning; only [`summary`] sends anything to a model.

pub mod build;
pub mod digest;
pub mod extras;
pub mod fingerprint;
pub mod infer;
pub mod model;
pub mod redact;
pub mod store;
pub mod summary;
pub mod values;

use std::{collections::HashSet, sync::Arc};

use serde::Serialize;

pub use self::{
    build::{Progress, Step},
    extras::ExtrasOutcome,
    fingerprint::ObjectDiff,
    model::{DatabaseProfile, ProfileNote},
    store::{ProfileStatus, ProfileStore},
    values::{strip_value_hints, Sampler, ValuesOutcome},
};
use super::AiRuns;
use crate::{
    contract::{cancelled, CancellationToken},
    error::{AppError, AppResult},
    runtime::Handle,
};

/// Objects described in detail per profile; the others keep a name and a kind.
pub const MAX_DETAILED_OBJECTS: usize = 2_000;

/// The answer to a build (`profile_learn`).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnOutcome {
    pub status: ProfileStatus,
    /// Whether a stored profile was refreshed (false: the first build).
    pub refreshed: bool,
    pub diff: ObjectDiff,
    /// What value sampling did; none when it was not asked for or not allowed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<ValuesOutcome>,
    /// What the engine's extras added (field paths, references, mappings, key patterns); none for engines without them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<ExtrasOutcome>,
}

/// How a build may read data: through the gateway as the assistant on the connection (`sampler`'s context), for the
/// engine's extras (sampled documents, key names), and value hints only with `values` (the user lets the assistant read
/// data on this connection and the caller asked for hints).
pub struct BuildReads<'a> {
    pub sampler: Sampler<'a>,
    pub values: bool,
}

/// Builds (or refreshes) the profile of `database` on `handle`'s connection and stores it. `run_id` registers the
/// build with [`AiRuns`], so `ai_cancel(run_id)` stops it; `progress` hears each step. With a `sampler` (the user lets the
/// assistant read data on this connection and the caller asked for it) the build also runs the engine's extras reads
/// and reads value hints ([`values`]).
pub async fn learn(
    runs: &AiRuns,
    store: &Arc<ProfileStore>,
    handle: &Handle,
    database: &str,
    run_id: &str,
    sampler: Option<Sampler<'_>>,
    progress: build::ProgressFn<'_>,
) -> AppResult<LearnOutcome> {
    let reads = sampler.map(|sampler| BuildReads { sampler, values: true });
    learn_with(runs, store, handle, database, run_id, reads, true, progress).await
}

/// [`learn`] with the reads spelled out: `reads` none reads nothing but metadata. Without `keep_hints` (the user opted
/// out of value sampling for this build) the stored value hints are dropped instead of carried over for unchanged
/// objects, and the profile no longer says data was read.
#[allow(clippy::too_many_arguments)]
pub async fn learn_with(
    runs: &AiRuns,
    store: &Arc<ProfileStore>,
    handle: &Handle,
    database: &str,
    run_id: &str,
    reads: Option<BuildReads<'_>>,
    keep_hints: bool,
    progress: build::ProgressFn<'_>,
) -> AppResult<LearnOutcome> {
    let connection_id = handle.connection_id.clone();
    let _slot = store.begin_learning(&connection_id, database)?;
    let run = runs.begin(run_id);
    // `ai_cancel` fires the run's token; the build watches the contract's token, and stops its engine call within the
    // call's grace period.
    let token = CancellationToken::new();
    // A cancel that arrives while the build runs on (between engine calls, or from a progress listener) reaches the
    // build's token at its next step, not only when the select below gets to poll the run.
    let watched = |step: Progress| {
        progress(step);
        if run.is_cancelled() {
            token.cancel();
        }
    };
    let progress: build::ProgressFn<'_> = &watched;
    let building = async {
        let mut profile = build::build(handle, &connection_id, database, &token, progress).await?;
        let extras_reads = reads.as_ref().map(|reads| extras::ExtrasReads {
            sampler: &reads.sampler,
            values: reads.values,
        });
        let enriched = extras::enrich(handle, extras_reads, &mut profile, &token, progress).await?;
        let (extras, sampled) = match enriched {
            Some((outcome, sampled)) => (Some(outcome), sampled),
            None => (None, HashSet::new()),
        };
        let values = match reads.as_ref().filter(|reads| reads.values) {
            Some(reads) => Some(values::sample_except(&reads.sampler, handle, &mut profile, &sampled, &token, progress).await?),
            None => None,
        };
        Ok::<_, AppError>((profile, values, extras))
    };
    tokio::pin!(building);
    let built = tokio::select! {
        outcome = &mut building => outcome,
        _ = run.cancelled() => {
            token.cancel();
            building.await
        }
    };
    runs.finish(run_id);
    let (fresh, values, extras) = built.map_err(|error| if token.is_cancelled() { cancelled() } else { error })?;
    progress(Progress::new(Step::Saving, 0, 1));
    let writer = Arc::clone(store);
    let (profile, diff, refreshed) = tokio::task::spawn_blocking(move || {
        let existed = writer.header(&fresh.connection_id, &fresh.database)?.is_some();
        writer
            .refresh_with(fresh, keep_hints)
            .map(|(profile, diff)| (profile, diff, existed))
    })
    .await
    .map_err(|error| AppError::new(format!("Saving what the assistant learned failed: {error}")))??;
    progress(Progress::new(Step::Saving, 1, 1));
    let mut status = status_of(&profile);
    status.stale = Some(false);
    status.checked_at = Some(store.remember_stale(&profile.connection_id, &profile.database, false));
    Ok(LearnOutcome {
        status,
        refreshed,
        diff,
        values,
        extras,
    })
}

fn status_of(profile: &DatabaseProfile) -> ProfileStatus {
    ProfileStatus {
        exists: true,
        learned_at: Some(profile.learned_at),
        objects: profile.total_objects,
        detailed_objects: profile.detailed_objects,
        data_read: profile.data_read,
        stale: None,
        checked_at: None,
    }
}

/// Whether a profile exists and whether the structure changed since it was built. The check lists the database again
/// (and reads its schema graph where the engine has one), without describing objects; its answer is reused for
/// [`store::STALE_CHECK_TTL`] unless `force`. Without an open connection (`handle` none) staleness is unknown.
pub async fn status(
    store: &Arc<ProfileStore>,
    handle: Option<&Handle>,
    connection_id: &str,
    database: &str,
    force: bool,
) -> AppResult<ProfileStatus> {
    let reader = Arc::clone(store);
    let (id, name) = (connection_id.to_owned(), database.to_owned());
    let header = tokio::task::spawn_blocking(move || reader.header(&id, &name))
        .await
        .map_err(|error| AppError::new(format!("Reading what the assistant learned failed: {error}")))??;
    let Some(header) = header else {
        return Ok(ProfileStatus::default());
    };
    let mut status = ProfileStatus::of(&header);
    if !force {
        if let Some((checked_at, stale)) = store.cached_stale(connection_id, database) {
            status.stale = Some(stale);
            status.checked_at = Some(checked_at);
            return Ok(status);
        }
    }
    let Some(handle) = handle else {
        return Ok(status);
    };
    match build::checks(handle, database, &CancellationToken::new()).await {
        Ok(now) => {
            let graph_changed = matches!((&header.checks.graph, &now.graph), (Some(before), Some(after)) if before != after);
            let stale = now.listing != header.checks.listing || graph_changed;
            status.stale = Some(stale);
            status.checked_at = Some(store.remember_stale(connection_id, database, stale));
        }
        Err(error) => tracing::debug!(%error, "the profile's stale check failed"),
    }
    Ok(status)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod extras_tests;
