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
//!
//! Later phases fill fields that are already in the model: value hints (P4, under the read gate), the model's summary
//! and descriptions (P5), engine extras (P6). The structural build reads no rows and sends nothing anywhere.

pub mod build;
pub mod fingerprint;
pub mod infer;
pub mod model;
pub mod redact;
pub mod store;

use std::sync::Arc;

use serde::Serialize;

pub use self::{
    build::{Progress, Step},
    fingerprint::ObjectDiff,
    model::{DatabaseProfile, ProfileNote},
    store::{ProfileStatus, ProfileStore},
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
}

/// Builds (or refreshes) the profile of `database` on `handle`'s connection and stores it. `run_id` registers the
/// build with [`AiRuns`], so `ai_cancel(run_id)` stops it; `progress` hears each step.
pub async fn learn(
    runs: &AiRuns,
    store: &Arc<ProfileStore>,
    handle: &Handle,
    database: &str,
    run_id: &str,
    progress: build::ProgressFn<'_>,
) -> AppResult<LearnOutcome> {
    let connection_id = handle.connection_id.clone();
    let _slot = store.begin_learning(&connection_id, database)?;
    let run = runs.begin(run_id);
    // `ai_cancel` fires the run's token; the build watches the contract's token, and stops its engine call within the
    // call's grace period.
    let token = CancellationToken::new();
    let building = build::build(handle, &connection_id, database, &token, progress);
    tokio::pin!(building);
    let built = tokio::select! {
        outcome = &mut building => outcome,
        _ = run.cancelled() => {
            token.cancel();
            building.await
        }
    };
    runs.finish(run_id);
    let fresh = built.map_err(|error| if token.is_cancelled() { cancelled() } else { error })?;
    progress(Progress::new(Step::Saving, 0, 1));
    let writer = Arc::clone(store);
    let (profile, diff, refreshed) = tokio::task::spawn_blocking(move || {
        let existed = writer.header(&fresh.connection_id, &fresh.database)?.is_some();
        writer.refresh(fresh).map(|(profile, diff)| (profile, diff, existed))
    })
    .await
    .map_err(|error| AppError::new(format!("Saving what the assistant learned failed: {error}")))??;
    progress(Progress::new(Step::Saving, 1, 1));
    let mut status = status_of(&profile);
    status.stale = Some(false);
    status.checked_at = Some(store.remember_stale(&profile.connection_id, &profile.database, false));
    Ok(LearnOutcome { status, refreshed, diff })
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
