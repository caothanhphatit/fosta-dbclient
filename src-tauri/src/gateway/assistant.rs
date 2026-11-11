//! The assistant's read-only query tool (`run_read_query`): one statement, classified by its engine in the engine's
//! default language and authorized as [`Caller::Assistant`] (Read only, never a confirmation), then run in a session of
//! its own that is opened read-only, with at most [`MAX_ROWS`] rows and a [`TIMEOUT`]. Every run and every refusal goes
//! to the audit log and the application log.
//!
//! Whether the tool exists at all is the user's choice ([`reads_allowed`]): Settings → "Allow the assistant to run
//! read-only queries", overridden per workspace. When it is off, the tool is not offered to the model.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use super::{
    audit_entry,
    classify::Category,
    rules::{Preset, SafetySettings, PRODUCTION_ICON},
    Caller, Context, Gateway, Request, BLOCKED,
};
use crate::{
    audit::Outcome,
    contract::{
        require, unsupported_capability, CallContext, Canceller, Capability, ClassifyInput, ColumnInfo, Engine, Flow, ResultSink,
        RunOptions, RunRequest, Session, SessionOptions, StreamEvent, Value,
    },
    error::{AppError, AppResult},
    runtime::ConnectionManager,
};

/// Rows the assistant reads per run.
pub const MAX_ROWS: usize = 50;
/// How long one assistant run may take.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The per-workspace override of the global setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AssistantQueries {
    /// Follow the global setting (off for Read-only and Production workspaces).
    #[default]
    Default,
    Allow,
    Deny,
}

/// Whether the assistant may run read-only queries on a connection: the workspace override wins; otherwise the global
/// setting, which a Read-only preset (the connection's own rules or the workspace's) or a Production workspace turns off.
pub fn reads_allowed(global: bool, workspace: AssistantQueries, safety: &SafetySettings, workspace_icon: &str) -> bool {
    match workspace {
        AssistantQueries::Allow => true,
        AssistantQueries::Deny => false,
        AssistantQueries::Default => global && safety.preset != Preset::ReadOnly && workspace_icon != PRODUCTION_ICON,
    }
}

/// Why the assistant was refused, worded for the model: it must hand the statement to the user instead.
pub(super) fn refusal_message(worst: Option<Category>, workspace: &str) -> String {
    match worst {
        Some(Category::Read) | None => format!(
            "Refused: the rules of workspace {workspace} ask for a confirmation before reads, which only the user can give. \
             Put the statement in a fenced code block instead so the user can review it and press Run."
        ),
        Some(category) => format!(
            "Refused: the assistant may only run read-only statements, and this is a {} statement. Put it in a fenced \
             code block instead so the user can review it and press Run.",
            category.label().to_lowercase()
        ),
    }
}

/// The check behind [`read`], without running anything: one non-empty statement that the engine classifies as a Read
/// in `language`, allowed for [`Caller::Assistant`] by the rules. Refusals are audited.
pub fn check(gateway: &Gateway, engine: &dyn Engine, context: &Context, language: &str, text: &str) -> AppResult<()> {
    if text.trim().is_empty() {
        return Err(AppError::new("Nothing to run: pass one statement in `query`."));
    }
    let classification = engine.classify(&ClassifyInput::Statement { language, text });
    let request = Request {
        caller: Caller::Assistant,
        context,
        engine: engine.id(),
        scope: &context.connection_id,
        text,
        classification: &classification,
        confirmation: None,
        reusable: false,
        client: None,
    };
    if classification.statements.is_empty() {
        return Err(AppError::new("Nothing to run: pass one statement in `query`."));
    }
    if classification.statements.len() > 1 && classification.refusal().is_none() {
        return Err(gateway.block(
            &request,
            "Refused: the assistant runs one statement at a time. Send each statement in its own call.".to_owned(),
        ));
    }
    gateway.authorize(&request)
}

/// What one assistant run read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadResult {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<Value>>,
    /// More rows were there than [`MAX_ROWS`] (or the rules' row limit).
    pub truncated: bool,
    /// Rows affected, for a statement without a result set (always none for a read; kept for the record).
    pub affected: Option<u64>,
    pub elapsed_ms: u64,
}

