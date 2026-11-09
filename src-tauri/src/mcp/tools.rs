//! The MCP tools. Names and argument schemas are a public contract: keep them stable.
//!
//! Every call is gated per connection ([`super::connection_access`]: only workspaces that allow MCP, capped by
//! Production and Read-only rules), connects on demand (reusing a running workspace Command, never starting one), and
//! is audited with caller `mcp` and the client's name, refusals included. Results that hold database content are
//! framed as untrusted data.

use std::time::Duration;

use serde_json::{json, Value};

use super::{clean, connection_access, Answer, ClientInfo, McpAccess, McpApproval};
use crate::{
    ai,
    audit::{AuditEntry, Outcome},
    commands::connections,
    config::AppConfig,
    connection_command::COMMAND_NOT_RUNNING,
    contract::Value as Cell,
    error::AppError,
    gateway::{self, assistant::ReadResult, mcp::McpRunLimits, Confirmation, Context},
    AppState,
};

/// Rows a query returns unless the client asks for fewer or more.
pub const DEFAULT_ROWS: usize = 100;
/// The most rows a query returns.
pub const MAX_ROWS: usize = 500;
/// How long one query may run.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// A cell is cut here.
const MAX_CELL_CHARS: usize = 1_000;
/// The whole query result.
const MAX_RESULT_CHARS: usize = 200_000;
/// The longest statement `run_read_query` and `run_query` accept: the approval dialog shows all of it.
pub const MAX_STATEMENT_CHARS: usize = 20_000;

const NAMES: [&str; 7] = [
    "list_connections",
    "list_databases",
    "list_tables",
    "describe_table",
    "schema_overview",
    "run_read_query",
    "run_query",
];

pub fn exists(name: &str) -> bool {
    NAMES.contains(&name)
}

/// `tools/list`.
pub fn definitions() -> Vec<Value> {
    let connection = json!({ "type": "string", "description": "Connection id (or exact name) from list_connections." });
    let database = json!({ "type": "string", "description": "Database (schema) name; defaults to the connection's database." });
    let query = json!({ "type": "string", "description": "One statement in the connection's query language." });
    let max_rows = json!({ "type": "integer", "minimum": 1, "maximum": MAX_ROWS, "description": format!("Rows to return (default {DEFAULT_ROWS}, at most {MAX_ROWS}).") });
    let read_only = json!({ "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false });
    let tool = |name: &str, title: &str, description: &str, properties: Value, required: &[&str], annotations: Value| {
        json!({
            "name": name,
            "title": title,
            "description": description,
            "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
            "annotations": annotations,
        })
    };
    vec![
        tool(
            "list_connections",
            "List connections",
            "Lists the saved connections the user allows MCP clients to use: id, name, engine, workspace, access \
             (read-only or ask-before-writes) and whether it is connected. No hosts, users or passwords.",
            json!({}),
            &[],
            read_only.clone(),
        ),
        tool(
            "list_databases",
            "List databases",
            "Lists the databases (schemas) of a connection. Connects on demand.",
            json!({ "connection": connection }),
            &["connection"],
            read_only.clone(),
        ),
        tool(
            "list_tables",
            "List tables",
            "Lists the tables and views (or collections, indexes, keys) of a database with approximate row counts.",
            json!({ "connection": connection, "database": database }),
            &["connection"],
            read_only.clone(),
        ),
        tool(
            "describe_table",
            "Describe table",
            "Returns the columns (name, type, nullability, key, default) and the definition of one table, view or \
             collection.",
            json!({ "connection": connection, "table": { "type": "string", "description": "Table, view or collection name." }, "database": database }),
            &["connection", "table"],
            read_only.clone(),
        ),
        tool(
            "schema_overview",
            "Schema overview",
            "Returns every table of a database with its columns (keys marked) and the foreign keys between them.",
            json!({ "connection": connection, "database": database }),
            &["connection"],
            read_only.clone(),
        ),
        tool(
            "run_read_query",
            "Run read-only query",
            &format!(
                "Runs one read-only statement (SELECT, SHOW, EXPLAIN without ANALYZE; a find or aggregate without \
                 $out/$merge; a read-only Redis command; a _search request) in a read-only session and returns at most \
                 {DEFAULT_ROWS} rows by default ({MAX_ROWS} at most), {} s at most. Writes, schema changes and admin \
                 commands are refused.",
                QUERY_TIMEOUT.as_secs()
            ),
            json!({ "connection": connection, "query": query, "database": database, "max_rows": max_rows }),
            &["connection", "query"],
            read_only,
        ),
        tool(
            "run_query",
            "Run query (asks the user)",
            "Runs one statement that may change data, only on connections whose workspace allows \"Ask before writes\". \
             The user sees the statement in Fosta Studio and approves or denies it (no answer within 120 s is a denial). \
             Never available in Production or Read-only workspaces. Prefer run_read_query for reads.",
            json!({ "connection": connection, "query": query, "database": database, "max_rows": max_rows }),
            &["connection", "query"],
            json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false }),
        ),
    ]
}

