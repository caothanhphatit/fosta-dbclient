//! "Learn this database": build, read, check, annotate and delete what the assistant knows about a database
//! (`ai::profile`), and have the model summarise it (`profile_summary_estimate`, `profile_summarize`). Builds and summaries
//! stream `profile-progress` events and stop with `ai_cancel(runId)`; builds, refreshes, summaries and deletions are
//! written to the audit log. File access runs on the blocking pool.

use std::sync::Arc;

use serde::Serialize;
use serde_json::json;

use crate::{
    ai::{
        profile::{
            self,
            summary::{self, Completer, ModelCompleter, SummaryEstimate, SummaryOptions, SummaryOutcome},
            DatabaseProfile, LearnOutcome, ProfileNote, ProfileStatus, ProfileStore, Progress,
        },
        AiConnection, AiProvider,
    },
    audit::{AuditEntry, Outcome},
    error::{AppError, AppResult},
    gateway::{
        assistant::{reads_allowed, AssistantQueries},
        Context,
    },
    secrets, AppState,
};

/// The event a build reports its steps with.
pub const PROGRESS_EVENT: &str = "profile-progress";

/// Builds (or refreshes) the profile of `database` on the open connection `connection_id`. Nothing is sent to a model.
/// Data is read only where the user lets the assistant read data on this connection
/// (`gateway::assistant::reads_allowed`): then the engine's extras read through the gateway as the assistant (MongoDB
/// `mongo.schema` samples, a Redis SCAN of key names), and value hints are read unless `sample_values` is false. Without
/// reads the build is metadata only (the structural walk, MongoDB collection options and indexes, Elasticsearch
/// mappings and aliases). The reads are audited as one summary entry per phase, failures and refusals one by one. Value
/// hints an earlier build stored are dropped whenever this build may not sample values (reads off now, or opted out).
pub async fn profile_learn(
    state: &AppState,
    connection_id: String,
    database: String,
    run_id: String,
    sample_values: Option<bool>,
) -> AppResult<LearnOutcome> {
    ensure_learning_allowed(state, &connection_id)?;
    let handle = state.engines.handle(&connection_id)?;
    let context = read_context(state, &connection_id);
    let values = context.is_some() && sample_values.unwrap_or(true);
    let reads = context.map(|context| profile::BuildReads {
        sampler: profile::Sampler {
            manager: &state.engines,
            context,
        },
        values,
    });
    // Hints are kept only while this build may sample them.
    let keep_hints = values;
    let progress = progress_emitter(state, &run_id, &connection_id, &database);
    // Read before the build: DDL run while it reads makes the cached "up to date" answer unusable.
    let schema_epoch = state.engines.gateway.schema_epoch(&connection_id);
    let mut outcome = profile::learn_with(
        &state.ai,
        &state.profiles,
        &handle,
        &database,
        &run_id,
        reads,
        keep_hints,
        &progress,
    )
    .await?;
    outcome.status.checked_at = Some(state.profiles.remember_stale(&connection_id, &database, false, schema_epoch));
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

/// Reports a run's steps as `profile-progress` events.
fn progress_emitter(state: &AppState, run_id: &str, connection_id: &str, database: &str) -> impl Fn(Progress) + Send + Sync {
    let events = Arc::clone(&state.events);
    let (run, connection, name) = (run_id.to_owned(), connection_id.to_owned(), database.to_owned());
    move |progress: Progress| {
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
    }
}

/// What a summary run would cost (`profile_summary_estimate`): the estimate from the prompts it would send, the model
/// and provider it would use, and the price where the provider settings know one (none carry prices today: `null`).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryCost {
    #[serde(flatten)]
    pub estimate: SummaryEstimate,
    pub model: String,
    pub provider: AiProvider,
    pub price: Option<f64>,
}

/// The LLM connection and model a summary uses: the chat's active ones, `model` instead of the active model when given.
fn summary_settings(state: &AppState, model: Option<String>) -> AppResult<(AiConnection, String)> {
    state.config.read(|config| {
        let active = config
            .ai_active
            .as_ref()
            .ok_or_else(|| AppError::new("Add an LLM provider first (Assistant → Set up…)"))?;
        let connection = config.ai_connection(&active.connection_id)?.clone();
        let model = model
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty())
            .or_else(|| Some(active.model.trim().to_owned()).filter(|model| !model.is_empty()))
            .unwrap_or_else(|| connection.default_model.clone());
        Ok((connection, model))
    })
}

async fn stored(state: &AppState, connection_id: &str, database: &str) -> AppResult<DatabaseProfile> {
    let (id, name) = (connection_id.to_owned(), database.to_owned());
    with_store(state, move |store| store.load(&id, &name))
        .await?
        .ok_or_else(|| AppError::new("The assistant has not learned this database yet"))
}

