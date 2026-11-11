//! "Learn this database": build, read, check, annotate and delete what the assistant knows about a database
//! (`ai::profile`). A build streams `profile-progress` events and stops with `ai_cancel(runId)`; builds, refreshes and
//! deletions are written to the audit log. File access runs on the blocking pool.

use std::sync::Arc;

use serde_json::json;

use crate::{
    ai::profile::{self, DatabaseProfile, LearnOutcome, ProfileNote, ProfileStatus, ProfileStore, Progress},
    audit::{AuditEntry, Outcome},
    error::{AppError, AppResult},
    gateway::{assistant::reads_allowed, Context},
    AppState,
};

/// The event a build reports its steps with.
pub const PROGRESS_EVENT: &str = "profile-progress";

/// Builds (or refreshes) the profile of `database` on the open connection `connection_id`. Nothing is sent to a model.
/// Rows are read only for value hints: when the user lets the assistant read data on this connection
/// (`gateway::assistant::reads_allowed`) and `sample_values` is not false (it defaults to that setting); those reads go
/// through the gateway as the assistant and are audited one by one.
pub async fn profile_learn(
    state: &AppState,
    connection_id: String,
    database: String,
    run_id: String,
    sample_values: Option<bool>,
) -> AppResult<LearnOutcome> {
    let handle = state.engines.handle(&connection_id)?;
    let sampler = read_context(state, &connection_id)
        .filter(|_| sample_values.unwrap_or(true))
        .map(|context| profile::Sampler {
            manager: &state.engines,
            context,
        });
    let events = Arc::clone(&state.events);
    let (run, connection, name) = (run_id.clone(), connection_id.clone(), database.clone());
    let progress = move |progress: Progress| {
        events.emit(
            PROGRESS_EVENT,
            json!({
                "runId": run,
                "connectionId": connection,
                "database": name,
                "step": progress.step,
                "done": progress.done,
                "total": progress.total,
                "object": progress.object,
            }),
        );
    };
    let outcome = profile::learn(&state.ai, &state.profiles, &handle, &database, &run_id, sampler, &progress).await?;
    let diff = &outcome.diff;
    let statement = if outcome.refreshed {
        format!(
            "Refreshed what the assistant knows about {} ({} added, {} changed, {} removed)",
            shown(&database),
            diff.added.len(),
            diff.changed.len(),
            diff.removed.len()
        )
    } else {
        format!("Learned the structure of {}", shown(&database))
    };
    audit(
        state,
        &connection_id,
        handle.engine.id(),
        statement,
        match &outcome.values {
            Some(values) => format!(
                "{} objects, {} columns sampled, {} value hints",
                outcome.status.objects, values.columns, values.hinted
            ),
            None => format!("{} objects", outcome.status.objects),
        },
    );
    Ok(outcome)
}

/// The assistant's gateway context on `connection_id` when the user lets it read data there.
fn read_context(state: &AppState, connection_id: &str) -> Option<Context> {
    state.config.read(|config| {
        let (workspace, _) = config.connection_scope(connection_id)?;
        let context = Context::for_connection(config, connection_id).ok()?;
        reads_allowed(
            config.assistant_read_queries,
            workspace.assistant_queries,
            &context.safety,
            &workspace.icon,
        )
        .then_some(context)
    })
}

/// The stored profile, if any.
pub async fn profile_get(state: &AppState, connection_id: String, database: String) -> AppResult<Option<DatabaseProfile>> {
    with_store(state, move |store| store.load(&connection_id, &database)).await
}

/// Whether a profile exists and whether the database changed since (checked on the open connection at most every ten
/// minutes, unless `force`).
pub async fn profile_status(state: &AppState, connection_id: String, database: String, force: Option<bool>) -> AppResult<ProfileStatus> {
    let handle = state.engines.handle(&connection_id).ok();
    profile::status(&state.profiles, handle.as_ref(), &connection_id, &database, force.unwrap_or(false)).await
}

/// Replaces the user's notes on a profile.
pub async fn profile_update_notes(
    state: &AppState,
    connection_id: String,
    database: String,
    notes: Vec<ProfileNote>,
) -> AppResult<DatabaseProfile> {
    with_store(state, move |store| store.update_notes(&connection_id, &database, notes)).await
}

/// Sets (or, with none, clears) the user's description of one learned object (`table` or `schema.table`).
pub async fn profile_update_description(
    state: &AppState,
    connection_id: String,
    database: String,
    object: String,
    description: Option<String>,
) -> AppResult<DatabaseProfile> {
    with_store(state, move |store| {
        store.update_description(&connection_id, &database, &object, description)
    })
    .await
}

/// Deletes a profile; false when there was none.
pub async fn profile_delete(state: &AppState, connection_id: String, database: String) -> AppResult<bool> {
    let (id, name) = (connection_id.clone(), database.clone());
    let deleted = with_store(state, move |store| store.delete(&id, &name)).await?;
    if deleted {
        let engine = state
            .config
            .read(|config| config.connection(&connection_id).map(|profile| profile.engine.clone()))
            .unwrap_or_default();
        audit(
            state,
            &connection_id,
            &engine,
            format!("Deleted what the assistant learned about {}", shown(&database)),
            String::new(),
        );
    }
    Ok(deleted)
}