fn string_arg(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// A connection the client may use, resolved from `connection` (id, else exact name).
struct Target {
    context: Context,
    engine: String,
    access: McpAccess,
    /// The profile's default database.
    database: Option<String>,
}

const UNKNOWN_CONNECTION: &str = "Unknown connection. Call list_connections for the ids you may use.";

fn resolve(app: &AppState, arguments: &Value) -> Result<Target, String> {
    let wanted = string_arg(arguments, "connection").ok_or("`connection` is required: an id from list_connections")?;
    app.config.read(|config| {
        let candidates = config
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.connections.iter())
            .filter(|profile| connection_access(config, &profile.id) != McpAccess::Off);
        let mut by_name = None;
        for profile in candidates {
            if profile.id == wanted {
                by_name = Some(profile);
                break;
            }
            if profile.name == wanted && by_name.is_none() {
                by_name = Some(profile);
            }
        }
        // A connection outside MCP's workspaces gets the same answer as one that does not exist.
        let profile = by_name.ok_or(UNKNOWN_CONNECTION)?;
        target(config, &profile.id)
    })
}

/// The connection `id` as the settings say now (its access may be `Off`).
fn target(config: &AppConfig, id: &str) -> Result<Target, String> {
    let profile = config.connection(id).ok_or(UNKNOWN_CONNECTION)?;
    let context = Context::for_connection(config, id).map_err(|error| error.to_string())?;
    Ok(Target {
        access: connection_access(config, id),
        engine: profile.engine.clone(),
        database: profile.database.clone().filter(|database| !database.is_empty()),
        context,
    })
}

/// Opens the connection when it is not open yet, with its saved secrets. A workspace Command it needs must already run:
/// MCP never starts one. An open connection is reused, never replaced.
async fn ensure_connected(app: &AppState, target: &Target) -> Result<(), String> {
    let id = &target.context.connection_id;
    if app.engines.handle(id).is_ok() {
        return Ok(());
    }
    let _connecting = app.mcp.connecting.lock().await;
    if app.engines.handle(id).is_ok() {
        return Ok(());
    }
    connections::connect_for_mcp(app, id)
        .await
        .map(|_| ())
        .map_err(|error| connect_error(&error))
}

fn connect_error(error: &AppError) -> String {
    match error.code() {
        Some(COMMAND_NOT_RUNNING) => format!(
            "{error} MCP clients never start workspace commands. Ask the user to start it in Fosta Studio (or to \
             connect once there), then try again."
        ),
        Some("PASSWORD_REQUIRED") => "Fosta Studio has no saved password for this connection. Ask the user to connect \
             to it once in Fosta Studio with \"Save password\" on."
            .to_owned(),
        Some(code) if code.starts_with("SSH_HOST_KEY") => "The SSH server's host key is not trusted yet. Ask the user to \
             connect once in Fosta Studio and accept it."
            .to_owned(),
        _ => crate::ai::framed("connect", &format!("Could not connect: {error}")),
    }
}

/// A `tools/call` refused before it reached a tool (no name, an unknown tool, `arguments` not an object): counted and
/// audited like any other call.
pub fn refuse_call(app: &AppState, client: &ClientInfo, name: &str, detail: &str) {
    let name = clean(name, 60);
    let name = if name.is_empty() { "tools/call" } else { name.as_str() };
    app.mcp.record_call(client, name);
    audit(app, client, None, name, "", Outcome::Blocked, detail);
}

