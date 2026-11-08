//! The statement gateway: every statement the app sends to a database on behalf of a
//! caller — a query tab, a grid filter, a grid save, an import, "New database", or the
//! assistant — passes through here before it reaches a [`Driver`](crate::drivers::Driver).
//!
//! 1. The statement is classified ([`classify`]): SQL is parsed per dialect, MongoDB
//!    commands are classified by name. Grid filters must be a single condition ([`filter`]).
//! 2. The workspace's rules ([`rules`]) give an [`Action`] per category: allow, confirm,
//!    typed confirm or block. The assistant can only ever read metadata.
//! 3. A confirmation is a structured `CONFIRMATION_REQUIRED` error with a one-time token
//!    bound to the exact statement text and session; the UI asks the user and calls again
//!    with the token. Blocked and confirmed operations go to the local audit log.
//! 4. The rules also set engine-level backstops passed to the driver as [`Limits`]:
//!    read-only sessions, statement timeouts and the result row limit.

pub mod classify;
#[cfg(test)]
mod engine_tests;
pub mod filter;
pub mod rules;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use self::{
    classify::{classify_changes, Category, Classification, StatementInfo},
    filter::{validate_raw_filter, INVALID_FILTER},
    rules::{Action, SafetySettings},
};
use crate::{
    audit::{AuditEntry, AuditLog, Outcome},
    config::AppConfig,
    contract::{
        require, unsupported_capability, ApplyResult, BrowseRequest, CallContext, Capability, ClassifyInput, Filter, Limits, Page,
        ResultSink, RowChangeSet, RunRequest, RunSummary, TransactionOp, TransactionState,
    },
    drivers::types::{BrowseRequest as OldBrowseRequest, QueryOutcome, RowChange as OldRowChange, TablePage},
    error::{AppError, AppResult},
    lock::lock,
    runtime::{ConnectionManager, Handle},
};

pub const CONFIRMATION_REQUIRED: &str = "CONFIRMATION_REQUIRED";
pub const CONFIRMATION_INVALID: &str = "CONFIRMATION_INVALID";
pub const BLOCKED: &str = "BLOCKED";
const TOKEN_LIFETIME: Duration = Duration::from_secs(5 * 60);
const MAX_PENDING_TOKENS: usize = 64;

/// Who is sending a statement; each caller gets its own rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Caller {
    QueryTab,
    GridFilter,
    GridSave,
    Import,
    CreateDatabase,
    Assistant,
    /// Begin / Commit / Rollback / auto-commit from the query toolbar.
    Transaction,
    /// Changes to the rules themselves (audit only).
    Settings,
    /// An engine view calling an engine-specific operation (`invoke_capability`).
    EngineView,
}

impl Caller {
    fn id(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// The user's answer to a `CONFIRMATION_REQUIRED` error.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Confirmation {
    pub token: String,
    /// What the user typed, for typed confirmations.
    #[serde(default)]
    pub typed: Option<String>,
}

/// Where a statement runs: workspace and connection identity plus the rules in force.
#[derive(Clone, Debug)]
pub struct Context {
    pub workspace_id: String,
    pub workspace_name: String,
    pub connection_id: String,
    pub connection_name: String,
    pub safety: SafetySettings,
}

impl Context {
    /// The rules for a saved connection: its own override, else its workspace's.
    pub fn for_connection(config: &AppConfig, connection_id: &str) -> AppResult<Self> {
        let (workspace, profile) = config
            .connection_scope(connection_id)
            .ok_or_else(|| AppError::new(format!("Connection '{connection_id}' no longer exists")))?;
        Ok(Self {
            workspace_id: workspace.id.clone(),
            workspace_name: workspace.name.clone(),
            connection_id: profile.id.clone(),
            connection_name: profile.name.clone(),
            safety: profile.safety.clone().unwrap_or_else(|| workspace.safety()),
        })
    }

    /// The rules' engine-level backstops.
    pub fn limits(&self) -> Limits {
        let effective = self.safety.effective();
        Limits {
            max_rows: effective.max_rows as usize,
            timeout_secs: effective.statement_timeout_secs,
            read_only: effective.read_only(),
        }
    }
}

/// The payload of `CONFIRMATION_REQUIRED` and `BLOCKED` errors (`GatewayProblem` in `src/core/types.ts`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Problem<'a> {
    code: &'static str,
    caller: Caller,
    workspace: &'a str,
    connection: &'a str,
    preset: rules::Preset,
    /// `confirm` or `typed` for confirmations, `block` when blocked.
    level: Action,
    token: Option<String>,
    /// What the user must type for a typed confirmation.
    expected: Option<String>,
    categories: Vec<Category>,
    tables: Vec<String>,
    statements: &'a [StatementInfo],
}