/// The cost of summarising the stored profile of `database` (`only_changed`: only objects without a description or
/// whose structure changed). Nothing is sent.
pub async fn profile_summary_estimate(
    state: &AppState,
    connection_id: String,
    database: String,
    model: Option<String>,
    only_changed: Option<bool>,
) -> AppResult<SummaryCost> {
    let (connection, model) = summary_settings(state, model)?;
    let profile = stored(state, &connection_id, &database).await?;
    let reads = read_context(state, &connection_id).is_some();
    Ok(SummaryCost {
        estimate: summary::estimate(&profile, reads, only_changed.unwrap_or(false)),
        model,
        provider: connection.provider,
        price: None,
    })
}

/// Has the model write the database summary and one-line object descriptions of the stored profile of `database`
/// (about 50 objects per request, then the summary), with the chat's active LLM connection and `model` (default: the
/// active model). The structure and the user's notes are sent; value hints only when the profile has them and the user
/// lets the assistant read data on this connection now. The user's descriptions are kept. Streams `profile-progress`
/// (step `summary`), stops with `ai_cancel(runId)` keeping the requests already answered, and is written to the audit
/// log with the provider, the model, the objects, the tokens and whether data was sent.
pub async fn profile_summarize(
    state: &AppState,
    connection_id: String,
    database: String,
    run_id: String,
    model: Option<String>,
    only_changed: bool,
) -> AppResult<SummaryOutcome> {
    ensure_learning_allowed(state, &connection_id)?;
    let (connection, model) = summary_settings(state, model)?;
    let completer = ModelCompleter::new(connection.settings(&model), secrets::load_ai_key(&connection.id)?)?;
    summarize_with(state, &connection, &completer, connection_id, database, run_id, model, only_changed).await
}

#[allow(clippy::too_many_arguments)]
async fn summarize_with(
    state: &AppState,
    llm: &AiConnection,
    completer: &dyn Completer,
    connection_id: String,
    database: String,
    run_id: String,
    model: String,
    only_changed: bool,
) -> AppResult<SummaryOutcome> {
    let options = SummaryOptions {
        reads_allowed: read_context(state, &connection_id).is_some(),
        only_changed,
        model,
    };
    let progress = progress_emitter(state, &run_id, &connection_id, &database);
    let (outcome, result) = summary::summarize(
        &state.ai,
        &state.profiles,
        &connection_id,
        &database,
        &run_id,
        completer,
        &options,
        &progress,
    )
    .await;
    // A run that stopped part way is audited too when it sent anything.
    if result.is_err() && outcome.requests == 0 {
        return result.map(|()| outcome);
    }
    let engine = state
        .config
        .read(|config| config.connection(&connection_id).map(|profile| profile.engine.clone()))
        .unwrap_or_default();
    let ending = match &result {
        Ok(()) => String::new(),
        Err(error) => format!("; stopped: {error}"),
    };
    audit(
        state,
        &connection_id,
        &engine,
        format!("Summarised {} with the model", shown(&database)),
        format!(
            "provider {}, model {}, {} objects ({} described), {} requests, about {} input and {} output tokens, data read: {}{ending}",
            llm.provider.display_name(),
            outcome.model,
            outcome.objects,
            outcome.described,
            outcome.requests,
            outcome.input_tokens,
            outcome.output_tokens,
            if outcome.data_read { "yes" } else { "no" }
        ),
    );
    result.map(|()| outcome)
}

/// Whether the assistant may learn databases: the workspace's override wins, else the global setting.
pub fn learning_allowed(global: bool, workspace: AssistantQueries) -> bool {
    match workspace {
        AssistantQueries::Allow => true,
        AssistantQueries::Deny => false,
        AssistantQueries::Default => global,
    }
}