/// Runs one tool for `client`; `Err` is a tool error (`isError`), worded for the client's model. Every call writes an
/// audit entry, refusals included.
pub async fn call(app: &AppState, client: &ClientInfo, name: &str, arguments: &Value) -> Result<String, String> {
    app.mcp.record_call(client, name);
    if name == "list_connections" {
        let text = list_connections(app);
        audit(app, client, None, name, "", Outcome::Ran, "ok");
        return Ok(text);
    }
    let refuse = |target: Option<&Target>, error: String| -> Result<String, String> {
        audit(app, client, target, name, &summary(arguments), Outcome::Blocked, &error);
        Err(error)
    };
    let runs_query = matches!(name, "run_read_query" | "run_query");
    // Checked before anything connects. The approval dialog shows the whole statement, so none is longer than it shows.
    let query = match string_arg(arguments, "query") {
        Some(query) if runs_query && query.chars().count() > MAX_STATEMENT_CHARS => {
            return refuse(
                None,
                format!(
                    "Refused: the statement is longer than {MAX_STATEMENT_CHARS} characters, the most Fosta Studio shows \
                     and runs. Send a shorter statement."
                ),
            );
        }
        Some(query) => query,
        None if runs_query => return refuse(None, "`query` is required".to_owned()),
        None => String::new(),
    };
    let limits = match row_limit(arguments) {
        Ok(max_rows) => McpRunLimits {
            max_rows,
            timeout: QUERY_TIMEOUT,
        },
        Err(error) if runs_query => return refuse(None, error),
        Err(_) => McpRunLimits {
            max_rows: DEFAULT_ROWS,
            timeout: QUERY_TIMEOUT,
        },
    };
    let target = match resolve(app, arguments) {
        Ok(target) => target,
        Err(error) => return refuse(None, error),
    };
    if name == "run_query" && target.access != McpAccess::AskBeforeWrites {
        let error = format!(
            "Refused: run_query is not available on this connection; its MCP access is {}. Production and Read-only \
             workspaces allow at most read-only access. Use run_read_query, or give the statement to the user.",
            target.access.label()
        );
        return refuse(Some(&target), error);
    }
    if let Err(error) = ensure_connected(app, &target).await {
        return refuse(Some(&target), error);
    }
    let database = string_arg(arguments, "database").or_else(|| target.database.clone());
    match name {
        "run_read_query" | "run_query" => {
            // The gateway audits every outcome from here on: runs, refusals and unanswered approvals.
            let outcome = if name == "run_read_query" {
                gateway::mcp::read(&app.engines, &target.context, &client.name, &query, database.as_deref(), limits).await
            } else {
                run_query(app, client, &target, &query, database.as_deref(), limits).await
            };
            match outcome {
                Ok(result) => Ok(ai::framed(name, &format_result(&result, limits.max_rows))),
                Err(error) if gateway::assistant::is_refusal(&error) => Err(error.to_string()),
                Err(error) => Err(ai::framed(name, &format!("Error: {error}"))),
            }
        }
        _ => {
            let mut input = arguments.clone();
            if let Some(object) = input.as_object_mut() {
                object.remove("connection");
            }
            let outcome = ai::metadata_tool(&app.engines, &target.context.connection_id, database.as_deref(), name, input).await;
            let detail = match &outcome {
                Ok(_) => "ok".to_owned(),
                Err(error) => format!("failed: {error}"),
            };
            audit(app, client, Some(&target), name, &summary(arguments), Outcome::Ran, &detail);
            outcome
                .map(|text| ai::framed(name, &text))
                .map_err(|error| ai::framed(name, &format!("Error: {error}")))
        }
    }
}

fn row_limit(arguments: &Value) -> Result<usize, String> {
    match arguments.get("max_rows") {
        None | Some(Value::Null) => Ok(DEFAULT_ROWS),
        Some(value) => match value.as_u64() {
            Some(rows) if (1..=MAX_ROWS as u64).contains(&rows) => Ok(rows as usize),
            _ => Err(format!("`max_rows` must be a whole number from 1 to {MAX_ROWS}")),
        },
    }
}