/// Keeps the first result set, at most `max_rows` rows; later ones (and notices) are not part of the answer.
struct Collect {
    result: ReadResult,
    sets: usize,
    max_rows: usize,
}

impl ResultSink for Collect {
    fn event(&mut self, event: StreamEvent) -> Flow {
        match event {
            StreamEvent::Columns { columns, .. } => {
                self.sets += 1;
                if self.sets == 1 {
                    self.result.columns = columns;
                }
            }
            StreamEvent::Rows { rows, .. } if self.sets <= 1 => {
                let room = self.max_rows.saturating_sub(self.result.rows.len());
                if rows.len() > room {
                    self.result.truncated = true;
                }
                self.result.rows.extend(rows.into_iter().take(room));
            }
            StreamEvent::Done { truncated, affected, .. } if self.sets <= 1 => {
                self.result.truncated |= truncated;
                self.result.affected = affected;
            }
            _ => {}
        }
        if self.sets > 1 {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }
}

/// A session opened for one assistant run and registered nowhere. When the run future is dropped (the user pressed
/// Stop, the chat request was cancelled) the statement is cancelled and the session closed in the background.
struct OwnSession {
    session: Arc<dyn Session>,
    canceller: Option<Arc<dyn Canceller>>,
    finished: bool,
}

impl OwnSession {
    async fn close(mut self) {
        self.finished = true;
        self.session.close().await;
    }
}

impl Drop for OwnSession {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let (session, canceller) = (Arc::clone(&self.session), self.canceller.clone());
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(canceller) = canceller {
                    let _ = canceller.cancel().await;
                }
                session.close().await;
            });
        }
    }
}

/// How a run in a session of its own is capped.
pub(super) struct OwnRun {
    pub(super) max_rows: usize,
    pub(super) timeout: Duration,
    /// Open the session read-only and run the statement read-only (the assistant and `run_read_query`).
    pub(super) read_only: bool,
    /// Who asked, for the application log.
    pub(super) who: &'static str,
}

/// The engine's default query language (the one the assistant and MCP clients write in).
pub(super) fn default_language(engine: &dyn Engine) -> AppResult<String> {
    engine
        .descriptor()
        .default_language()
        .map(|language| language.id.clone())
        .ok_or_else(|| unsupported_capability("queries"))
}

/// Runs one already authorized statement in a session opened for it alone, within `run`'s caps (and the rules' own
/// row limit and timeout when they are lower). Nothing is audited here.
pub(super) async fn run_own_session(
    handle: &crate::runtime::Handle,
    context: &Context,
    language: &str,
    text: &str,
    database: Option<&str>,
    run: &OwnRun,
) -> AppResult<ReadResult> {
    let mut limits = context.limits();
    limits.read_only |= run.read_only;
    let options = SessionOptions {
        language: Some(language.to_owned()),
        database: database.map(str::to_owned),
    };
    let ctx = CallContext::new(limits.clone());
    let opened = handle
        .call("opened a session of its own", move |conn| async move {
            conn.open_session(&options, &ctx).await
        })
        .await?;
    let session = OwnSession {
        session: Arc::clone(&opened.session),
        canceller: opened.canceller.clone(),
        finished: false,
    };
    let rule_timeout = (limits.timeout_secs > 0).then(|| Duration::from_secs(u64::from(limits.timeout_secs)));
    let run_options = RunOptions {
        row_limit: run.max_rows.min(limits.max_rows.max(1)),
        timeout: Some(rule_timeout.map_or(run.timeout, |timeout| timeout.min(run.timeout))),
        read_only: limits.read_only,
        ..RunOptions::default()
    };
    let request = RunRequest {
        text: text.to_owned(),
        database: database.map(str::to_owned),
        offset: 0,
    };
    let started = Instant::now();
    let mut sink = Collect {
        result: ReadResult::default(),
        sets: 0,
        max_rows: run.max_rows,
    };
    let outcome = tokio::time::timeout(
        run.timeout,
        handle.run(Arc::clone(&opened.session), request, run_options, &mut sink),
    )
    .await;
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let outcome = match outcome {
        Ok(outcome) => {
            session.close().await;
            outcome.map(|_| ())
        }
        // Dropping the session cancels the statement and closes it in the background.
        Err(_) => {
            drop(session);
            Err(AppError::new(format!(
                "The query did not finish within {} seconds and was cancelled. Narrow it down (a WHERE, a LIMIT) or ask \
                 the user to run it.",
                run.timeout.as_secs()
            )))
        }
    };
    let mut result = sink.result;
    result.elapsed_ms = elapsed_ms;
    tracing::info!(
        connection = %context.connection_id,
        engine = handle.engine.id(),
        statement = %crate::logging::statement_excerpt(text),
        rows = result.rows.len(),
        truncated = result.truncated,
        ms = elapsed_ms,
        ok = outcome.is_ok(),
        "{} ran a query",
        run.who
    );
    outcome.map(|()| result)
}

