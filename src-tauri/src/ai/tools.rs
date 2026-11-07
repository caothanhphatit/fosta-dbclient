//! Tools the model may call. They only read metadata (database, table and
//! column names, DDL) or ask the UI to open a view; none of them runs a query
//! or changes data.

use serde_json::{json, Value};

use super::{prompt::truncate, ChatContext, ChatEvent, ChatSink, ToolCall, UiAction, Wire};
use crate::{
    drivers::{types::TableDesign, DbManager},
    schema::{self, SchemaGraph},
};

const MAX_DDL_CHARS: usize = 8000;
const MAX_OVERVIEW_CHARS: usize = 24_000;

pub(super) struct ToolSpec {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    pub(super) schema: Value,
}

pub(super) fn tool_definitions() -> Vec<ToolSpec> {
    let database = json!({ "type": "string", "description": "Database name; defaults to the current one." });
    vec![
        ToolSpec {
            name: "schema_overview",
            description: "Returns every table of a database with its columns (keys marked) and all foreign keys between tables. Use it before writing joins across several tables or explaining how tables relate.",
            schema: json!({ "type": "object", "properties": { "database": database }, "additionalProperties": false }),
        },
        ToolSpec {
            name: "open_diagram",
            description: "Opens the interactive ER diagram of a database in a new tab of the app, optionally centred on one table. Use when the user wants to see the structure or relationships.",
            schema: json!({
                "type": "object",
                "properties": { "database": database, "focus_table": { "type": "string", "description": "Table to select and centre." } },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "open_table",
            description: "Opens a table's data grid in the app, optionally filtered (a SQL WHERE condition, or a JSON query for MongoDB). The user sees the rows; you do not.",
            schema: json!({
                "type": "object",
                "properties": {
                    "table": { "type": "string" },
                    "database": database,
                    "filter": { "type": "string", "description": "WHERE condition without the WHERE keyword, e.g. status = 'paid'; JSON for MongoDB." }
                },
                "required": ["table"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "open_query",
            description: "Opens a new query tab with the given text in the editor. It does not run it; the user reviews and runs it.",
            schema: json!({
                "type": "object",
                "properties": { "sql": { "type": "string" }, "database": database },
                "required": ["sql"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "list_databases",
            description: "Lists the databases (schemas) available on the current connection.",
            schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        },
        ToolSpec {
            name: "list_tables",
            description: "Lists tables and views (or collections) of a database with approximate row counts.",
            schema: json!({
                "type": "object",
                "properties": { "database": { "type": "string", "description": "Database name; defaults to the current one." } },
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: "describe_table",
            description: "Returns the columns (name, type, nullability, key, default) and the CREATE statement of one table or view. For MongoDB it returns the fields seen in a sample of documents and the indexes.",
            schema: json!({
                "type": "object",
                "properties": {
                    "table": { "type": "string", "description": "Table, view or collection name (schema.table for SQL Server)." },
                    "database": { "type": "string", "description": "Database name; defaults to the current one." }
                },
                "required": ["table"],
                "additionalProperties": false
            }),
        },
    ]
}

pub(super) fn describe_call(call: &ToolCall, context: &ChatContext) -> String {
    let argument = |key: &str| call.input.get(key).and_then(Value::as_str).map(str::to_owned);
    let database = argument("database").or_else(|| context.database.clone()).unwrap_or_default();
    match call.name.as_str() {
        "list_databases" => String::from("Listed the databases"),
        "list_tables" => format!("Listed the tables of {database}"),
        "describe_table" => format!("Looked at {}", argument("table").unwrap_or_default()),
        "schema_overview" => format!("Read the schema of {database}"),
        "open_diagram" => format!("Opened the diagram of {database}"),
        "open_table" => format!("Opened {}", argument("table").unwrap_or_default()),
        "open_query" => String::from("Opened a query tab"),
        other => format!("Called {other}"),
    }
}

pub(super) async fn run_tool(db: &DbManager, context: &ChatContext, call: &ToolCall, events: &dyn ChatSink) -> Result<String, String> {
    let connection_id = context.connection_id.as_deref().ok_or("No connection is open")?;
    let driver = db.driver(connection_id).map_err(|error| error.to_string())?;
    let argument = |key: &str| {
        call.input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let database = || {
        argument("database")
            .or_else(|| context.database.clone())
            .ok_or_else(|| String::from("No database selected; pass `database`."))
    };
    let outcome = match call.name.as_str() {
        "list_databases" => driver.list_databases().await.map(|names| names.join("\n")),
        "schema_overview" => {
            let database = database()?;
            schema::schema_graph(driver.as_ref(), &database)
                .await
                .map(|graph| format_overview(&database, &graph))
        }
        "open_diagram" => {
            let database = database()?;
            let focus_table = argument("focus_table");
            let note = focus_table
                .as_deref()
                .map(|table| format!(", centred on {table}"))
                .unwrap_or_default();
            show(
                events,
                UiAction::OpenDiagram {
                    database: database.clone(),
                    focus_table,
                },
            );
            return Ok(format!("Opened the ER diagram of {database}{note} in a tab."));
        }
        "open_table" => {
            let table = argument("table").ok_or("`table` is required")?;
            let database = database()?;
            let filter = argument("filter");
            show(
                events,
                UiAction::OpenTable {
                    database: database.clone(),
                    table: table.clone(),
                    filter: filter.clone(),
                },
            );
            return Ok(match filter {
                Some(filter) => format!("Opened {database}.{table} filtered by {filter}. The user sees the rows; you do not."),
                None => format!("Opened {database}.{table}. The user sees the rows; you do not."),
            });
        }
        "open_query" => {
            let sql = argument("sql").ok_or("`sql` is required")?;
            show(
                events,
                UiAction::OpenQuery {
                    database: argument("database").or_else(|| context.database.clone()),
                    sql,
                },
            );
            return Ok(String::from("Opened a query tab with that text. It has not run; the user runs it."));
        }
        "list_tables" => {
            let database = database()?;
            driver.list_tables(&database).await.map(|tables| {
                tables
                    .iter()
                    .map(|table| match table.rows {
                        Some(rows) => format!("{}\t{}\t~{rows} rows", table.name, table.kind),
                        None => format!("{}\t{}", table.name, table.kind),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        "describe_table" => {
            let table = argument("table").ok_or("`table` is required")?;
            let database = database()?;
            driver
                .describe_table(&database, &table)
                .await
                .map(|design| format_design(&table, &design))
        }
        other => return Err(format!("Unknown tool {other}")),
    };
    outcome.map_err(|error| error.to_string())
}

fn format_design(table: &str, design: &TableDesign) -> String {
    let mut text = format!("Columns of {table}:\n");
    for column in &design.columns {
        let nullable = if column.nullable { "NULL" } else { "NOT NULL" };
        let key = if column.key.is_empty() {
            String::new()
        } else {
            format!(" [{}]", column.key)
        };
        let default = match &column.default_value {
            Some(value) => format!(" default {value}"),
            None => String::new(),
        };
        let extra = if column.extra.is_empty() {
            String::new()
        } else {
            format!(" {}", column.extra)
        };
        let comment = if column.comment.is_empty() {
            String::new()
        } else {
            format!(" -- {}", column.comment)
        };
        text.push_str(&format!(
            "- {} {} {nullable}{key}{default}{extra}{comment}\n",
            column.name, column.column_type
        ));
    }
    if !design.ddl.trim().is_empty() {
        text.push_str("\nDefinition:\n");
        text.push_str(&truncate(&design.ddl, MAX_DDL_CHARS));
    }
    text
}

pub(super) fn append_tool_results(wire: Wire, history: &mut Vec<Value>, results: &[(&ToolCall, Result<String, String>)]) {
    match wire {
        Wire::Anthropic => {
            let blocks: Vec<Value> = results
                .iter()
                .map(|(call, outcome)| match outcome {
                    Ok(text) => json!({ "type": "tool_result", "tool_use_id": call.id, "content": text }),
                    Err(error) => json!({ "type": "tool_result", "tool_use_id": call.id, "content": error, "is_error": true }),
                })
                .collect();
            history.push(json!({ "role": "user", "content": blocks }));
        }
        Wire::OpenAi => {
            for (call, outcome) in results {
                let content = match outcome {
                    Ok(text) => text.clone(),
                    Err(error) => format!("Error: {error}"),
                };
                history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
            }
        }
    }
}

fn show(events: &dyn ChatSink, action: UiAction) {
    let _ = events.send(ChatEvent::Action { action });
}

/// Compact text for the model: one line per table, then the foreign keys.
fn format_overview(database: &str, graph: &SchemaGraph) -> String {
    let mut text = format!("Schema of {database}: {} tables", graph.tables.len());
    if graph.total_tables > graph.tables.len() {
        text.push_str(&format!(" (first {} of {})", graph.tables.len(), graph.total_tables));
    }
    text.push_str("\n\nTables (* = primary key, ? = nullable):\n");
    for table in &graph.tables {
        let columns: Vec<String> = table
            .columns
            .iter()
            .map(|column| {
                format!(
                    "{}{} {}{}",
                    if column.primary { "*" } else { "" },
                    column.name,
                    column.column_type,
                    if column.nullable { "?" } else { "" }
                )
            })
            .collect();
        let kind = if table.kind == "view" { " [view]" } else { "" };
        let rows = table.rows.map(|rows| format!(" ~{rows} rows")).unwrap_or_default();
        text.push_str(&format!("- {}{kind}{rows}: {}\n", table.name, columns.join(", ")));
    }
    if graph.relations.is_empty() {
        text.push_str("\nNo foreign keys are declared.");
    } else {
        text.push_str("\nForeign keys:\n");
        for relation in &graph.relations {
            text.push_str(&format!(
                "- {}({}) -> {}({})\n",
                relation.table,
                relation.columns.join(", "),
                relation.referenced_table,
                relation.referenced_columns.join(", ")
            ));
        }
    }
    truncate(&text, MAX_OVERVIEW_CHARS)
}