#[derive(Clone)]
struct Pending {
    binding: String,
    expected: Option<String>,
    expires: Instant,
    /// Already used once (only reusable tokens survive their first use).
    used: bool,
}

struct Request<'a> {
    caller: Caller,
    context: &'a Context,
    /// The engine id, for the audit log.
    engine: &'a str,
    /// Session id for query tabs, connection id otherwise: tokens are bound to it.
    scope: &'a str,
    /// The exact statement text (or a canonical form of the grid changes).
    text: &'a str,
    classification: &'a Classification,
    confirmation: Option<&'a Confirmation>,
    /// One import run sends its rows in batches: its token is bound to the run (id, database,
    /// table) and stays valid for the following batches until it expires. Everything else is single-use.
    reusable: bool,
}

#[derive(Default)]
pub struct Gateway {
    tokens: Mutex<HashMap<String, Pending>>,
    audit: Mutex<Option<Arc<AuditLog>>>,
}

impl Gateway {
    #[cfg(test)]
    pub fn with_audit(log: AuditLog) -> Self {
        Self {
            tokens: Mutex::default(),
            audit: Mutex::new(Some(Arc::new(log))),
        }
    }

    /// Writes one audit entry; a failing disk never stops the operation.
    pub fn audit(&self, entry: AuditEntry) {
        let log = {
            let mut slot = lock(&self.audit);
            if slot.is_none() {
                *slot = AuditLog::open_default().ok().map(Arc::new);
            }
            slot.clone()
        };
        if let Some(log) = log {
            if let Err(error) = log.append(entry) {
                eprintln!("audit log: {error}");
            }
        }
    }

    fn authorize(&self, request: &Request<'_>) -> AppResult<()> {
        let effective = request.context.safety.effective();
        let statements = &request.classification.statements;
        // Hard refusals (an unknown engine operation, a command the console cannot run) hold whatever the rules say.
        if let Some(why) = request.classification.refusal() {
            return Err(self.block(request, format!("Refused: {why}.")));
        }
        if request.caller == Caller::QueryTab && !effective.allow_multi_statement && statements.len() > 1 {
            return Err(self.block(
                request,
                format!(
                    "Running several statements at once is turned off in workspace {}. Run them one at a time.",
                    request.context.workspace_name
                ),
            ));
        }
        let action = match request.caller {
            // The assistant never changes anything, whatever the rules say.
            Caller::Assistant if statements.iter().any(|statement| statement.category != Category::Read) => Action::Block,
            Caller::Transaction => Action::Allow,
            _ => statements
                .iter()
                .map(|statement| effective.rules.action(statement.category))
                .max()
                .unwrap_or(Action::Allow),
        };
        match action {
            Action::Allow => Ok(()),
            Action::Block => {
                let worst = worst_category(request.classification);
                let message = if request.caller == Caller::Assistant {
                    "The assistant cannot run statements that change anything.".to_owned()
                } else {
                    format!(
                        "Blocked: {} statements are not allowed by the {} rules of workspace {}. \
                         Change them under Edit workspace → Safety, or use a read-write connection elsewhere.",
                        worst.map(Category::label).unwrap_or("These"),
                        preset_label(request.context.safety.preset),
                        request.context.workspace_name
                    )
                };
                Err(self.block(request, message))
            }
            Action::Confirm | Action::Typed => self.confirm(request, action),
        }
    }

    fn block(&self, request: &Request<'_>, message: String) -> AppError {
        self.audit(audit_entry(request, Outcome::Blocked, &message));
        AppError::structured(
            message,
            Problem {
                code: BLOCKED,
                caller: request.caller,
                workspace: &request.context.workspace_name,
                connection: &request.context.connection_name,
                preset: request.context.safety.preset,
                level: Action::Block,
                token: None,
                expected: None,
                categories: request.classification.categories(),
                tables: request.classification.tables(),
                statements: &request.classification.statements,
            },
        )
    }