/// The audit detail of a run: its row count, or why it failed.
pub(super) fn run_detail(outcome: &AppResult<ReadResult>) -> String {
    match outcome {
        Ok(result) => match result.affected {
            Some(affected) if result.columns.is_empty() => format!("{affected} row(s) affected"),
            _ => format!("{} row(s){}", result.rows.len(), if result.truncated { ", truncated" } else { "" }),
        },
        Err(error) => format!("failed: {error}"),
    }
}

/// Runs one read-only statement for the assistant on `context`'s connection (see the module docs). The run is audited
/// with its row count; refusals are audited by the check.
pub async fn read(mgr: &ConnectionManager, context: &Context, text: &str, database: Option<&str>) -> AppResult<ReadResult> {
    let handle = mgr.handle(&context.connection_id)?;
    require(handle.conn.capabilities(), Capability::Query)?;
    let language = default_language(handle.engine)?;
    check(&mgr.gateway, handle.engine, context, &language, text)?;
    let run = OwnRun {
        max_rows: MAX_ROWS,
        timeout: TIMEOUT,
        read_only: true,
        who: "the assistant",
    };
    let outcome = run_own_session(&handle, context, &language, text, database, &run).await;
    let classification = handle.engine.classify(&ClassifyInput::Statement { language: &language, text });
    mgr.gateway.audit(audit_entry(
        &Request {
            caller: Caller::Assistant,
            context,
            engine: handle.engine.id(),
            scope: &context.connection_id,
            text,
            classification: &classification,
            confirmation: None,
            reusable: false,
            client: None,
        },
        Outcome::Ran,
        &run_detail(&outcome),
    ));
    outcome
}

/// Runs one engine operation for the assistant (`sample_documents`: `mongo.find`, `mongo.aggregatePreview`): the engine
/// classifies it by name and arguments, and it is authorized as [`Caller::Assistant`] like [`read`] (Read only, never a
/// confirmation; unknown operations are refused whatever the rules). It runs read-only within [`TIMEOUT`], and the run is
/// audited; refusals are audited by the check.
pub async fn invoke_read(mgr: &ConnectionManager, context: &Context, name: &str, args: serde_json::Value) -> AppResult<serde_json::Value> {
    let handle = mgr.handle(&context.connection_id)?;
    let classification = handle.engine.classify(&ClassifyInput::Capability { name, args: &args });
    let binding = format!("{name}\u{0}{}", serde_json::to_string(&args)?);
    let request = Request {
        caller: Caller::Assistant,
        context,
        engine: handle.engine.id(),
        scope: &context.connection_id,
        text: &binding,
        classification: &classification,
        confirmation: None,
        reusable: false,
        client: None,
    };
    mgr.gateway.authorize(&request)?;
    let mut limits = context.limits();
    limits.read_only = true;
    let (operation, ctx) = (name.to_owned(), CallContext::new(limits));
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        TIMEOUT,
        handle.call("ran an operation for the assistant", |conn| async move {
            conn.invoke(&operation, args, &ctx).await
        }),
    )
    .await
    .unwrap_or_else(|_| {
        Err(AppError::new(format!(
            "The read did not finish within {} seconds. Narrow it down (a filter, fewer stages).",
            TIMEOUT.as_secs()
        )))
    });
    let detail = match &outcome {
        Ok(_) => format!("ran in {} ms", started.elapsed().as_millis()),
        Err(error) => format!("failed: {error}"),
    };
    mgr.gateway.audit(audit_entry(&request, Outcome::Ran, &detail));
    outcome
}