/// Deletes the profiles of a deleted connection; a failure is logged, never stops the deletion.
pub fn forget_connection(state: &AppState, connection_id: &str) {
    if let Err(error) = state.profiles.remove_connection(connection_id) {
        tracing::warn!(%error, "could not delete the assistant's profiles of a deleted connection");
    }
}

fn shown(database: &str) -> String {
    if database.is_empty() {
        "this connection".to_owned()
    } else {
        database.to_owned()
    }
}

fn audit(state: &AppState, connection_id: &str, engine: &str, statement: String, detail: String) {
    let context = crate::commands::gateway_context(state, connection_id).ok();
    state.engines.gateway.audit(AuditEntry {
        caller: "assistant".to_owned(),
        workspace_id: context.as_ref().map(|c| c.workspace_id.clone()).unwrap_or_default(),
        workspace: context.as_ref().map(|c| c.workspace_name.clone()).unwrap_or_default(),
        connection_id: connection_id.to_owned(),
        connection: context.as_ref().map(|c| c.connection_name.clone()).unwrap_or_default(),
        engine: engine.to_owned(),
        category: "profile".to_owned(),
        statement,
        outcome: Some(Outcome::Ran),
        detail,
        ..AuditEntry::default()
    });
}

async fn with_store<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce(&ProfileStore) -> AppResult<T> + Send + 'static,
) -> AppResult<T> {
    let store = Arc::clone(&state.profiles);
    tokio::task::spawn_blocking(move || work(&store))
        .await
        .map_err(|error| AppError::new(format!("Reading what the assistant learned failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::{config::AppConfig, contract::testing::FakeEngine, events::EventEmitter};

    struct Recorder(Mutex<Vec<(String, serde_json::Value)>>);

    impl EventEmitter for Recorder {
        fn emit(&self, event: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event.to_owned(), payload));
        }
    }

    #[tokio::test]
    async fn the_commands_learn_read_annotate_and_delete_a_profile() {
        let mut state = AppState::for_test("profile-commands", crate::Capabilities::desktop(), AppConfig::default());
        let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
        state.events = recorder.clone();
        let engine = FakeEngine::leak();
        state.engines.insert("c", engine, engine.connection());
        assert!(!profile_status(&state, "c".into(), "shop".into(), None).await.unwrap().exists);
        assert!(profile_get(&state, "c".into(), "shop".into()).await.unwrap().is_none());
        let learned = profile_learn(&state, "c".into(), "shop".into(), "run".into(), None).await.unwrap();
        // The connection is not saved in a workspace that lets the assistant read data: no values are sampled.
        assert!(learned.values.is_none());
        assert!(!learned.refreshed);
        assert_eq!((learned.status.objects, learned.status.stale), (1, Some(false)));
        let events = recorder.0.lock().unwrap().clone();
        assert!(events
            .iter()
            .all(|(name, payload)| name == PROGRESS_EVENT && payload["runId"] == "run"));
        assert!(events
            .iter()
            .any(|(_, payload)| payload["step"] == "describing" && payload["object"] == "items"));
        let status = profile_status(&state, "c".into(), "shop".into(), Some(true)).await.unwrap();
        assert_eq!((status.exists, status.stale), (true, Some(false)));
        let note = ProfileNote {
            id: "n".into(),
            text: "Items are products".into(),
            object: Some("items".into()),
            created_at: 0,
            updated_at: 0,
        };
        profile_update_notes(&state, "c".into(), "shop".into(), vec![note]).await.unwrap();
        let described = profile_update_description(&state, "c".into(), "shop".into(), "items".into(), Some("Things".into()))
            .await
            .unwrap();
        assert_eq!(described.object("items").unwrap().description.as_ref().unwrap().text, "Things");
        let again = profile_learn(&state, "c".into(), "shop".into(), "run".into(), None).await.unwrap();
        assert!(again.refreshed && again.diff.is_empty());
        let stored = profile_get(&state, "c".into(), "shop".into()).await.unwrap().unwrap();
        assert_eq!(stored.notes.len(), 1);
        assert!(stored.object("items").unwrap().description.is_some());
        assert!(profile_delete(&state, "c".into(), "shop".into()).await.unwrap());
        assert!(!profile_delete(&state, "c".into(), "shop".into()).await.unwrap());
        // A connection that is not open cannot be learned, and its staleness is unknown.
        assert!(profile_learn(&state, "gone".into(), "shop".into(), "r".into(), None).await.is_err());
        profile_learn(&state, "c".into(), "shop".into(), "run".into(), None).await.unwrap();
        forget_connection(&state, "c");
        assert!(profile_get(&state, "c".into(), "shop".into()).await.unwrap().is_none());
    }
}