/// Why an approved `run_query` must not run after all: the server, the connection's MCP access or the connection itself
/// changed while the statement waited. `Ok` is the connection as the settings say now.
fn recheck(app: &AppState, connection_id: &str) -> Result<Target, String> {
    let current = app.config.read(|config| config.mcp.enabled.then(|| target(config, connection_id)));
    let Some(current) = current else {
        return Err("Refused: the MCP server was turned off while the statement waited for approval. Nothing was run.".to_owned());
    };
    let current =
        current.map_err(|_| "Refused: the connection was removed while the statement waited for approval. Nothing was run.".to_owned())?;
    if current.access != McpAccess::AskBeforeWrites {
        return Err(format!(
            "Refused: the connection's MCP access changed to {} while the statement waited for approval. Nothing was run.",
            current.access.label()
        ));
    }
    Ok(current)
}

fn refusal(message: String) -> AppError {
    AppError::structured(message, json!({ "code": gateway::BLOCKED }))
}

/// `run_query`: asks the user in the app, then, once approved and only if the server, the connection's access and its
/// rules still allow it, runs the statement with their answer.
async fn run_query(
    app: &AppState,
    client: &ClientInfo,
    target: &Target,
    query: &str,
    database: Option<&str>,
    limits: McpRunLimits,
) -> Result<ReadResult, AppError> {
    let context = &target.context;
    let mut problem = gateway::mcp::prepare_write(&app.engines, context, &client.name, query, database)?;
    let token = problem["token"].as_str().unwrap_or_default().to_owned();
    // The token never leaves the backend: the app answers by approval id.
    if let Some(object) = problem.as_object_mut() {
        object.remove("token");
    }
    let approval = McpApproval {
        id: String::new(),
        client: client.name.clone(),
        client_label: client.label.clone(),
        connection_id: context.connection_id.clone(),
        connection: context.connection_name.clone(),
        workspace: context.workspace_name.clone(),
        database: database.map(str::to_owned),
        statement: query.to_owned(),
        problem,
        timeout_ms: 0,
    };
    let unanswered = |detail: &str| gateway::mcp::audit_unanswered(&app.engines, context, &client.name, query, database, detail);
    let answer = match app.mcp.request_approval(approval).await {
        Ok(answer) => answer,
        Err(error) => {
            unanswered(&error.to_string());
            return Err(refusal(error.to_string()));
        }
    };
    match answer {
        Answer::Approved { typed } => {
            // The settings may have changed during the wait: run under the current ones, or not at all.
            let current = match recheck(app, &context.connection_id) {
                Ok(current) => current,
                Err(why) => {
                    unanswered(&why);
                    return Err(refusal(why));
                }
            };
            let confirmation = Confirmation { token, typed };
            gateway::mcp::run_confirmed(&app.engines, &current.context, &client.name, query, database, &confirmation, limits).await
        }
        Answer::Withdrawn => {
            unanswered("withdrawn: the MCP server was turned off, its settings changed or its token was rotated");
            Err(refusal(
                "Withdrawn: the MCP server was turned off, its settings changed or its token was rotated while the \
                 statement waited for approval. Nothing was run."
                    .to_owned(),
            ))
        }
        Answer::Denied => {
            unanswered("denied by the user");
            Err(AppError::structured(
                "The user denied this statement in Fosta Studio. Nothing was run; do not retry it unless the user asks.",
                json!({ "code": gateway::BLOCKED }),
            ))
        }
        Answer::TimedOut => {
            unanswered("no answer in time");
            Err(AppError::structured(
                format!(
                    "Nobody approved this statement in Fosta Studio within {} seconds, so it was denied and nothing \
                     was run. Ask the user to keep Fosta Studio open and try again if they want it.",
                    super::APPROVAL_TIMEOUT.as_secs()
                ),
                json!({ "code": gateway::BLOCKED }),
            ))
        }
    }
}