/// Whether `error` is the gateway refusing the statement (the model should hand it to the user).
pub fn is_refusal(error: &AppError) -> bool {
    error.code() == Some(BLOCKED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audit::AuditLog,
        contract::testing::FakeEngine,
        gateway::rules::{Action, Preset},
    };

    fn context(preset: Preset) -> Context {
        Context {
            workspace_id: "w".into(),
            workspace_name: "Team".into(),
            connection_id: "c".into(),
            connection_name: "Main DB".into(),
            safety: SafetySettings::preset(preset),
        }
    }

    #[test]
    fn the_setting_follows_the_workspace() {
        let open = SafetySettings::preset(Preset::Open);
        let read_only = SafetySettings::preset(Preset::ReadOnly);
        assert!(reads_allowed(true, AssistantQueries::Default, &open, "laptop"));
        assert!(!reads_allowed(false, AssistantQueries::Default, &open, "laptop"));
        assert!(!reads_allowed(true, AssistantQueries::Default, &read_only, "laptop"));
        assert!(!reads_allowed(true, AssistantQueries::Default, &open, PRODUCTION_ICON));
        assert!(reads_allowed(false, AssistantQueries::Allow, &read_only, PRODUCTION_ICON));
        assert!(!reads_allowed(true, AssistantQueries::Deny, &open, "laptop"));
    }

    #[test]
    fn only_single_allowed_reads_pass() {
        let path = std::env::temp_dir().join(format!("fosta-assistant-check-{}.jsonl", std::process::id()));
        let gateway = Gateway::with_audit(AuditLog::new(path.clone(), 1000));
        let engine = FakeEngine::leak();
        let open = context(Preset::Open);
        assert!(check(&gateway, engine, &open, "sql", "SELECT 1").is_ok());
        let write = check(&gateway, engine, &open, "sql", "UPDATE items SET name = 'x'").unwrap_err();
        assert!(is_refusal(&write));
        assert!(write.to_string().contains("code block"), "{write}");
        assert!(check(&gateway, engine, &open, "sql", "   ").is_err());
        // A Custom matrix that confirms reads cannot be answered by the model: refused, no token handed out.
        let mut custom = context(Preset::Custom);
        custom.safety.rules.read = Action::Confirm;
        let confirm = check(&gateway, engine, &custom, "sql", "SELECT 1").unwrap_err();
        assert!(is_refusal(&confirm), "{confirm:?}");
        assert!(confirm.payload().unwrap()["token"].is_null());
        let audit = AuditLog::new(path.clone(), 1000).read_all().unwrap();
        assert!(audit
            .iter()
            .all(|entry| entry.caller == "assistant" && entry.outcome == Some(Outcome::Blocked)));
        assert_eq!(audit.len(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_read_runs_through_the_engine_in_a_session_of_its_own() {
        let manager = ConnectionManager::default();
        let engine = FakeEngine::leak();
        manager.insert("c", engine, engine.connection());
        let result = read(&manager, &context(Preset::Guarded), "SELECT * FROM items", Some("shop"))
            .await
            .unwrap();
        assert!(!result.columns.is_empty(), "{result:?}");
        let calls = engine.calls.lock().unwrap().clone();
        assert!(
            calls.contains(&"open_session".to_owned()) && calls.contains(&"run".to_owned()),
            "{calls:?}"
        );
        let refused = read(&manager, &context(Preset::Open), "DELETE FROM items", None).await.unwrap_err();
        assert!(is_refusal(&refused));
        // The refused statement never reached a session.
        let runs = engine.calls.lock().unwrap().iter().filter(|call| *call == "run").count();
        assert_eq!(runs, 1);
    }
}