    fn confirm(&self, request: &Request<'_>, level: Action) -> AppResult<()> {
        let binding = binding(request);
        if let Some(confirmation) = request.confirmation {
            let now = Instant::now();
            let pending = {
                let mut tokens = lock(&self.tokens);
                let matches = tokens
                    .get(&confirmation.token)
                    .is_some_and(|pending| pending.binding == binding && pending.expires > now);
                if request.reusable && matches {
                    tokens.get_mut(&confirmation.token).map(|pending| {
                        let before = pending.clone();
                        pending.used = true;
                        pending.expires = now + TOKEN_LIFETIME;
                        before
                    })
                } else {
                    tokens.remove(&confirmation.token)
                }
            };
            let valid = pending
                .as_ref()
                .is_some_and(|pending| pending.binding == binding && pending.expires > now);
            let first_use = pending.as_ref().is_some_and(|pending| !pending.used);
            if !valid {
                return Err(AppError::structured(
                    "This confirmation is no longer valid: it is single-use and bound to the exact statement. Run it again to confirm anew.",
                    serde_json::json!({ "code": CONFIRMATION_INVALID }),
                ));
            }
            if let Some(expected) = pending.and_then(|pending| pending.expected) {
                let typed = confirmation.typed.as_deref().map(str::trim).unwrap_or_default();
                if typed != expected {
                    return Err(AppError::structured(
                        format!("The typed name does not match \"{expected}\". Nothing was run."),
                        serde_json::json!({ "code": CONFIRMATION_INVALID }),
                    ));
                }
            }
            if first_use {
                self.audit(audit_entry(request, Outcome::Confirmed, ""));
            }
            return Ok(());
        }
        let expected = (level == Action::Typed).then(|| expected_name(request));
        let token = new_token();
        {
            let mut tokens = lock(&self.tokens);
            let now = Instant::now();
            tokens.retain(|_, pending| pending.expires > now);
            if tokens.len() >= MAX_PENDING_TOKENS {
                tokens.clear();
            }
            tokens.insert(
                token.clone(),
                Pending {
                    binding,
                    expected: expected.clone(),
                    expires: now + TOKEN_LIFETIME,
                    used: false,
                },
            );
        }
        let worst = worst_category(request.classification).map(Category::label).unwrap_or("These");
        let message = match &expected {
            Some(name) => format!(
                "{worst} statements need a typed confirmation in workspace {}: type \"{name}\" to run them.",
                request.context.workspace_name
            ),
            None => format!(
                "{worst} statements need a confirmation in workspace {}.",
                request.context.workspace_name
            ),
        };
        Err(AppError::structured(
            message,
            Problem {
                code: CONFIRMATION_REQUIRED,
                caller: request.caller,
                workspace: &request.context.workspace_name,
                connection: &request.context.connection_name,
                preset: request.context.safety.preset,
                level,
                token: Some(token),
                expected,
                categories: request.classification.categories(),
                tables: request.classification.tables(),
                statements: &request.classification.statements,
            },
        ))
    }
}

fn worst_category(classification: &Classification) -> Option<Category> {
    classification.statements.iter().map(|statement| statement.category).max()
}

fn preset_label(preset: rules::Preset) -> &'static str {
    match preset {
        rules::Preset::Open => "Open",
        rules::Preset::Guarded => "Guarded",
        rules::Preset::ReadOnly => "Read-only",
        rules::Preset::Custom => "Custom",
    }
}

fn binding(request: &Request<'_>) -> String {
    format!(
        "{:?}\u{0}{}\u{0}{}\u{0}{}",
        request.caller, request.context.connection_id, request.scope, request.text
    )
}

/// A typed confirmation asks for the table name when one table is at stake, else the workspace name.
fn expected_name(request: &Request<'_>) -> String {
    let effective = request.context.safety.effective();
    let mut tables: Vec<String> = Vec::new();
    for statement in &request.classification.statements {
        if effective.rules.action(statement.category) != Action::Typed {
            continue;
        }
        for table in &statement.tables {
            let short = short_name(table);
            if !tables.contains(&short) {
                tables.push(short);
            }
        }
    }
    match tables.as_slice() {
        [table] if !table.is_empty() => table.clone(),
        _ => request.context.workspace_name.clone(),
    }
}

/// `shop.orders` → `orders`, `[sales].[Invoices]` → `Invoices`.
fn short_name(table: &str) -> String {
    table
        .rsplit('.')
        .next()
        .unwrap_or(table)
        .trim_matches(|c| c == '`' || c == '"' || c == '[' || c == ']')
        .to_owned()
}