/// The arguments for the audit log, without long text.
fn summary(arguments: &Value) -> String {
    ["database", "table"]
        .iter()
        .filter_map(|key| string_arg(arguments, key).map(|value| format!("{key}={value}")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn audit(app: &AppState, client: &ClientInfo, target: Option<&Target>, tool: &str, summary: &str, outcome: Outcome, detail: &str) {
    let statement = if summary.is_empty() {
        tool.to_owned()
    } else {
        format!("{tool} {summary}")
    };
    app.engines.gateway.audit(AuditEntry {
        caller: "mcp".to_owned(),
        workspace_id: target.map(|target| target.context.workspace_id.clone()).unwrap_or_default(),
        workspace: target.map(|target| target.context.workspace_name.clone()).unwrap_or_default(),
        connection_id: target.map(|target| target.context.connection_id.clone()).unwrap_or_default(),
        connection: target.map(|target| target.context.connection_name.clone()).unwrap_or_default(),
        engine: target.map(|target| target.engine.clone()).unwrap_or_default(),
        category: "metadata".to_owned(),
        statement,
        outcome: Some(outcome),
        detail: detail.chars().take(500).collect(),
        client: client.name.clone(),
        ..AuditEntry::default()
    });
}

fn list_connections(app: &AppState) -> String {
    let connections: Vec<Value> = app.config.read(|config| {
        config
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.connections.iter().map(move |profile| (workspace, profile)))
            .filter_map(|(workspace, profile)| {
                let access = connection_access(config, &profile.id);
                (access != McpAccess::Off).then(|| {
                    json!({
                        "id": profile.id,
                        "name": profile.name,
                        "engine": profile.engine,
                        "workspace": workspace.name,
                        "access": match access {
                            McpAccess::AskBeforeWrites => "ask-before-writes",
                            _ => "read-only",
                        },
                        "connected": app.engines.handle(&profile.id).is_ok(),
                    })
                })
            })
            .collect()
    });
    if connections.is_empty() {
        return "No connections are shared with MCP clients. The user turns it on per workspace in Fosta Studio (Edit \
                workspace → MCP clients)."
            .to_owned();
    }
    serde_json::to_string(&json!({ "connections": connections })).unwrap_or_default()
}

fn cell(value: &Cell) -> Value {
    let json = match value {
        Cell::Bytes(bytes) => Value::String(format!("<binary {} bytes>", bytes.len())),
        other => other.to_json(),
    };
    match &json {
        Value::String(text) if text.chars().count() > MAX_CELL_CHARS => {
            Value::String(format!("{}…", text.chars().take(MAX_CELL_CHARS).collect::<String>()))
        }
        Value::Array(_) | Value::Object(_) => {
            let text = json.to_string();
            if text.chars().count() > MAX_CELL_CHARS {
                Value::String(format!("{}…", text.chars().take(MAX_CELL_CHARS).collect::<String>()))
            } else {
                json
            }
        }
        _ => json,
    }
}

/// Compact JSON: columns, rows (arrays of cells), and a note when rows were cut.
fn format_result(result: &ReadResult, max_rows: usize) -> String {
    if result.columns.is_empty() {
        return match result.affected {
            Some(affected) => format!("The statement ran: {affected} row(s) affected."),
            None => "The statement ran and returned no result set.".to_owned(),
        };
    }
    let columns: Vec<&str> = result.columns.iter().map(|column| column.name.as_str()).collect();
    let mut rows = Vec::new();
    let mut size = 0;
    let mut cut = false;
    for row in &result.rows {
        let row = Value::Array(row.iter().map(cell).collect());
        size += row.to_string().len() + 1;
        if size > MAX_RESULT_CHARS {
            cut = true;
            break;
        }
        rows.push(row);
    }
    let mut note = Vec::new();
    if result.truncated {
        note.push(format!(
            "only the first {} row(s) were read (limit {max_rows}); there are more",
            result.rows.len()
        ));
    }
    if cut {
        note.push(format!(
            "{} row(s) left out to keep the answer short",
            result.rows.len() - rows.len()
        ));
    }
    let mut body = json!({
        "columns": columns,
        "rows": rows,
        "rowCount": rows.len(),
        "truncated": result.truncated || cut,
        "elapsedMs": result.elapsed_ms,
    });
    if !note.is_empty() {
        body["note"] = Value::String(note.join("; "));
    }
    body.to_string()
}