/// Refuses learning (a build or a summary) on `connection_id` where the user turned it off: Settings → "Assistant may
/// learn databases", or the workspace's override. A connection outside every workspace follows the global setting.
fn ensure_learning_allowed(state: &AppState, connection_id: &str) -> AppResult<()> {
    let (global, workspace) = state.config.read(|config| {
        let workspace = config.connection_scope(connection_id).map(|(workspace, _)| workspace);
        (
            config.assistant_learn,
            workspace.map(|workspace| (workspace.name.clone(), workspace.assistant_learn)),
        )
    });
    let choice = workspace.as_ref().map(|(_, choice)| *choice).unwrap_or_default();
    if learning_allowed(global, choice) {
        return Ok(());
    }
    Err(AppError::new(match (choice, workspace) {
        (AssistantQueries::Deny, Some((name, _))) => {
            format!("The assistant may not learn databases in the {name} workspace (Edit workspace → Assistant may learn databases)")
        }
        _ => "The assistant may not learn databases: turn on Settings → Assistant may learn databases".to_owned(),
    }))
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
/// minutes, unless `force` or a statement that may change the schema ran on it through the app since).
pub async fn profile_status(state: &AppState, connection_id: String, database: String, force: Option<bool>) -> AppResult<ProfileStatus> {
    let handle = state.engines.handle(&connection_id).ok();
    let schema_epoch = state.engines.gateway.schema_epoch(&connection_id);
    profile::status(
        &state.profiles,
        handle.as_ref(),
        &connection_id,
        &database,
        schema_epoch,
        force.unwrap_or(false),
    )
    .await
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

    struct Echo;

    #[async_trait::async_trait]
    impl Completer for Echo {
        async fn complete(&self, system: &str, user: &str) -> AppResult<summary::Completion> {
            let text = if user.contains("Objects:\n") {
                "{\"object\": \"items\", \"description\": \"Things for sale.\"}".to_owned()
            } else {
                "{\"summary\": \"A tiny shop.\"}".to_owned()
            };
            Ok(summary::Completion {
                input_tokens: summary::estimate_tokens(system) + summary::estimate_tokens(user),
                output_tokens: summary::estimate_tokens(&text),
                text,
            })
        }
    }

    #[tokio::test]
    async fn the_summary_commands_estimate_summarise_and_audit() {
        let mut state = AppState::for_test("profile-summary", crate::Capabilities::desktop(), AppConfig::default());
        let audit_path = std::env::temp_dir().join(format!("fosta-profile-summary-audit-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&audit_path);
        state.engines.gateway = crate::gateway::Gateway::with_audit(crate::audit::AuditLog::new(audit_path.clone(), 100));
        let engine = FakeEngine::leak();
        state.engines.insert("c", engine, engine.connection());
        let no_provider = profile_summary_estimate(&state, "c".into(), "shop".into(), None, None).await;
        assert!(no_provider.unwrap_err().to_string().contains("LLM provider"));
        let llm = AiConnection {
            id: "llm".into(),
            name: "LLM".into(),
            provider: AiProvider::Anthropic,
            base_url: String::new(),
            default_model: "big-model".into(),
        };
        state
            .config
            .update(|config| {
                config.put_ai_connection(llm.clone());
                config.set_ai_active("llm", "big-model")
            })
            .unwrap();
        // Nothing learned yet.
        assert!(profile_summary_estimate(&state, "c".into(), "shop".into(), None, None)
            .await
            .is_err());
        profile_learn(&state, "c".into(), "shop".into(), "run".into(), Some(false))
            .await
            .unwrap();
        let cost = profile_summary_estimate(&state, "c".into(), "shop".into(), Some("small-model".into()), None)
            .await
            .unwrap();
        assert_eq!(
            (cost.model.as_str(), cost.provider, cost.price),
            ("small-model", AiProvider::Anthropic, None)
        );
        assert_eq!((cost.estimate.objects, cost.estimate.requests), (1, 2));
        let json = serde_json::to_value(&cost).unwrap();
        assert!(json["inputTokens"].as_u64().unwrap() > 0 && json["price"].is_null(), "{json}");
        assert_eq!(
            summary_settings(&state, None).unwrap().1,
            "big-model",
            "the active model by default"
        );
        let outcome = summarize_with(
            &state,
            &llm,
            &Echo,
            "c".into(),
            "shop".into(),
            "s".into(),
            "small-model".into(),
            false,
        )
        .await
        .unwrap();
        assert_eq!((outcome.described, outcome.summary.as_deref()), (1, Some("A tiny shop.")));
        let stored = profile_get(&state, "c".into(), "shop".into()).await.unwrap().unwrap();
        assert_eq!(
            stored.object("items").unwrap().description.as_ref().unwrap().text,
            "Things for sale."
        );
        assert_eq!(stored.model.as_deref(), Some("small-model"));
        let audit = crate::audit::AuditLog::new(audit_path.clone(), 100).read_all().unwrap();
        let entry = audit.iter().find(|entry| entry.statement.starts_with("Summarised")).unwrap();
        assert_eq!(entry.category, "profile");
        assert!(
            entry
                .detail
                .contains("provider Claude, model small-model, 1 objects (1 described), 2 requests"),
            "{}",
            entry.detail
        );
        assert!(entry.detail.contains("data read: no"));
        let _ = std::fs::remove_file(audit_path);
    }

    fn save_doc_connection(state: &AppState, reads: AssistantQueries) {
        state
            .config
            .update(|config| {
                config.workspaces[0].connections.retain(|connection| connection.id != "doc");
                config.workspaces[0].connections.push(crate::config::ConnectionProfile {
                    id: "doc".into(),
                    name: "Doc".into(),
                    engine: "fakedoc".into(),
                    ..Default::default()
                });
                config.workspaces[0].assistant_queries = reads;
                Ok(())
            })
            .unwrap();
    }

    #[tokio::test]
    async fn the_extras_read_only_where_the_assistant_may_read_data() {
        let state = AppState::for_test("profile-extras-gate", crate::Capabilities::desktop(), AppConfig::default());
        let doc = crate::ai::profile::extras_tests::insert_doc(&state.engines, "doc");
        save_doc_connection(&state, AssistantQueries::Deny);
        let denied = profile_learn(&state, "doc".into(), "shop".into(), "run".into(), None)
            .await
            .unwrap();
        assert!(doc.invoked.lock().unwrap().is_empty(), "no sample, no key names");
        let extras = denied.extras.unwrap();
        assert_eq!(extras.reads, 0);
        assert_eq!(extras.skipped.as_deref(), Some(crate::ai::profile::extras::NO_READS));
        assert!(denied.values.is_none() && !denied.status.data_read);
        let stored = profile_get(&state, "doc".into(), "shop".into()).await.unwrap().unwrap();
        assert!(stored.object("session:{id}").is_none(), "no key pattern without reads");
        // Allowed: the extras read through the gateway and keep value hints.
        save_doc_connection(&state, AssistantQueries::Allow);
        let allowed = profile_learn(&state, "doc".into(), "shop".into(), "run".into(), None)
            .await
            .unwrap();
        assert!(!doc.invoked.lock().unwrap().is_empty());
        assert!(allowed.status.data_read);
        let stored = profile_get(&state, "doc".into(), "shop".into()).await.unwrap().unwrap();
        assert!(stored.objects.iter().any(|object| !object.value_hints.is_empty()));
        // Reads revoked again: a refresh drops the stored hints and no longer says data was read.
        save_doc_connection(&state, AssistantQueries::Deny);
        let revoked = profile_learn(&state, "doc".into(), "shop".into(), "run".into(), None)
            .await
            .unwrap();
        assert!(!revoked.status.data_read);
        let stored = profile_get(&state, "doc".into(), "shop".into()).await.unwrap().unwrap();
        assert!(stored.objects.iter().all(|object| object.value_hints.is_empty()));
        assert!(!stored.data_read);
    }

    #[tokio::test]
    async fn learning_follows_the_setting_and_the_workspace_override() {
        assert!(learning_allowed(true, AssistantQueries::Default));
        assert!(!learning_allowed(false, AssistantQueries::Default));
        assert!(learning_allowed(false, AssistantQueries::Allow));
        assert!(!learning_allowed(true, AssistantQueries::Deny));
        let state = AppState::for_test("profile-learn-setting", crate::Capabilities::desktop(), AppConfig::default());
        let engine = FakeEngine::leak();
        state.engines.insert("c", engine, engine.connection());
        assert!(state.config.read(|config| config.assistant_learn), "on unless turned off");
        crate::commands::ai::set_assistant_learn(&state, false).unwrap();
        let refused = profile_learn(&state, "c".into(), "shop".into(), "run".into(), None).await;
        assert!(refused.unwrap_err().to_string().contains("Assistant may learn databases"));
        let summary = profile_summarize(&state, "c".into(), "shop".into(), "s".into(), None, false).await;
        assert!(summary.unwrap_err().to_string().contains("Assistant may learn databases"));
        assert!(
            profile_get(&state, "c".into(), "shop".into()).await.unwrap().is_none(),
            "nothing was built"
        );
        crate::commands::ai::set_assistant_learn(&state, true).unwrap();
        profile_learn(&state, "c".into(), "shop".into(), "run".into(), None).await.unwrap();
        // A workspace that denies it wins over the global setting.
        state
            .config
            .update(|config| {
                config.workspaces[0].connections.push(crate::config::ConnectionProfile {
                    id: "saved".into(),
                    name: "Saved".into(),
                    engine: "fake".into(),
                    ..Default::default()
                });
                config.workspaces[0].assistant_learn = AssistantQueries::Deny;
                Ok(())
            })
            .unwrap();
        let denied = ensure_learning_allowed(&state, "saved").unwrap_err().to_string();
        assert!(denied.contains("workspace"), "{denied}");
        assert!(
            ensure_learning_allowed(&state, "c").is_ok(),
            "outside every workspace: the global setting"
        );
    }
}