fn new_token() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Extremely unlikely; fall back to time and address entropy (still single-use and bound).
        let seed = format!("{:?}{:p}", Instant::now(), &bytes);
        for (index, byte) in seed.bytes().enumerate() {
            bytes[index % 16] ^= byte;
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn audit_entry(request: &Request<'_>, outcome: Outcome, detail: &str) -> AuditEntry {
    let category = worst_category(request.classification)
        .and_then(|category| serde_json::to_value(category).ok())
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
    AuditEntry {
        time: 0,
        caller: request.caller.id(),
        workspace_id: request.context.workspace_id.clone(),
        workspace: request.context.workspace_name.clone(),
        connection_id: request.context.connection_id.clone(),
        connection: request.context.connection_name.clone(),
        engine: request.engine.to_owned(),
        category,
        statement: request
            .classification
            .statements
            .iter()
            .map(|statement| statement.excerpt.as_str())
            .collect::<Vec<_>>()
            .join("; "),
        outcome: Some(outcome),
        detail: detail.to_owned(),
    }
}

// --- Gated operations: what the commands call -------------------------------------------

/// A filter the gateway refused, in the audit log.
fn audit_filter(mgr: &ConnectionManager, context: &Context, engine: &str, text: &str, error: &AppError) {
    if error.code() == Some(INVALID_FILTER) {
        mgr.gateway.audit(AuditEntry {
            caller: Caller::GridFilter.id(),
            workspace_id: context.workspace_id.clone(),
            workspace: context.workspace_name.clone(),
            connection_id: context.connection_id.clone(),
            connection: context.connection_name.clone(),
            engine: engine.to_owned(),
            category: "filter".to_owned(),
            statement: text.to_owned(),
            outcome: Some(Outcome::Blocked),
            detail: error.to_string(),
            ..AuditEntry::default()
        });
    }
}

/// Checks a browse filter: the engine's own check, then raw text in the engine's filter language (one SQL condition, a
/// JSON object without code-running operators). Rejections are audited.
pub fn check_browse_filter(mgr: &ConnectionManager, context: &Context, handle: &Handle, filter: &Filter) -> AppResult<()> {
    let descriptor = handle.engine.descriptor();
    let checked = handle.engine.validate_filter(filter).and_then(|()| match filter {
        Filter::Raw { language, text } => validate_raw_filter(*language, descriptor.sql_dialect, text),
        _ => Ok(()),
    });
    checked.inspect_err(|error| {
        let text = match filter {
            Filter::Raw { text, .. } => text.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        audit_filter(mgr, context, handle.engine.id(), &text, error);
    })
}

/// Validates a raw filter typed for the old browse command or the vector search (one condition / JSON object).
pub fn check_filter(mgr: &ConnectionManager, context: &Context, connection_id: &str, filter: Option<&str>) -> AppResult<()> {
    let Some(text) = filter.filter(|text| !text.trim().is_empty()) else {
        return Ok(());
    };
    let handle = mgr.handle(connection_id)?;
    let descriptor = handle.engine.descriptor();
    let language = descriptor.filter.raw.ok_or_else(|| unsupported_capability("filters"))?;
    validate_raw_filter(language, descriptor.sql_dialect, text)
        .inspect_err(|error| audit_filter(mgr, context, handle.engine.id(), text, error))
}

/// `browse_object`: validates the filter, then reads a page with the rules' limits.
pub async fn browse(mgr: &ConnectionManager, context: &Context, connection_id: &str, request: &BrowseRequest) -> AppResult<Page> {
    let handle = mgr.handle(connection_id)?;
    require(handle.conn.capabilities(), Capability::BrowseRows)?;
    check_browse_filter(mgr, context, &handle, &request.filter)?;
    let (request, ctx) = (request.clone(), CallContext::new(context.limits()));
    handle
        .call("read a page of rows", |conn| async move {
            let tabular = conn.tabular().ok_or_else(|| unsupported_capability("browsing rows"))?;
            tabular.browse(&request, &ctx).await
        })
        .await
}

/// `apply_edits` from a grid save or an import: classified by the engine, confirmed per the rules, applied in one
/// transaction where the engine has them, each update and delete guarded by its concurrency check.
pub async fn apply_edits(
    mgr: &ConnectionManager,
    context: &Context,
    caller: Caller,
    set: &RowChangeSet,
    import_id: Option<&str>,
    confirmation: Option<&Confirmation>,
) -> AppResult<ApplyResult> {
    if !matches!(caller, Caller::GridSave | Caller::Import) {
        return Err(AppError::new("Row changes come from a grid save or an import"));
    }
    let handle = mgr.handle(&context.connection_id)?;
    require(handle.conn.capabilities(), Capability::EditRows)?;
    let classification = handle.engine.classify(&ClassifyInput::Changes(set));
    // An import run is confirmed once for all of its batches; a grid save for its exact changes.
    let import_run = import_id.filter(|id| caller == Caller::Import && !id.is_empty());
    let object = serde_json::to_string(&set.object)?;
    let text = match import_run {
        Some(id) => format!("{object}\u{0}import:{id}"),
        None => serde_json::to_string(set)?,
    };
    mgr.gateway.authorize(&Request {
        caller,
        context,
        engine: handle.engine.id(),
        scope: &context.connection_id,
        text: &text,
        classification: &classification,
        confirmation,
        reusable: import_run.is_some(),
    })?;
    let (set, ctx) = (set.clone(), CallContext::new(context.limits()));
    handle
        .call("saved row changes", |conn| async move {
            let tabular = conn.tabular().ok_or_else(|| unsupported_capability("editing rows"))?;
            tabular.apply(&set, &ctx).await
        })
        .await
}

/// `run` from a query tab: classified by the engine in the session's language, checked against the rules, then run with
/// the limits, streaming into `sink`.
pub async fn run(
    mgr: &ConnectionManager,
    context: &Context,
    session_id: &str,
    request: &RunRequest,
    confirmation: Option<&Confirmation>,
    sink: &mut dyn ResultSink,
) -> AppResult<RunSummary> {
    let (handle, session) = mgr.session(session_id)?;
    let classification = handle.engine.classify(&ClassifyInput::Statement {
        language: session.language(),
        text: &request.text,
    });
    let binding = format!("{}\u{0}{}", request.database.as_deref().unwrap_or_default(), request.text);
    mgr.gateway.authorize(&Request {
        caller: Caller::QueryTab,
        context,
        engine: handle.engine.id(),
        scope: session_id,
        text: &binding,
        classification: &classification,
        confirmation,
        reusable: false,
    })?;
    tracing::debug!(session = session_id, statement = %crate::logging::statement_excerpt(&request.text), "run");
    handle.run(session, request.clone(), context.limits().run_options(), sink).await
}

/// Begin / Commit / Rollback / auto-commit from the query toolbar: always allowed, even in a Read-only workspace (Begin
/// then opens a read-only transaction where the engine has one).
pub async fn transaction(mgr: &ConnectionManager, context: &Context, session_id: &str, op: TransactionOp) -> AppResult<TransactionState> {
    let (handle, session) = mgr.session(session_id)?;
    let classification = handle.engine.classify(&ClassifyInput::Transaction(op));
    mgr.gateway.authorize(&Request {
        caller: Caller::Transaction,
        context,
        engine: handle.engine.id(),
        scope: session_id,
        text: "",
        classification: &classification,
        confirmation: None,
        reusable: false,
    })?;
    let options = context.limits().run_options();
    handle
        .call("changed the transaction state", move |_| async move {
            session.transaction(op, &options).await
        })
        .await
}

/// "New database…" (DDL).
pub async fn create_database(
    mgr: &ConnectionManager,
    context: &Context,
    name: &str,
    first_collection: Option<&str>,
    confirmation: Option<&Confirmation>,
) -> AppResult<()> {
    let handle = mgr.handle(&context.connection_id)?;
    require(handle.conn.capabilities(), Capability::CreateDatabase)?;
    let classification = handle.engine.classify(&ClassifyInput::CreateDatabase { name });
    let text = format!("{name}\u{0}{}", first_collection.unwrap_or_default());
    mgr.gateway.authorize(&Request {
        caller: Caller::CreateDatabase,
        context,
        engine: handle.engine.id(),
        scope: &context.connection_id,
        text: &text,
        classification: &classification,
        confirmation,
        reusable: false,
    })?;
    let (name, first_collection, ctx) = (
        name.to_owned(),
        first_collection.map(str::to_owned),
        CallContext::new(context.limits()),
    );
    handle
        .call("created a database", |conn| async move {
            let admin = conn.admin().ok_or_else(|| unsupported_capability("creating databases"))?;
            admin.create_database(&name, first_collection.as_deref(), &ctx).await
        })
        .await
}

/// `invoke_capability` from an engine view (or a read-only assistant tool): the engine classifies the operation by name;
/// an unclassified name is refused whatever the rules.
pub async fn invoke(
    mgr: &ConnectionManager,
    context: &Context,
    caller: Caller,
    name: &str,
    args: serde_json::Value,
    confirmation: Option<&Confirmation>,
) -> AppResult<serde_json::Value> {
    if !matches!(caller, Caller::EngineView | Caller::Assistant) {
        return Err(AppError::new("Engine operations come from an engine view or the assistant"));
    }
    let handle = mgr.handle(&context.connection_id)?;
    let classification = handle.engine.classify(&ClassifyInput::Capability { name, args: &args });
    let binding = format!("{name}\u{0}{}", serde_json::to_string(&args)?);
    mgr.gateway.authorize(&Request {
        caller,
        context,
        engine: handle.engine.id(),
        scope: &context.connection_id,
        text: &binding,
        classification: &classification,
        confirmation,
        reusable: false,
    })?;
    let (name, ctx) = (name.to_owned(), CallContext::new(context.limits()));
    handle
        .call(
            "ran an engine operation",
            |conn| async move { conn.invoke(&name, args, &ctx).await },
        )
        .await
}

// --- The old commands (shims until the UI moves to the operations above) ------------------

/// The old `browse_table`: validates the filter as one condition, then reads a page.
pub async fn legacy_browse(db: &ConnectionManager, context: &Context, request: &OldBrowseRequest) -> AppResult<TablePage> {
    check_filter(db, context, &request.connection_id, request.filter.as_deref())?;
    let (request, limits) = (request.clone(), context.limits());
    db.call_legacy(&request.connection_id.clone(), "read a page of rows", |driver| async move {
        driver.browse_table(&request, &limits).await
    })
    .await
}

/// The old `apply_changes` from a grid save or an import.
#[allow(clippy::too_many_arguments)]
pub async fn legacy_apply_changes(
    db: &ConnectionManager,
    context: &Context,
    caller: Caller,
    database: &str,
    table: &str,
    changes: Vec<OldRowChange>,
    import_id: Option<&str>,
    confirmation: Option<&Confirmation>,
) -> AppResult<u64> {
    if !matches!(caller, Caller::GridSave | Caller::Import) {
        return Err(AppError::new("Row changes come from a grid save or an import"));
    }
    let engine = db.handle(&context.connection_id)?.engine.id().to_owned();
    let classification = classify_changes(table, &changes);
    let import_run = import_id.filter(|id| caller == Caller::Import && !id.is_empty());
    let text = match import_run {
        Some(id) => format!("{database}\u{0}{table}\u{0}import:{id}"),
        None => format!("{database}\u{0}{table}\u{0}{}", serde_json::to_string(&changes)?),
    };
    db.gateway.authorize(&Request {
        caller,
        context,
        engine: &engine,
        scope: &context.connection_id,
        text: &text,
        classification: &classification,
        confirmation,
        reusable: import_run.is_some(),
    })?;
    let (database, table) = (database.to_owned(), table.to_owned());
    db.call_legacy(&context.connection_id, "saved row changes", |driver| async move {
        driver.apply_changes(&database, &table, changes).await
    })
    .await
}

/// The old `run_sql` (whole result in one response) over [`run`]: the events are collected into the old shape.
pub async fn run_query(
    db: &ConnectionManager,
    context: &Context,
    session_id: &str,
    database: Option<String>,
    text: String,
    confirmation: Option<&Confirmation>,
) -> AppResult<QueryOutcome> {
    let request = RunRequest { text, database, offset: 0 };
    let mut sink = crate::contract::CollectSink::default();
    let summary = run(db, context, session_id, &request, confirmation, &mut sink).await?;
    let sets = sink
        .sets
        .into_iter()
        .map(|set| crate::drivers::types::ResultSet {
            columns: set
                .columns
                .iter()
                .map(|column| crate::drivers::types::ColumnMeta {
                    name: column.name.clone(),
                    type_name: column.type_name.clone(),
                    numeric: column.kind.is_numeric(),
                    binary: column.hints.binary || matches!(column.kind, crate::contract::ValueKind::Bytes),
                    primary: column.hints.primary,
                    vector_dims: column.hints.vector_dims,
                })
                .collect(),
            rows: set
                .rows
                .iter()
                .map(|row| row.iter().map(crate::runtime::legacy::cell_of).collect())
                .collect(),
            affected_rows: set.affected.unwrap_or(0),
            truncated: set.truncated,
        })
        .collect();
    Ok(QueryOutcome {
        sets,
        elapsed_ms: summary.elapsed_ms,
        transaction: summary.transaction,
    })
}

/// The old `sql_transaction`.
pub async fn legacy_transaction(
    db: &ConnectionManager,
    context: &Context,
    session_id: &str,
    op: TransactionOp,
) -> AppResult<TransactionState> {
    transaction(db, context, session_id, op).await
}

/// What an assistant tool may do. Every tool the model can call must be listed here; the
/// assistant has no tool that runs a statement or changes data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantToolEffect {
    /// Reads catalog metadata (names, columns, DDL) through the driver; never row data.
    ReadMetadata,
    /// Asks the UI to open a view; the UI pre-fills it and runs nothing on its own.
    OpenView,
}

pub fn assistant_tool_effect(name: &str) -> AppResult<AssistantToolEffect> {
    match name {
        "list_databases" | "list_tables" | "describe_table" | "schema_overview" => Ok(AssistantToolEffect::ReadMetadata),
        "open_diagram" | "open_table" | "open_query" => Ok(AssistantToolEffect::OpenView),
        other => Err(AppError::new(format!("The assistant has no tool called {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{rules::Preset, *};

    fn gateway() -> (Gateway, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("fosta-gateway-{}-{}.jsonl", std::process::id(), new_token()));
        (Gateway::with_audit(AuditLog::new(path.clone(), 1000)), path)
    }

    fn context(preset: Preset) -> Context {
        Context {
            workspace_id: "w".into(),
            workspace_name: "Production".into(),
            connection_id: "c".into(),
            connection_name: "Main DB".into(),
            safety: SafetySettings::preset(preset),
        }
    }

    fn ask(gateway: &Gateway, context: &Context, sql: &str, confirmation: Option<&Confirmation>) -> AppResult<()> {
        let classification = classify::classify_sql(crate::contract::SqlDialectId::MariaDb, sql);
        gateway.authorize(&Request {
            caller: Caller::QueryTab,
            context,
            engine: "mariadb",
            scope: "session-1",
            text: sql,
            classification: &classification,
            confirmation,
            reusable: false,
        })
    }

    fn token_of(error: &AppError) -> (String, Option<String>) {
        let detail = error.payload().expect("structured");
        (
            detail["token"].as_str().expect("token").to_owned(),
            detail["expected"].as_str().map(str::to_owned),
        )
    }

    #[test]
    fn open_allows_everything_and_read_only_blocks_writes() {
        let (gateway, path) = gateway();
        let open = context(Preset::Open);
        for sql in ["SELECT 1", "DELETE FROM t", "DROP TABLE t", "GRANT ALL ON *.* TO x"] {
            assert!(ask(&gateway, &open, sql, None).is_ok(), "{sql}");
        }
        let read_only = context(Preset::ReadOnly);
        assert!(ask(&gateway, &read_only, "SELECT * FROM t", None).is_ok());
        assert!(ask(&gateway, &read_only, "COMMIT", None).is_ok(), "commit is always allowed");
        let error = ask(&gateway, &read_only, "INSERT INTO t VALUES (1)", None).unwrap_err();
        assert_eq!(error.code(), Some(BLOCKED));
        assert_eq!(error.payload().unwrap()["categories"][0], "write");
        assert_eq!(ask(&gateway, &read_only, "FROBNICATE", None).unwrap_err().code(), Some(BLOCKED));
        let audit = AuditLog::new(path.clone(), 1000).read_all().unwrap();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[0].outcome, Some(Outcome::Blocked));
        assert_eq!(audit[0].caller, "queryTab");
        assert_eq!(audit[0].workspace, "Production");
        assert!(audit[0].statement.starts_with("INSERT INTO t"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn guarded_confirmation_tokens_are_single_use_and_bound() {
        let (gateway, path) = gateway();
        let guarded = context(Preset::Guarded);
        let sql = "UPDATE orders SET status = 'paid' WHERE id = 7";
        let error = ask(&gateway, &guarded, sql, None).unwrap_err();
        assert_eq!(error.code(), Some(CONFIRMATION_REQUIRED));
        assert_eq!(error.payload().unwrap()["level"], "confirm");
        assert_eq!(error.payload().unwrap()["tables"][0], "orders");
        let (token, expected) = token_of(&error);
        assert!(expected.is_none());
        // Bound to the exact text: a different statement cannot use it (and it is spent).
        let wrong = Confirmation {
            token: token.clone(),
            typed: None,
        };
        let other = ask(&gateway, &guarded, "DELETE FROM orders WHERE id > 0", Some(&wrong)).unwrap_err();
        assert_eq!(other.code(), Some(CONFIRMATION_INVALID));
        assert_eq!(
            ask(&gateway, &guarded, sql, Some(&wrong)).unwrap_err().code(),
            Some(CONFIRMATION_INVALID)
        );
        // A fresh token works once.
        let (token, _) = token_of(&ask(&gateway, &guarded, sql, None).unwrap_err());
        let good = Confirmation { token, typed: None };
        assert!(ask(&gateway, &guarded, sql, Some(&good)).is_ok());
        assert_eq!(
            ask(&gateway, &guarded, sql, Some(&good)).unwrap_err().code(),
            Some(CONFIRMATION_INVALID)
        );
        // Bound to the session too.
        let (token, _) = token_of(&ask(&gateway, &guarded, sql, None).unwrap_err());
        let classification = classify::classify_sql(crate::contract::SqlDialectId::MariaDb, sql);
        let elsewhere = gateway.authorize(&Request {
            caller: Caller::QueryTab,
            context: &guarded,
            engine: "mariadb",
            scope: "session-2",
            text: sql,
            classification: &classification,
            confirmation: Some(&Confirmation { token, typed: None }),
            reusable: false,
        });
        assert_eq!(elsewhere.unwrap_err().code(), Some(CONFIRMATION_INVALID));
        let audit = AuditLog::new(path.clone(), 1000).read_all().unwrap();
        assert_eq!(audit.iter().filter(|entry| entry.outcome == Some(Outcome::Confirmed)).count(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn destructive_statements_need_the_typed_name() {
        let (gateway, path) = gateway();
        let guarded = context(Preset::Guarded);
        let error = ask(&gateway, &guarded, "DELETE FROM shop.orders", None).unwrap_err();
        assert_eq!(error.payload().unwrap()["level"], "typed");
        let (token, expected) = token_of(&error);
        assert_eq!(expected.as_deref(), Some("orders"));
        let typo = Confirmation {
            token,
            typed: Some("order".into()),
        };
        assert_eq!(
            ask(&gateway, &guarded, "DELETE FROM shop.orders", Some(&typo)).unwrap_err().code(),
            Some(CONFIRMATION_INVALID)
        );
        let (token, _) = token_of(&ask(&gateway, &guarded, "DELETE FROM shop.orders", None).unwrap_err());
        let typed = Confirmation {
            token,
            typed: Some(" orders ".into()),
        };
        assert!(ask(&gateway, &guarded, "DELETE FROM shop.orders", Some(&typed)).is_ok());
        // Several tables (or none) ask for the workspace name.
        let error = ask(&gateway, &guarded, "DROP TABLE a, b", None).unwrap_err();
        assert_eq!(token_of(&error).1.as_deref(), Some("Production"));
        let error = ask(&gateway, &guarded, "KILL 12", None).unwrap_err();
        assert_eq!(token_of(&error).1.as_deref(), Some("Production"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn multi_statement_switch_and_the_assistant() {
        let (gateway, path) = gateway();
        let mut custom = context(Preset::Custom);
        custom.safety.rules.write = Action::Allow;
        custom.safety.allow_multi_statement = false;
        assert!(ask(&gateway, &custom, "SELECT 1", None).is_ok());
        assert_eq!(
            ask(&gateway, &custom, "SELECT 1; SELECT 2", None).unwrap_err().code(),
            Some(BLOCKED)
        );
        let open = context(Preset::Open);
        for (sql, allowed) in [("SELECT 1", true), ("DELETE FROM t WHERE id = 1", false)] {
            let classification = classify::classify_sql(crate::contract::SqlDialectId::MariaDb, sql);
            let outcome = gateway.authorize(&Request {
                caller: Caller::Assistant,
                context: &open,
                engine: "mariadb",
                scope: "c",
                text: sql,
                classification: &classification,
                confirmation: None,
                reusable: false,
            });
            assert_eq!(outcome.is_ok(), allowed, "{sql}");
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_import_run_is_confirmed_once_for_all_batches() {
        let (gateway, path) = gateway();
        let guarded = context(Preset::Guarded);
        let classification = classify::classify_sql(crate::contract::SqlDialectId::MariaDb, "INSERT INTO t VALUES (1)");
        let request = |text: &'static str, reusable: bool, confirmation: Option<&'static Confirmation>| Request {
            caller: Caller::Import,
            context: &guarded,
            engine: "mariadb",
            scope: "c",
            text,
            classification: &classification,
            confirmation,
            reusable,
        };
        let error = gateway.authorize(&request("shop\0t\0import:run-1", true, None)).unwrap_err();
        let token = token_of(&error).0;
        let confirmation: &'static Confirmation = Box::leak(Box::new(Confirmation { token, typed: None }));
        for _batch in 0..3 {
            assert!(gateway
                .authorize(&request("shop\0t\0import:run-1", true, Some(confirmation)))
                .is_ok());
        }
        // Not for another run or another table.
        assert!(gateway
            .authorize(&request("shop\0t\0import:run-2", true, Some(confirmation)))
            .is_err());
        let audit = AuditLog::new(path.clone(), 1000).read_all().unwrap();
        assert_eq!(audit.iter().filter(|entry| entry.outcome == Some(Outcome::Confirmed)).count(), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn every_assistant_tool_is_read_only_or_opens_a_view() {
        for name in [
            "list_databases",
            "list_tables",
            "describe_table",
            "schema_overview",
            "open_diagram",
            "open_table",
            "open_query",
        ] {
            assert!(assistant_tool_effect(name).is_ok(), "{name}");
        }
        assert!(assistant_tool_effect("run_sql").is_err());
        assert!(assistant_tool_effect("apply_changes").is_err());
    }
}
