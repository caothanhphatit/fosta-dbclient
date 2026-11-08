//! Tools the model may call. They only read metadata (database, table and
//! column names, DDL) or ask the UI to open a view; none of them runs a query
//! or changes data. Each tool is registered with the statement gateway
//! (`gateway::assistant_tool_effect`), and a filter from `open_table` is only
//! pre-filled in the grid for the user to apply.

use serde_json::{json, Value};

use super::{prompt::truncate, ChatContext, ChatEvent, ChatSink, ToolCall, UiAction, Wire};
use crate::{
    drivers::types::TableDesign,
    gateway,
    runtime::ConnectionManager,
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
            description: "Opens a table's data grid in the app. A filter (a SQL WHERE condition, or a JSON query for MongoDB) is only pre-filled as your suggestion: nothing runs until the user applies it. The user sees the rows; you do not.",
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

pub(super) async fn run_tool(
    db: &ConnectionManager,
    context: &ChatContext,
    call: &ToolCall,
    events: &dyn ChatSink,
) -> Result<String, String> {
    // Every tool is registered with the gateway as metadata-only or view-opening; anything else is refused.
    gateway::assistant_tool_effect(&call.name).map_err(|error| error.to_string())?;
    let connection_id = context.connection_id.as_deref().ok_or("No connection is open")?;
    let driver = db.legacy_driver(connection_id).map_err(|error| error.to_string())?;
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
            if let Some(filter) = filter.as_deref() {
                // Same check as a grid filter, so the model can fix a bad one; it still never runs here.
                let handle = db.handle(connection_id).map_err(|error| error.to_string())?;
                let descriptor = handle.engine.descriptor();
                let language = descriptor.filter.raw.unwrap_or(crate::contract::RawFilterLang::Sql);
                gateway::filter::validate_raw_filter(language, descriptor.sql_dialect, filter).map_err(|error| error.to_string())?;
            }
            show(
                events,
                UiAction::OpenTable {
                    database: database.clone(),
                    table: table.clone(),
                    suggested_filter: filter.clone(),
                },
            );
            return Ok(match filter {
                Some(filter) => format!(
                    "Opened {database}.{table} with the filter {filter} pre-filled as your suggestion. It has not run: \
                     the user reviews it and applies it. You do not see the rows."
                ),
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::{
        drivers::{
            types::{BrowseRequest, ColumnDesign, QueryOutcome, RowChange, TableInfo, TablePage, TransactionOp, TransactionState},
            Driver, Engine, Limits,
        },
        error::{AppError, AppResult},
    };

    /// Answers catalog reads and records every call that would run a statement or change data.
    #[derive(Default)]
    struct Recorder {
        executed: Mutex<Vec<&'static str>>,
    }

    impl Recorder {
        fn ran(&self, what: &'static str) -> AppError {
            self.executed.lock().unwrap().push(what);
            AppError::new(format!("{what} must not be called by an assistant tool"))
        }
    }

    #[async_trait]
    impl Driver for Recorder {
        fn engine(&self) -> Engine {
            Engine::MariaDb
        }
        async fn list_databases(&self) -> AppResult<Vec<String>> {
            Ok(vec!["shop".into()])
        }
        async fn create_database(&self, _: &str, _: Option<&str>) -> AppResult<()> {
            Err(self.ran("create_database"))
        }
        async fn list_tables(&self, _: &str) -> AppResult<Vec<TableInfo>> {
            Ok(vec![TableInfo {
                name: "orders".into(),
                kind: "table".into(),
                rows: Some(3),
                comment: String::new(),
            }])
        }
        async fn describe_table(&self, _: &str, _: &str) -> AppResult<TableDesign> {
            Ok(TableDesign {
                columns: vec![ColumnDesign {
                    name: "id".into(),
                    column_type: "int".into(),
                    nullable: false,
                    key: "PRI".into(),
                    default_value: None,
                    extra: String::new(),
                    comment: String::new(),
                }],
                ddl: "CREATE TABLE orders (id INT PRIMARY KEY)".into(),
            })
        }
        async fn browse_table(&self, _: &BrowseRequest, _: &Limits) -> AppResult<TablePage> {
            Err(self.ran("browse_table"))
        }
        async fn apply_changes(&self, _: &str, _: &str, _: Vec<RowChange>) -> AppResult<u64> {
            Err(self.ran("apply_changes"))
        }
        async fn open_session(&self, _: &str) -> AppResult<()> {
            Err(self.ran("open_session"))
        }
        fn close_session(&self, _: &str) {}
        async fn run_query(&self, _: &str, _: Option<String>, _: String, _: &Limits) -> AppResult<QueryOutcome> {
            Err(self.ran("run_query"))
        }
        async fn cancel_query(&self, _: &str) -> AppResult<()> {
            Err(self.ran("cancel_query"))
        }
        async fn transaction(&self, _: &str, _: TransactionOp, _: &Limits) -> AppResult<TransactionState> {
            Err(self.ran("transaction"))
        }
    }

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: name.into(),
            input,
        }
    }

    #[tokio::test]
    async fn no_assistant_tool_runs_a_statement_or_changes_data() {
        let db = ConnectionManager::default();
        let recorder = Arc::new(Recorder::default());
        db.insert_legacy("c", recorder.clone());
        let context: ChatContext = serde_json::from_value(json!({ "connectionId": "c", "database": "shop" })).unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let events = move |event: ChatEvent| sink.lock().unwrap().push(serde_json::to_string(&event).unwrap());
        let inputs = [
            ("list_databases", json!({})),
            ("list_tables", json!({})),
            ("describe_table", json!({ "table": "orders" })),
            ("schema_overview", json!({})),
            ("open_diagram", json!({ "focus_table": "orders" })),
            ("open_table", json!({ "table": "orders", "filter": "status = 'paid'" })),
            ("open_query", json!({ "sql": "DELETE FROM orders" })),
        ];
        let defined: Vec<&str> = tool_definitions().iter().map(|tool| tool.name).collect();
        assert_eq!(defined.len(), inputs.len(), "every tool is exercised here");
        for (name, input) in inputs {
            assert!(defined.contains(&name), "{name}");
            let outcome = run_tool(&db, &context, &call(name, input), &events).await;
            assert!(outcome.is_ok(), "{name}: {outcome:?}");
        }
        // Stacked statements in a suggested filter are rejected before the UI ever sees them.
        let injected = call(
            "open_table",
            json!({ "table": "orders", "filter": "1=0; DELETE FROM orders; SELECT 1 WHERE 1" }),
        );
        assert!(run_tool(&db, &context, &injected, &events).await.is_err());
        assert!(
            run_tool(&db, &context, &call("run_sql", json!({ "sql": "DELETE FROM orders" })), &events)
                .await
                .is_err()
        );
        assert!(
            recorder.executed.lock().unwrap().is_empty(),
            "assistant tools ran statements: {:?}",
            recorder.executed.lock().unwrap()
        );
        let events = seen.lock().unwrap().clone();
        let tables: Vec<&String> = events.iter().filter(|event| event.contains("openTable")).collect();
        assert_eq!(tables.len(), 1, "{events:?}");
        assert!(tables[0].contains(r#""suggestedFilter":"status = 'paid'""#), "{}", tables[0]);
    }
}
