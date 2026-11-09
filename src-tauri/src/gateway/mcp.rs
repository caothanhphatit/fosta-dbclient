//! The statements of MCP clients (`crate::mcp`), through the same rules as everyone else:
//!
//! - `run_read_query` ([`read`]): one statement the engine classifies as a Read, authorized as [`Caller::McpRead`]
//!   (like the assistant: Read only, never a confirmation), run in a read-only session of its own with a row cap and a
//!   timeout.
//! - `run_query` ([`prepare_write`], then [`run_confirmed`]): one statement authorized as [`Caller::Mcp`], which always
//!   needs a confirmation (typed where the rules say so) and is blocked where the rules block it. The one-time token
//!   stays in the app: the user answers in Fosta Studio, never the client.
//!
//! Every run, refusal and unanswered request is audited with caller `mcp` and the client's name, including statements
//! refused before they could be classified and confirmations that no longer hold.

use std::time::Duration;

use serde_json::Value;

use super::{
    assistant::{default_language, run_detail, run_own_session, OwnRun, ReadResult},
    audit_entry,
    classify::{Category, Classification},
    Caller, Confirmation, Context, Request, BLOCKED, CONFIRMATION_REQUIRED,
};
use crate::{
    audit::{AuditEntry, Outcome},
    contract::{require, Capability, ClassifyInput},
    error::{AppError, AppResult},
    runtime::{ConnectionManager, Handle},
};

/// Caps of one MCP run.
#[derive(Clone, Copy, Debug)]
pub struct McpRunLimits {
    pub max_rows: usize,
    pub timeout: Duration,
}

/// Why an MCP client was refused, worded for the client's model.
pub(super) fn refusal_message(caller: Caller, worst: Option<Category>, workspace: &str) -> String {
    match (caller, worst) {
        (Caller::McpRead, Some(Category::Read) | None) => format!(
            "Refused: the rules of workspace {workspace} ask for a confirmation before reads. Use run_query, which asks \
             the user in Fosta Studio, where the workspace allows it, or give the statement to the user to run."
        ),
        (Caller::McpRead, Some(category)) => format!(
            "Refused: run_read_query runs only read-only statements, and this is a {} statement. Use run_query where the \
             workspace's MCP access is \"Ask before writes\", or give the statement to the user to run.",
            category.label().to_lowercase()
        ),
        (_, worst) => format!(
            "Refused: the rules of workspace {workspace} block {} statements. Nothing was run; only the user can change \
             the rules, in Fosta Studio.",
            worst.map_or("these", |category| category.label()).to_lowercase()
        ),
    }
}

/// What the gateway binds a confirmation to: the database and the exact text.
fn binding(text: &str, database: Option<&str>) -> String {
    format!("{}\u{0}{text}", database.unwrap_or_default())
}

struct Prepared<'a> {
    handle: Handle,
    language: String,
    classification: Classification,
    binding: String,
    text: &'a str,
}

impl Prepared<'_> {
    fn request<'r>(&'r self, caller: Caller, context: &'r Context, client: &'r str, confirmation: Option<&'r Confirmation>) -> Request<'r> {
        Request {
            caller,
            context,
            engine: self.handle.engine.id(),
            scope: &context.connection_id,
            text: &self.binding,
            classification: &self.classification,
            confirmation,
            reusable: false,
            client: Some(client),
        }
    }
}

/// Audits a statement refused outside the rules (it could not be classified, or its confirmation no longer holds).
fn audit_refused(mgr: &ConnectionManager, caller: Caller, context: &Context, client: &str, text: &str, detail: &str) {
    let engine = mgr
        .handle(&context.connection_id)
        .map(|handle| handle.engine.id().to_owned())
        .unwrap_or_default();
    mgr.gateway.audit(AuditEntry {
        caller: caller.id(),
        workspace_id: context.workspace_id.clone(),
        workspace: context.workspace_name.clone(),
        connection_id: context.connection_id.clone(),
        connection: context.connection_name.clone(),
        engine,
        statement: text.chars().take(200).collect(),
        outcome: Some(Outcome::Blocked),
        detail: detail.chars().take(500).collect(),
        client: client.to_owned(),
        ..AuditEntry::default()
    });
}

/// Classifies one statement in the engine's default language; more than one is refused. Every failure is audited.
fn prepare<'a>(
    mgr: &ConnectionManager,
    caller: Caller,
    context: &Context,
    client: &str,
    text: &'a str,
    database: Option<&str>,
) -> AppResult<Prepared<'a>> {
    let prepared = prepare_unaudited(mgr, caller, context, client, text, database);
    if let Err(error) = &prepared {
        // A refusal by the rules is audited where it is made.
        if error.code() != Some(BLOCKED) {
            audit_refused(mgr, caller, context, client, text, &error.to_string());
        }
    }
    prepared
}

