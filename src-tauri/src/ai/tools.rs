//! Tools the model may call. They only read metadata (database, table and
//! column names, DDL) through the engine contract (`super::catalog`) or ask the
//! UI to open a view; none of them runs a query or changes data. Each tool is
//! registered with the statement gateway (`gateway::assistant_tool_effect`), and
//! a filter from `open_table` is only pre-filled in the grid for the user to apply.

use serde_json::{json, Value};

use super::{catalog, prompt::truncate, ChatContext, ChatEvent, ChatSink, ToolCall, UiAction, Wire};
use crate::{
    contract::{CallContext, ObjectDescription, SchemaGraph},
    gateway,
    runtime::ConnectionManager,
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
            description: "Lists tables and views (or collections, search indexes, keys) of a database with approximate row counts.",
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
    let handle = db.handle(connection_id).map_err(|error| error.to_string())?;
    let ctx = CallContext::default();
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
        "list_databases" => catalog::databases(&handle, &ctx).await.map(|names| names.join("\n")),
        "schema_overview" => {
            let database = database()?;
            catalog::schema_overview(&handle, &ctx, &database)
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
            catalog::objects(&handle, &ctx, &database).await.map(|objects| {
                objects
                    .iter()
                    .map(|object| match &object.detail {
                        Some(detail) => format!("{}\t{}\t{detail}", object.label, object.kind),
                        None => format!("{}\t{}", object.label, object.kind),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        "describe_table" => {
            let table = argument("table").ok_or("`table` is required")?;
            let database = database()?;
            let object = catalog::objects(&handle, &ctx, &database)
                .await
                .and_then(|objects| catalog::find_object(&objects, &database, &table))
                .map_err(|error| error.to_string())?;
            catalog::describe(&handle, &ctx, object)
                .await
                .map(|description| format_description(&table, &description))
        }
        other => return Err(format!("Unknown tool {other}")),
    };
    outcome.map_err(|error| error.to_string())
}

fn format_description(table: &str, description: &ObjectDescription) -> String {
    let primary: Vec<&str> = description
        .primary_key
        .iter()
        .flat_map(|key| key.columns.iter().map(String::as_str))
        .collect();
    let mut text = format!("Columns of {table}:\n");
    for column in &description.columns {
        let nullable = if column.nullable { "NULL" } else { "NOT NULL" };
        let key = if column.hints.primary || primary.contains(&column.name.as_str()) {
            " [PK]"
        } else {
            ""
        };
        let default = column.default.as_ref().map(|value| format!(" default {value}")).unwrap_or_default();
        let identity = column
            .identity
            .as_ref()
            .map(|value| format!(" identity {value}"))
            .unwrap_or_default();
        let generated = column
            .generated
            .as_ref()
            .map(|value| format!(" generated as {value}"))
            .unwrap_or_default();
        let comment = column
            .comment
            .as_deref()
            .filter(|comment| !comment.is_empty())
            .map(|comment| format!(" -- {comment}"))
            .unwrap_or_default();
        text.push_str(&format!(
            "- {} {} {nullable}{key}{default}{identity}{generated}{comment}\n",
            column.name, column.type_name
        ));
    }
    if !description.foreign_keys.is_empty() {
        text.push_str("\nForeign keys:\n");
        for key in &description.foreign_keys {
            let target = [key.ref_schema.as_deref(), Some(key.ref_table.as_str())]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(".");
            text.push_str(&format!(
                "- ({}) -> {target}({})\n",
                key.columns.join(", "),
                key.ref_columns.join(", ")
            ));
        }
    }
    if !description.indexes.is_empty() {
        text.push_str("\nIndexes:\n");
        for index in &description.indexes {
            let unique = if index.primary {
                " primary"
            } else if index.unique {
                " unique"
            } else {
                ""
            };
            text.push_str(&format!("- {}{unique} ({})\n", index.name, index.columns.join(", ")));
        }
    }
    if let Some(ddl) = description.ddl.as_deref().filter(|ddl| !ddl.trim().is_empty()) {
        text.push_str("\nDefinition:\n");
        text.push_str(&truncate(ddl, MAX_DDL_CHARS));
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

    use super::*;
    use crate::contract::testing::FakeEngine;

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
        let engine = FakeEngine::leak();
        db.insert("c", engine, engine.connection());
        let context: ChatContext = serde_json::from_value(json!({ "connectionId": "c", "database": "shop" })).unwrap();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let events = move |event: ChatEvent| sink.lock().unwrap().push(serde_json::to_string(&event).unwrap());
        let inputs = [
            ("list_databases", json!({})),
            ("list_tables", json!({})),
            ("describe_table", json!({ "table": "items" })),
            ("schema_overview", json!({})),
            ("open_diagram", json!({ "focus_table": "items" })),
            ("open_table", json!({ "table": "items", "filter": "name = 'x'" })),
            ("open_query", json!({ "sql": "DELETE FROM items" })),
        ];
        let defined: Vec<&str> = tool_definitions().iter().map(|tool| tool.name).collect();
        assert_eq!(defined.len(), inputs.len(), "every tool is exercised here");
        let mut outputs = Vec::new();
        for (name, input) in inputs {
            assert!(defined.contains(&name), "{name}");
            let outcome = run_tool(&db, &context, &call(name, input), &events).await;
            assert!(outcome.is_ok(), "{name}: {outcome:?}");
            outputs.push(outcome.unwrap());
        }
        assert_eq!(outputs[0], "shop");
        assert!(outputs[1].starts_with("items\ttable"), "{}", outputs[1]);
        assert!(
            outputs[2].contains("- id int NULL") && outputs[2].contains("- name text"),
            "{}",
            outputs[2]
        );
        assert!(outputs[3].contains("- items: id int?, name text?"), "{}", outputs[3]);
        // Unknown objects are a clear error, not a guess.
        let missing = run_tool(&db, &context, &call("describe_table", json!({ "table": "nope" })), &events).await;
        assert!(missing.unwrap_err().contains("no table, view or collection nope"));
        // Stacked statements in a suggested filter are rejected before the UI ever sees them.
        let injected = call(
            "open_table",
            json!({ "table": "items", "filter": "1=0; DELETE FROM items; SELECT 1 WHERE 1" }),
        );
        assert!(run_tool(&db, &context, &injected, &events).await.is_err());
        assert!(
            run_tool(&db, &context, &call("run_sql", json!({ "sql": "DELETE FROM items" })), &events)
                .await
                .is_err()
        );
        // The engine saw metadata reads only: no browse, apply, session, run or invoke.
        let calls = engine.calls.lock().unwrap().clone();
        assert!(
            calls.iter().any(|call| call == "children") && calls.iter().any(|call| call == "describe"),
            "{calls:?}"
        );
        assert!(
            calls.iter().all(|call| matches!(call.as_str(), "children" | "describe")),
            "assistant tools ran more than metadata reads: {calls:?}"
        );
        let events = seen.lock().unwrap().clone();
        let tables: Vec<&String> = events.iter().filter(|event| event.contains("openTable")).collect();
        assert_eq!(tables.len(), 1, "{events:?}");
        assert!(tables[0].contains(r#""suggestedFilter":"name = 'x'""#), "{}", tables[0]);
    }

    // --- Against the dev MariaDB (`dev/compose/mariadb.yml`, demo database `shop`) ------------------------------------

    fn env(name: &str, default: &str) -> String {
        std::env::var(format!("FOSTA_CONTRACT_MARIADB_{name}")).unwrap_or_else(|_| default.to_owned())
    }

    async fn shop() -> ConnectionManager {
        let spec = crate::contract::ConnectionSpec {
            engine: "mariadb".into(),
            host: env("HOST", "127.0.0.1"),
            port: env("PORT", "3306").parse().unwrap_or(3306),
            user: env("USER", "root"),
            database: Some("shop".into()),
            ..crate::contract::ConnectionSpec::default()
        };
        let db = ConnectionManager::default();
        db.connect(
            "shop",
            spec,
            crate::contract::ConnectSecrets::password(&env("PASSWORD", "studio-root")),
        )
        .await
        .expect("connect");
        db
    }

    /// Every metadata tool through the contract on a real server.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs dev/compose/mariadb.yml"]
    async fn contract_mariadb_assistant_tools() {
        let db = shop().await;
        let context: ChatContext = serde_json::from_value(json!({ "connectionId": "shop", "database": "shop" })).unwrap();
        let events = |_: ChatEvent| {};
        let tool = |name: &str, input: Value| {
            let (db, context, call) = (&db, &context, call(name, input));
            async move { run_tool(db, context, &call, &events).await }
        };
        assert!(tool("list_databases", json!({})).await.unwrap().lines().any(|line| line == "shop"));
        let tables = tool("list_tables", json!({})).await.unwrap();
        assert!(tables.lines().any(|line| line.starts_with("products	table")), "{tables}");
        let products = tool("describe_table", json!({ "table": "products" })).await.unwrap();
        assert!(products.contains("[PK]") && products.contains("Foreign keys:"), "{products}");
        assert!(products.contains("CREATE TABLE"), "{products}");
        let overview = tool("schema_overview", json!({})).await.unwrap();
        assert!(
            overview.contains(
                "Foreign keys:
- "
            ),
            "{overview}"
        );
        assert!(tool("describe_table", json!({ "table": "no_such_table" })).await.is_err());
        let prompt = super::super::prompt::system_prompt(&db, &context).await;
        assert!(
            prompt.contains("- Tables in shop: ") && prompt.contains("products (table"),
            "{prompt}"
        );
        db.disconnect("shop").await;
    }

    /// The assistant end to end against Fosta (needs FOSTA_KEY; skipped without it).
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs dev/compose/mariadb.yml and FOSTA_KEY"]
    async fn live_assistant_tools() {
        let Ok(key) = std::env::var("FOSTA_KEY") else {
            println!("FOSTA_KEY not set; skipped");
            return;
        };
        let db = shop().await;
        let settings = crate::ai::AiSettings {
            provider: crate::ai::AiProvider::Fosta,
            base_url: String::new(),
            model: "cx/gpt-6-astra".into(),
        };
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = seen.clone();
        let channel = move |event: ChatEvent| sink.lock().unwrap().push(serde_json::to_string(&event).unwrap());
        let request: crate::ai::ChatRequest = serde_json::from_value(json!({
            "id": "t1",
            "messages": [{ "role": "user", "content": "Open the ER diagram of shop centred on orders, then write a query listing each customer's total paid order amount." }],
            "context": { "connectionId": "shop", "connectionName": "Local MariaDB", "driverLabel": "MariaDB", "database": "shop" }
        }))
        .unwrap();
        let reply = crate::ai::chat(&settings, Some(key), &db, &crate::ai::AiRuns::default(), request, &channel)
            .await
            .expect("chat");
        let events = seen.lock().unwrap().clone();
        assert!(events.iter().any(|event| event.contains("openDiagram")), "no diagram action");
        assert!(reply.contains("```"), "no code block");
        db.disconnect("shop").await;
    }

    async fn connect(id: &str, spec: crate::contract::ConnectionSpec, password: &str) -> ConnectionManager {
        let db = ConnectionManager::default();
        db.connect(id, spec, crate::contract::ConnectSecrets::password(password))
            .await
            .expect("connect");
        db
    }

    fn named_env(engine: &str, name: &str, default: &str) -> String {
        std::env::var(format!("FOSTA_CONTRACT_{engine}_{name}")).unwrap_or_else(|_| default.to_owned())
    }

    /// SQL Server: tables sit under schema and folder nodes; `schema.T` and a bare `T` both resolve.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs dev/compose/mssql.yml"]
    async fn contract_mssql_assistant_tools() {
        let env = |name: &str, default: &str| named_env("MSSQL", name, default);
        let spec = crate::contract::ConnectionSpec {
            engine: "mssql".into(),
            host: env("HOST", "127.0.0.1"),
            port: env("PORT", "1433").parse().unwrap_or(1433),
            user: env("USER", "sa"),
            database: Some("ERP".into()),
            tls: Some(crate::contract::TlsConfig {
                mode: crate::contract::TlsMode::Require,
                accept_invalid_certs: true,
                ..crate::contract::TlsConfig::default()
            }),
            ..crate::contract::ConnectionSpec::default()
        };
        let db = connect("erp", spec, &env("PASSWORD", "Studio#Pass2026")).await;
        let context: ChatContext = serde_json::from_value(json!({ "connectionId": "erp", "database": "ERP" })).unwrap();
        let events = |_: ChatEvent| {};
        let tool = |name: &str, input: Value| {
            let (db, context, call) = (&db, &context, call(name, input));
            async move { run_tool(db, context, &call, &events).await }
        };
        let tables = tool("list_tables", json!({})).await.unwrap();
        assert!(tables.lines().any(|line| line.contains("Customers")), "{tables}");
        assert!(tables.lines().any(|line| line.contains("Invoices")), "{tables}");
        for name in ["sales.Invoices", "Invoices"] {
            let described = tool("describe_table", json!({ "table": name })).await.unwrap();
            assert!(described.contains("InvoiceId") && described.contains("[PK]"), "{name}: {described}");
        }
        let overview = tool("schema_overview", json!({})).await.unwrap();
        assert_eq!(overview.matches(" -> ").count(), 3, "{overview}");
        db.disconnect("erp").await;
    }

    /// MongoDB: collections are listed and described from sampled fields.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs dev/compose/mongo.yml"]
    async fn contract_mongo_assistant_tools() {
        let env = |name: &str, default: &str| named_env("MONGO", name, default);
        let spec = crate::contract::ConnectionSpec {
            engine: "mongodb".into(),
            host: env("HOST", "127.0.0.1"),
            port: env("PORT", "27017").parse().unwrap_or(27017),
            user: env("USER", "root"),
            database: Some("admin".into()),
            ..crate::contract::ConnectionSpec::default()
        };
        let db = connect("events", spec, &env("PASSWORD", "studio-root")).await;
        let context: ChatContext = serde_json::from_value(json!({ "connectionId": "events", "database": "events" })).unwrap();
        let events = |_: ChatEvent| {};
        let tool = |name: &str, input: Value| {
            let (db, context, call) = (&db, &context, call(name, input));
            async move { run_tool(db, context, &call, &events).await }
        };
        let collections = tool("list_tables", json!({})).await.unwrap();
        let first = collections
            .lines()
            .find(|line| line.contains("\tcollection"))
            .and_then(|line| line.split('\t').next())
            .unwrap_or_else(|| panic!("no collection in {collections}"))
            .to_owned();
        let described = tool("describe_table", json!({ "table": first })).await.unwrap();
        assert!(described.contains("- _id "), "{described}");
        let overview = tool("schema_overview", json!({})).await.unwrap();
        assert!(overview.contains("No foreign keys are declared."), "{overview}");
        db.disconnect("events").await;
    }
}