fn prepare_unaudited<'a>(
    mgr: &ConnectionManager,
    caller: Caller,
    context: &Context,
    client: &str,
    text: &'a str,
    database: Option<&str>,
) -> AppResult<Prepared<'a>> {
    let handle = mgr.handle(&context.connection_id)?;
    require(handle.conn.capabilities(), Capability::Query)?;
    let language = default_language(handle.engine)?;
    if text.trim().is_empty() {
        return Err(AppError::new("Nothing to run: pass one statement in `query`."));
    }
    let classification = handle.engine.classify(&ClassifyInput::Statement { language: &language, text });
    if classification.statements.is_empty() {
        return Err(AppError::new("Nothing to run: pass one statement in `query`."));
    }
    let prepared = Prepared {
        handle,
        language,
        classification,
        binding: binding(text, database),
        text,
    };
    if prepared.classification.statements.len() > 1 && prepared.classification.refusal().is_none() {
        return Err(mgr.gateway.block(
            &prepared.request(caller, context, client, None),
            "Refused: one statement per call. Send each statement in its own call.".to_owned(),
        ));
    }
    Ok(prepared)
}

async fn run_and_audit(
    mgr: &ConnectionManager,
    prepared: &Prepared<'_>,
    caller: Caller,
    context: &Context,
    client: &str,
    database: Option<&str>,
    run: OwnRun,
) -> AppResult<ReadResult> {
    let outcome = run_own_session(&prepared.handle, context, &prepared.language, prepared.text, database, &run).await;
    mgr.gateway.audit(audit_entry(
        &prepared.request(caller, context, client, None),
        Outcome::Ran,
        &run_detail(&outcome),
    ));
    outcome
}

/// `run_read_query`: one Read statement in a read-only session of its own (see the module docs).
pub async fn read(
    mgr: &ConnectionManager,
    context: &Context,
    client: &str,
    text: &str,
    database: Option<&str>,
    limits: McpRunLimits,
) -> AppResult<ReadResult> {
    let prepared = prepare(mgr, Caller::McpRead, context, client, text, database)?;
    mgr.gateway.authorize(&prepared.request(Caller::McpRead, context, client, None))?;
    let run = OwnRun {
        max_rows: limits.max_rows,
        timeout: limits.timeout,
        read_only: true,
        who: "an MCP client",
    };
    run_and_audit(mgr, &prepared, Caller::McpRead, context, client, database, run).await
}

/// `run_query`, first step: the gateway's `CONFIRMATION_REQUIRED` payload for the statement (what runs, its categories,
/// the confirmation level and the one-time token), for the app to ask the user. Blocked statements fail here.
pub fn prepare_write(mgr: &ConnectionManager, context: &Context, client: &str, text: &str, database: Option<&str>) -> AppResult<Value> {
    let prepared = prepare(mgr, Caller::Mcp, context, client, text, database)?;
    match mgr.gateway.authorize(&prepared.request(Caller::Mcp, context, client, None)) {
        Err(error) if error.code() == Some(CONFIRMATION_REQUIRED) => error
            .payload()
            .ok_or_else(|| AppError::new("The confirmation request has no details")),
        Err(error) => Err(error),
        // Caller::Mcp always confirms; never run without the user.
        Ok(()) => Err(AppError::new("Refused: this statement needs the user's approval in Fosta Studio")),
    }
}

/// `run_query`, second step: runs the statement with the user's answer (the token from [`prepare_write`], checked by the
/// gateway: single-use, bound to the exact statement, typed text matching).
pub async fn run_confirmed(
    mgr: &ConnectionManager,
    context: &Context,
    client: &str,
    text: &str,
    database: Option<&str>,
    confirmation: &Confirmation,
    limits: McpRunLimits,
) -> AppResult<ReadResult> {
    let prepared = prepare(mgr, Caller::Mcp, context, client, text, database)?;
    if let Err(error) = mgr
        .gateway
        .authorize(&prepared.request(Caller::Mcp, context, client, Some(confirmation)))
    {
        if error.code() != Some(BLOCKED) {
            audit_refused(mgr, Caller::Mcp, context, client, text, &error.to_string());
        }
        return Err(error);
    }
    let run = OwnRun {
        max_rows: limits.max_rows,
        timeout: limits.timeout,
        read_only: false,
        who: "an MCP client",
    };
    run_and_audit(mgr, &prepared, Caller::Mcp, context, client, database, run).await
}

/// Records a `run_query` the user denied or did not answer.
pub fn audit_unanswered(mgr: &ConnectionManager, context: &Context, client: &str, text: &str, database: Option<&str>, detail: &str) {
    match prepare_unaudited(mgr, Caller::Mcp, context, client, text, database) {
        Ok(prepared) => mgr.gateway.audit(audit_entry(
            &prepared.request(Caller::Mcp, context, client, None),
            Outcome::Blocked,
            detail,
        )),
        Err(_) => audit_refused(mgr, Caller::Mcp, context, client, text, detail),
    }
}
