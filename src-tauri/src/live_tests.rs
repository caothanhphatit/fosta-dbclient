//! Driver tests against the containers in `dev/docker-compose.yml`.
//! Ignored by default; run with:
//!   cargo test live_ -- --ignored --test-threads=1

use std::collections::HashMap;

use crate::{
    config::ConnectionProfile,
    contract::ConnectSecrets,
    drivers::{
        self,
        types::{BrowseRequest, CellValue, RowChange, TransactionOp},
        Driver, Limits,
    },
    gateway::{
        self,
        rules::{self, Preset, SafetySettings},
        Confirmation, Context,
    },
    runtime::ConnectionManager,
};

fn profile(id: &str, driver: &str, port: u16, user: &str, database: &str, options: &[(&str, &str)]) -> ConnectionProfile {
    ConnectionProfile {
        id: id.into(),
        name: id.into(),
        driver: driver.into(),
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: Some(database.into()),
        save_password: true,
        options: options
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<HashMap<_, _>>(),
        safety: None,
    }
}

fn browse(database: &str, table: &str, filter: Option<&str>) -> BrowseRequest {
    BrowseRequest {
        connection_id: String::new(),
        database: database.into(),
        table: table.into(),
        offset: 0,
        limit: 5,
        order_by: None,
        descending: false,
        filter: filter.map(str::to_owned),
    }
}

fn cell(column: &str, value: &str) -> CellValue {
    CellValue {
        column: column.into(),
        value: Some(value.into()),
    }
}

/// Creates a scratch database, checks it is listed, then drops it with `drop_sql`.
async fn create_and_drop(driver: &dyn Driver, name: &str, collection: Option<&str>, run_in: Option<&str>, drop_sql: &str) {
    driver.create_database(name, collection).await.expect("create_database");
    assert!(
        driver.list_databases().await.expect("list").iter().any(|item| item == name),
        "{name} not listed"
    );
    driver.open_session("drop").await.expect("session");
    driver
        .run_query("drop", run_in.map(str::to_owned), drop_sql.into(), &Limits::default())
        .await
        .expect("drop");
    driver.close_session("drop");
}

async fn exercise(driver: &dyn Driver, database: &str, table: &str, filter: &str, query: &str) {
    let databases = driver.list_databases().await.expect("list_databases");
    assert!(
        databases.iter().any(|name| name == database),
        "{database} missing from {databases:?}"
    );
    let tables = driver.list_tables(database).await.expect("list_tables");
    assert!(tables.iter().any(|item| item.name == table), "{table} missing");
    let design = driver.describe_table(database, table).await.expect("describe_table");
    assert!(!design.columns.is_empty());
    let page = driver
        .browse_table(&browse(database, table, None), &Limits::default())
        .await
        .expect("browse_table");
    assert_eq!(page.rows.len(), 5);
    let filtered = driver
        .browse_table(&browse(database, table, Some(filter)), &Limits::default())
        .await
        .expect("filtered browse");
    assert!(
        filtered.total < page.total,
        "filter did not narrow: {} vs {}",
        filtered.total,
        page.total
    );
    driver.open_session("live").await.expect("open_session");
    let outcome = driver
        .run_query("live", Some(database.into()), query.into(), &Limits::default())
        .await
        .expect("run_query");
    assert!(outcome.sets.iter().any(|set| !set.rows.is_empty()), "query returned nothing");
    driver.close_session("live");
    println!(
        "{table}: {} tables, {} columns, total {}, filtered {}, query sets {}",
        tables.len(),
        design.columns.len(),
        page.total,
        filtered.total,
        outcome.sets.len()
    );
}

#[tokio::test]
#[ignore]
async fn live_saved_passwords_are_readable() {
    for id in ["local-mariadb", "local-mssql", "local-mongo"] {
        assert!(
            crate::secrets::load_password(id).expect("keyring").is_some(),
            "no saved password for {id}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn live_mariadb() {
    let profile = profile("live-mariadb", "mariadb", 3306, "root", "shop", &[]);
    let (driver, version) = drivers::open(&profile, "studio-root").await.expect("connect");
    println!("MariaDB {version}");
    exercise(
        driver.as_ref(),
        "shop",
        "products",
        "stock < 5",
        "SELECT category_id, COUNT(*) FROM products GROUP BY category_id",
    )
    .await;
    create_and_drop(driver.as_ref(), "studio_scratch", None, None, "DROP DATABASE `studio_scratch`").await;
    let relations = driver.table_relations("shop", "products").await.expect("relations");
    assert!(!relations.outgoing.is_empty() && !relations.incoming.is_empty());
    // A table with rows never shows a stale "~0" estimate (InnoDB statistics lag after the bulk seed).
    let tables = driver.list_tables("shop").await.expect("tables");
    let order_items = tables.iter().find(|table| table.name == "order_items").expect("order_items");
    assert_ne!(order_items.rows, Some(0), "order_items holds rows");
    // Declared SQL types on table pages and SQL type names in query results, not wire types (long, newdecimal).
    let page = driver
        .browse_table(&browse("shop", "order_items", None), &Limits::default())
        .await
        .expect("browse");
    let id = page.columns.iter().find(|column| column.name == "id").expect("id");
    assert!(
        id.type_name.starts_with("int") || id.type_name.starts_with("bigint"),
        "{}",
        id.type_name
    );
    driver.open_session("types").await.expect("session");
    let typed = driver
        .run_query(
            "types",
            Some("shop".into()),
            "SELECT 1 AS n, CAST(1.5 AS DECIMAL(4,2)) AS d, CAST('2025-01-02' AS DATE) AS t".into(),
            &Limits::default(),
        )
        .await
        .expect("typed query");
    let names: Vec<&str> = typed.sets[0].columns.iter().map(|column| column.type_name.as_str()).collect();
    assert_eq!(names, ["int", "decimal", "date"]);
    driver.close_session("types");
    // MySQL counts changed rows, so write a different value and then restore it.
    for name in ["Outdoor (edited)", "Outdoor"] {
        let changed = driver
            .apply_changes(
                "shop",
                "categories",
                vec![RowChange::Update {
                    key: vec![cell("id", "8")],
                    values: vec![cell("name", name)],
                }],
            )
            .await
            .expect("apply_changes");
        assert_eq!(changed, 1);
    }
    driver.close().await;
}

#[tokio::test]
#[ignore]
async fn live_mssql() {
    let profile = profile(
        "live-mssql",
        "mssql",
        1433,
        "sa",
        "ERP",
        &[("encrypt", "true"), ("trustServerCertificate", "true")],
    );
    let (driver, version) = drivers::open(&profile, "Studio#Pass2026").await.expect("connect");
    println!("SQL Server {version}");
    exercise(
        driver.as_ref(),
        "ERP",
        "sales.Invoices",
        "Status = 'paid'",
        "SELECT TOP 5 * FROM sales.vMonthlyRevenue ORDER BY Month",
    )
    .await;
    // The grid's column filters and foreign-key navigation for SQL Server (src/core/sql.ts, tsql dialect).
    let filtered = driver
        .browse_table(
            &browse("ERP", "sales.Invoices", Some("([Status] LIKE N'%paid%') AND ([InvoiceId] > 0)")),
            &Limits::default(),
        )
        .await
        .expect("bracket-quoted filter");
    assert!(filtered.total > 0, "bracket-quoted filter matches rows");
    // Declared SQL types, not TDS wire types (int4, bigvarchar, decimaln).
    let invoice_id = filtered
        .columns
        .iter()
        .find(|column| column.name == "InvoiceId")
        .expect("InvoiceId");
    assert_eq!(invoice_id.type_name, "int");
    let wire = ["int4", "intn", "bigvarchar", "nvarchar_", "decimaln"];
    assert!(
        filtered.columns.iter().all(|column| !wire.contains(&column.type_name.as_str())),
        "{:?}",
        filtered.columns
    );
    create_and_drop(driver.as_ref(), "StudioScratch", None, None, "DROP DATABASE [StudioScratch]").await;
    let relations = driver.table_relations("ERP", "sales.Invoices").await.expect("relations");
    assert!(!relations.outgoing.is_empty());
    let changed = driver
        .apply_changes(
            "ERP",
            "Warehouses",
            vec![RowChange::Update {
                key: vec![cell("WarehouseId", "1")],
                values: vec![cell("City", "Soc Trang")],
            }],
        )
        .await
        .expect("apply_changes");
    assert_eq!(changed, 1);
    driver.close().await;
}

/// Batches return every result set and every row count, in order, whatever comes first.
#[tokio::test]
#[ignore]
async fn live_mssql_batches() {
    let (driver, _) = drivers::open(&mssql_profile("live-mssql-batch"), "Studio#Pass2026")
        .await
        .expect("connect");
    driver.open_session("batch").await.expect("session");
    let limits = Limits::default();
    let run = |sql: &'static str| driver.run_query("batch", Some("ERP".into()), sql.into(), &limits);

    let update_first = run("UPDATE sales.Invoices SET Status = Status WHERE InvoiceId = 1; SELECT TOP 2 InvoiceId FROM sales.Invoices")
        .await
        .expect("update then select");
    assert_eq!(update_first.sets.len(), 2, "{:?}", update_first.sets);
    assert!(update_first.sets[0].columns.is_empty());
    assert_eq!(update_first.sets[0].affected_rows, 1);
    assert_eq!(update_first.sets[1].rows.len(), 2);

    let select_first = run("SELECT TOP 2 InvoiceId FROM sales.Invoices; UPDATE sales.Invoices SET Status = Status WHERE InvoiceId = 1")
        .await
        .expect("select then update");
    assert_eq!(select_first.sets.len(), 2, "{:?}", select_first.sets);
    assert_eq!(select_first.sets[0].rows.len(), 2);
    assert_eq!(select_first.sets[1].affected_rows, 1);

    // Variables keep their scope: the batch still runs as one batch.
    let scoped = run("DECLARE @id INT = 1; UPDATE sales.Invoices SET Status = Status WHERE InvoiceId = @id; SELECT @id AS id")
        .await
        .expect("declared variable");
    assert_eq!(scoped.sets.len(), 2, "{:?}", scoped.sets);
    assert_eq!(scoped.sets[1].rows[0][0].as_deref(), Some("1"));

    let error = run("SELECT * FROM sales.NoSuchTable").await.expect_err("missing table");
    assert!(error.to_string().contains("SQL Server error 208"), "{error}");
    assert!(!error.to_string().contains("executing  on line"), "{error}");
    driver.close_session("batch");
    driver.close().await;
}

/// SQL Server's self-signed certificate without "Trust server certificate" explains the fix.
#[tokio::test]
#[ignore]
async fn live_mssql_untrusted_certificate() {
    let untrusted = profile(
        "live-mssql-tls",
        "mssql",
        1433,
        "sa",
        "ERP",
        &[("encrypt", "true"), ("trustServerCertificate", "false")],
    );
    let error = drivers::probe(&untrusted, "Studio#Pass2026")
        .await
        .expect_err("certificate not trusted");
    assert!(error.to_string().contains("Trust server certificate"), "{error}");
}

/// The connection-string tab stores the URI without its password; the driver adds it back.
#[tokio::test]
#[ignore]
async fn live_mongo_uri_without_password() {
    let profile = profile(
        "live-mongo-uri",
        "mongodb",
        27017,
        "root",
        "admin",
        &[("uri", "mongodb://root@127.0.0.1:27017/?authSource=admin")],
    );
    let (driver, _) = drivers::open(&profile, "studio-root").await.expect("connect with uri");
    assert!(driver.list_databases().await.expect("list").iter().any(|name| name == "events"));
    driver.close().await;
}

#[tokio::test]
#[ignore]
async fn live_mongo() {
    let profile = profile("live-mongo", "mongodb", 27017, "root", "admin", &[]);
    let (driver, version) = drivers::open(&profile, "studio-root").await.expect("connect");
    println!("MongoDB {version}");
    exercise(
        driver.as_ref(),
        "events",
        "clicks",
        r#"{ "type": "purchase" }"#,
        r#"{ "aggregate": "clicks", "pipeline": [ { "$group": { "_id": "$device", "n": { "$sum": 1 } } } ], "cursor": {} }"#,
    )
    .await;
    create_and_drop(
        driver.as_ref(),
        "studio_scratch",
        Some("first"),
        Some("studio_scratch"),
        r#"{ "dropDatabase": 1 }"#,
    )
    .await;
    // Mongo also counts modified documents; flip the value and restore it.
    for plan in ["pro", "free"] {
        let changed = driver
            .apply_changes(
                "events",
                "users",
                vec![RowChange::Update {
                    key: vec![cell("_id", "1")],
                    values: vec![cell("plan", plan)],
                }],
            )
            .await
            .expect("apply_changes");
        assert!(changed <= 1);
    }
    driver.close().await;
}

/// Grid edits keep BSON types, and query-tab cursors are read past the first batch.
#[tokio::test]
#[ignore]
async fn live_mongo_types_and_cursors() {
    let profile = profile("live-mongo-types", "mongodb", 27017, "root", "admin", &[]);
    let (driver, _) = drivers::open(&profile, "studio-root").await.expect("connect");
    driver.open_session("m").await.expect("session");
    let limits = Limits::default();
    let run = |text: &'static str| driver.run_query("m", Some("events".into()), text.into(), &limits);

    let user = run(r#"{ "find": "users", "filter": { "_id": 1 } }"#).await.expect("find user");
    let set = &user.sets[0];
    let position = set
        .columns
        .iter()
        .position(|column| column.name == "signedUpAt")
        .expect("signedUpAt column");
    assert_eq!(set.columns[position].type_name, "date");
    let shown = set.rows[0][position].clone().expect("a date");
    // Save the shown value back, as the grid does after an edit.
    driver
        .apply_changes(
            "events",
            "users",
            vec![RowChange::Update {
                key: vec![cell("_id", "1")],
                values: vec![cell("signedUpAt", &shown)],
            }],
        )
        .await
        .expect("apply");
    let still_date = run(r#"{ "count": "users", "query": { "_id": 1, "signedUpAt": { "$type": "date" } } }"#)
        .await
        .expect("count");
    let n = still_date.sets[0].columns.iter().position(|column| column.name == "n").expect("n");
    assert_eq!(
        still_date.sets[0].rows[0][n].as_deref(),
        Some("1"),
        "signedUpAt is still a BSON date"
    );
    let refused = driver
        .apply_changes(
            "events",
            "users",
            vec![RowChange::Update {
                key: vec![cell("_id", "1")],
                values: vec![cell("signedUpAt", "not a date")],
            }],
        )
        .await
        .expect_err("text is not a date");
    assert!(refused.to_string().contains("holds a date"), "{refused}");

    let five_hundred = run(r#"{ "find": "clicks", "limit": 500 }"#).await.expect("find 500");
    assert_eq!(five_hundred.sets[0].rows.len(), 500);
    assert!(!five_hundred.sets[0].truncated);
    let capped = run(r#"{ "find": "clicks", "limit": 5000 }"#).await.expect("find 5000");
    assert_eq!(capped.sets[0].rows.len(), limits.max_rows);
    assert!(capped.sets[0].truncated);
    driver.close_session("m");
    driver.close().await;
}

/// The ER diagram's data on each engine: tables, columns and the expected foreign keys.
#[tokio::test]
#[ignore]
async fn live_schema_graphs() {
    let cases = [
        (profile("g-maria", "mariadb", 3306, "root", "shop", &[]), "studio-root", "shop", 4),
        (
            profile(
                "g-mssql",
                "mssql",
                1433,
                "sa",
                "ERP",
                &[("encrypt", "true"), ("trustServerCertificate", "true")],
            ),
            "Studio#Pass2026",
            "ERP",
            3,
        ),
        (
            profile("g-mongo", "mongodb", 27017, "root", "admin", &[]),
            "studio-root",
            "events",
            0,
        ),
    ];
    for (profile, password, database, expected_links) in cases {
        let (driver, _) = drivers::open(&profile, password).await.expect("connect");
        let started = std::time::Instant::now();
        let slow = crate::schema::schema_graph_per_table(driver.as_ref(), database)
            .await
            .expect("per-table schema_graph");
        let per_table = started.elapsed();
        let started = std::time::Instant::now();
        let _warm = crate::schema::schema_graph(driver.as_ref(), database).await.expect("schema_graph");
        let first = started.elapsed();
        let started = std::time::Instant::now();
        let graph = crate::schema::schema_graph(driver.as_ref(), database).await.expect("schema_graph");
        let bulk = started.elapsed();
        println!("{database}: per-table {per_table:?}, bulk {bulk:?} (cold {first:?})");
        assert_eq!(
            serde_json::to_string(&graph).unwrap(),
            serde_json::to_string(&slow).unwrap(),
            "{database} bulk graph differs"
        );
        let columns: usize = graph.tables.iter().map(|table| table.columns.len()).sum();
        let keys: usize = graph
            .tables
            .iter()
            .flat_map(|table| &table.columns)
            .filter(|column| column.primary)
            .count();
        println!(
            "{database}: {} tables, {columns} columns, {keys} key columns, {} relations",
            graph.tables.len(),
            graph.relations.len()
        );
        for relation in &graph.relations {
            println!(
                "  {}.{:?} -> {}.{:?}",
                relation.table, relation.columns, relation.referenced_table, relation.referenced_columns
            );
        }
        assert_eq!(graph.relations.len(), expected_links, "{database} relations");
        assert!(keys > 0);
        driver.close().await;
    }
}

/// The assistant end to end against Fosta: needs FOSTA_KEY in the environment.
#[tokio::test]
#[ignore]
async fn live_assistant_tools() {
    use std::sync::{Arc, Mutex};
    let Ok(key) = std::env::var("FOSTA_KEY") else {
        println!("FOSTA_KEY not set; skipped");
        return;
    };
    let manager = ConnectionManager::default();
    manager
        .connect(
            "chat-maria",
            profile("chat-maria", "mariadb", 3306, "root", "shop", &[]).spec(),
            ConnectSecrets::password("studio-root"),
        )
        .await
        .expect("connect");
    let settings = crate::ai::AiSettings {
        provider: crate::ai::AiProvider::Fosta,
        base_url: String::new(),
        model: "cx/gpt-6-astra".into(),
    };
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = seen.clone();
    let channel = move |event: crate::ai::ChatEvent| {
        sink.lock().unwrap().push(serde_json::to_string(&event).unwrap());
    };
    let request: crate::ai::ChatRequest = serde_json::from_value(serde_json::json!({
        "id": "t1",
        "messages": [{ "role": "user", "content": "Open the ER diagram of shop centred on orders, then write a query listing each customer's total paid order amount." }],
        "context": { "connectionId": "chat-maria", "connectionName": "Local MariaDB", "driverLabel": "MariaDB", "database": "shop" }
    }))
    .unwrap();
    let reply = crate::ai::chat(&settings, Some(key), &manager, &crate::ai::AiRuns::default(), request, &channel)
        .await
        .expect("chat");
    let events = seen.lock().unwrap().clone();
    for event in events.iter().filter(|event| !event.contains("\"text\"")) {
        println!("event {event}");
    }
    println!("reply:\n{reply}");
    assert!(events.iter().any(|event| event.contains("openDiagram")), "no diagram action");
    assert!(reply.contains("```"), "no code block");
}

// --- Statement gateway and transactions ------------------------------------------------

const INJECTED_FILTER: &str = "1=0; DELETE FROM gate_probe; SELECT 1 WHERE 1";

fn mssql_profile(id: &str) -> ConnectionProfile {
    profile(
        id,
        "mssql",
        1433,
        "sa",
        "ERP",
        &[("encrypt", "true"), ("trustServerCertificate", "true")],
    )
}

fn gate(connection_id: &str, preset: Preset) -> Context {
    Context {
        workspace_id: "live".into(),
        workspace_name: "Live".into(),
        connection_id: connection_id.into(),
        connection_name: connection_id.into(),
        safety: SafetySettings::preset(preset),
    }
}

/// Runs app-internal setup SQL in a throwaway session (no gateway: this is the test's own fixture).
async fn setup(manager: &ConnectionManager, connection_id: &str, database: &str, sql: &str) {
    let session = format!("setup-{connection_id}");
    manager
        .open_legacy_session(connection_id, session.clone())
        .await
        .expect("setup session");
    let driver = manager
        .legacy_driver(&manager.session_connection(&session).unwrap())
        .expect("driver");
    driver
        .run_query(&session, Some(database.into()), sql.into(), &Limits::default())
        .await
        .unwrap_or_else(|error| panic!("setup {sql}: {error}"));
    manager.close_session(&session).await;
}

async fn probe_rows(manager: &ConnectionManager, connection_id: &str, database: &str) -> u64 {
    let request = BrowseRequest {
        connection_id: connection_id.into(),
        ..browse(database, "gate_probe", None)
    };
    gateway::legacy_browse(manager, &gate(connection_id, Preset::Open), &request)
        .await
        .expect("browse probe")
        .total
}

/// Filters that try to stack statements are rejected by the gateway on MariaDB and SQL Server,
/// and on MariaDB even the driver alone cannot run them (prepared statements).
#[tokio::test]
#[ignore]
async fn live_gateway_rejects_injected_filters() {
    let cases = [
        (profile("gate-maria", "mariadb", 3306, "root", "shop", &[]), "studio-root", "shop"),
        (mssql_profile("gate-mssql"), "Studio#Pass2026", "ERP"),
    ];
    for (profile, password, database) in cases {
        let manager = ConnectionManager::default();
        manager
            .connect(&profile.id, profile.spec(), ConnectSecrets::password(password))
            .await
            .expect("connect");
        let id = profile.id.as_str();
        let create = if profile.driver == "mssql" {
            "IF OBJECT_ID('dbo.gate_probe') IS NULL CREATE TABLE dbo.gate_probe (id INT PRIMARY KEY); \
             DELETE FROM dbo.gate_probe; INSERT INTO dbo.gate_probe VALUES (1)"
        } else {
            "CREATE TABLE IF NOT EXISTS gate_probe (id INT PRIMARY KEY); DELETE FROM gate_probe; INSERT INTO gate_probe VALUES (1)"
        };
        setup(&manager, id, database, create).await;
        let context = gate(id, Preset::Open);
        let injected = BrowseRequest {
            connection_id: id.into(),
            ..browse(database, "gate_probe", Some(INJECTED_FILTER))
        };
        let error = gateway::legacy_browse(&manager, &context, &injected)
            .await
            .expect_err("injected filter must fail");
        assert_eq!(error.code(), Some(gateway::filter::INVALID_FILTER), "{error}");
        assert_eq!(
            probe_rows(&manager, id, database).await,
            1,
            "{}: the probe row survived",
            profile.driver
        );
        if profile.driver == "mariadb" {
            // Defence in depth: the driver itself runs browse queries as prepared statements.
            let direct = manager.legacy_driver(id).unwrap().browse_table(&injected, &Limits::default()).await;
            assert!(direct.is_err(), "a prepared statement cannot carry a second statement");
            assert_eq!(probe_rows(&manager, id, database).await, 1);
        }
        let fine = BrowseRequest {
            connection_id: id.into(),
            ..browse(database, "gate_probe", Some("id = 1"))
        };
        assert_eq!(
            gateway::legacy_browse(&manager, &context, &fine).await.expect("valid filter").total,
            1
        );
        println!("{}: injected filter rejected with: {error}", profile.driver);
        setup(&manager, id, database, "DROP TABLE gate_probe").await;
        manager.disconnect(id).await;
    }
}

/// Read-only blocks writes (gateway and engine); Guarded asks for a confirmation and runs with the token.
#[tokio::test]
#[ignore]
async fn live_gateway_rules_and_confirmation() {
    let manager = ConnectionManager::default();
    let profile = profile("rules-maria", "mariadb", 3306, "root", "shop", &[]);
    manager
        .connect(&profile.id, profile.spec(), ConnectSecrets::password("studio-root"))
        .await
        .expect("connect");
    setup(
        &manager,
        "rules-maria",
        "shop",
        "CREATE TABLE IF NOT EXISTS gate_probe (id INT PRIMARY KEY); DELETE FROM gate_probe",
    )
    .await;
    manager.open_legacy_session("rules-maria", "tab".into()).await.expect("session");
    let insert = "INSERT INTO gate_probe VALUES (7)";

    let read_only = gate("rules-maria", Preset::ReadOnly);
    let blocked = gateway::run_query(&manager, &read_only, "tab", Some("shop".into()), insert.into(), None)
        .await
        .expect_err("read-only blocks INSERT");
    assert_eq!(blocked.code(), Some(gateway::BLOCKED), "{blocked}");
    let read = gateway::run_query(
        &manager,
        &read_only,
        "tab",
        Some("shop".into()),
        "SELECT COUNT(*) FROM gate_probe".into(),
        None,
    )
    .await
    .expect("reads still run");
    assert_eq!(read.sets[0].rows[0][0].as_deref(), Some("0"));
    // The session itself is read-only now: even bypassing the gateway the server refuses the write.
    let driver = manager.legacy_driver(&manager.session_connection("tab").unwrap()).unwrap();
    let engine_refusal = driver
        .run_query(
            "tab",
            Some("shop".into()),
            insert.into(),
            &Limits {
                read_only: true,
                ..Limits::default()
            },
        )
        .await
        .expect_err("READ ONLY session");
    println!("engine refusal: {engine_refusal}");

    let guarded = gate("rules-maria", Preset::Guarded);
    let asked = gateway::run_query(&manager, &guarded, "tab", Some("shop".into()), insert.into(), None)
        .await
        .expect_err("guarded asks first");
    assert_eq!(asked.code(), Some(gateway::CONFIRMATION_REQUIRED));
    let token = asked.payload().unwrap()["token"].as_str().unwrap().to_owned();
    let confirmation = Confirmation { token, typed: None };
    let outcome = gateway::run_query(&manager, &guarded, "tab", Some("shop".into()), insert.into(), Some(&confirmation))
        .await
        .expect("runs with the token");
    assert_eq!(outcome.sets[0].affected_rows, 1);
    let reused = gateway::run_query(&manager, &guarded, "tab", Some("shop".into()), insert.into(), Some(&confirmation)).await;
    assert_eq!(
        reused.unwrap_err().code(),
        Some(gateway::CONFIRMATION_INVALID),
        "tokens are single-use"
    );

    // Statement timeout from the rules (MariaDB max_statement_time).
    let mut slow = gate("rules-maria", Preset::Custom);
    slow.safety.rules.write = rules::Action::Allow;
    slow.safety.statement_timeout_secs = 1;
    let started = std::time::Instant::now();
    let timed_out = gateway::run_query(
        &manager,
        &slow,
        "tab",
        Some("shop".into()),
        "SELECT SUM(a.seq * b.seq) FROM seq_1_to_100000 a, seq_1_to_100000 b".into(),
        None,
    )
    .await;
    println!(
        "timeout after {:?}: {:?}",
        started.elapsed(),
        timed_out.as_ref().err().map(ToString::to_string)
    );
    assert!(
        timed_out.is_err() && started.elapsed().as_secs() < 10,
        "max_statement_time stops the query"
    );

    manager.close_session("tab").await;
    setup(&manager, "rules-maria", "shop", "DROP TABLE gate_probe").await;
    manager.disconnect("rules-maria").await;
}

/// Begin / Commit / Rollback and auto-commit report the engine's transaction state and roll back.
#[tokio::test]
#[ignore]
async fn live_transactions() {
    let cases = [
        (profile("tx-maria", "mariadb", 3306, "root", "shop", &[]), "studio-root", "shop"),
        (mssql_profile("tx-mssql"), "Studio#Pass2026", "ERP"),
    ];
    for (profile, password, database) in cases {
        let manager = ConnectionManager::default();
        manager
            .connect(&profile.id, profile.spec(), ConnectSecrets::password(password))
            .await
            .expect("connect");
        let id = profile.id.as_str();
        let create = if profile.driver == "mssql" {
            "IF OBJECT_ID('dbo.gate_probe') IS NULL CREATE TABLE dbo.gate_probe (id INT PRIMARY KEY); DELETE FROM dbo.gate_probe"
        } else {
            "CREATE TABLE IF NOT EXISTS gate_probe (id INT PRIMARY KEY); DELETE FROM gate_probe"
        };
        setup(&manager, id, database, create).await;
        manager.open_legacy_session(id, "tx".into()).await.expect("session");
        let context = gate(id, Preset::Open);
        let run = |sql: &'static str| gateway::run_query(&manager, &context, "tx", Some(database.into()), sql.into(), None);

        let idle = gateway::legacy_transaction(&manager, &context, "tx", TransactionOp::Status)
            .await
            .expect("status");
        assert!(idle.supported && !idle.open && idle.auto_commit, "{}: {idle:?}", profile.driver);
        let begun = gateway::legacy_transaction(&manager, &context, "tx", TransactionOp::Begin)
            .await
            .expect("begin");
        assert!(begun.open, "{}: begin opens a transaction", profile.driver);
        let inserted = run("INSERT INTO gate_probe VALUES (1)").await.expect("insert");
        assert!(inserted.transaction.open, "still open after the run");
        // A second Begin must be refused, not commit (MySQL) or nest (SQL Server) the open one.
        let nested = gateway::legacy_transaction(&manager, &context, "tx", TransactionOp::Begin)
            .await
            .expect_err("nested begin refused");
        assert_eq!(nested.to_string(), drivers::types::TRANSACTION_ALREADY_OPEN, "{}", profile.driver);
        let rolled = gateway::legacy_transaction(&manager, &context, "tx", TransactionOp::Rollback)
            .await
            .expect("rollback");
        assert!(!rolled.open);
        let count = run("SELECT COUNT(*) FROM gate_probe").await.expect("count");
        assert_eq!(count.sets[0].rows[0][0].as_deref(), Some("0"), "{}: rolled back", profile.driver);
        assert!(!count.transaction.open);

        // Auto-commit off: the first write opens a transaction implicitly.
        let manual = gateway::legacy_transaction(&manager, &context, "tx", TransactionOp::AutoCommitOff)
            .await
            .expect("autocommit off");
        assert!(!manual.auto_commit && !manual.open, "{}: {manual:?}", profile.driver);
        let pending = run("INSERT INTO gate_probe VALUES (2)").await.expect("insert");
        assert!(pending.transaction.open, "{}: implicit transaction", profile.driver);
        gateway::legacy_transaction(&manager, &context, "tx", TransactionOp::Commit)
            .await
            .expect("commit");
        let back = gateway::legacy_transaction(&manager, &context, "tx", TransactionOp::AutoCommitOn)
            .await
            .expect("autocommit on");
        assert!(back.auto_commit && !back.open);
        let count = run("SELECT COUNT(*) FROM gate_probe").await.expect("count");
        assert_eq!(count.sets[0].rows[0][0].as_deref(), Some("1"), "{}: committed", profile.driver);
        println!("{}: transaction states {idle:?} → {begun:?} → {rolled:?}", profile.driver);

        manager.close_session("tx").await;
        setup(&manager, id, database, "DROP TABLE gate_probe").await;
        manager.disconnect(id).await;
    }
}

/// MongoDB: Read-only refuses write commands, filters must be query documents, no transactions.
#[tokio::test]
#[ignore]
async fn live_gateway_mongo() {
    let manager = ConnectionManager::default();
    let profile = profile("gate-mongo", "mongodb", 27017, "root", "admin", &[]);
    manager
        .connect(&profile.id, profile.spec(), ConnectSecrets::password("studio-root"))
        .await
        .expect("connect");
    manager.open_legacy_session("gate-mongo", "m".into()).await.expect("session");
    let read_only = gate("gate-mongo", Preset::ReadOnly);
    let insert = r#"{ "insert": "clicks", "documents": [ { "type": "probe" } ] }"#;
    let blocked = gateway::run_query(&manager, &read_only, "m", Some("events".into()), insert.into(), None)
        .await
        .expect_err("blocked");
    assert_eq!(blocked.code(), Some(gateway::BLOCKED));
    let out = r#"{ "aggregate": "clicks", "pipeline": [ { "$out": "copy" } ], "cursor": {} }"#;
    assert!(
        gateway::run_query(&manager, &read_only, "m", Some("events".into()), out.into(), None)
            .await
            .is_err()
    );
    let read = gateway::run_query(
        &manager,
        &read_only,
        "m",
        Some("events".into()),
        r#"{ "count": "clicks" }"#.into(),
        None,
    )
    .await
    .expect("count runs");
    assert!(!read.transaction.supported);
    let request = BrowseRequest {
        connection_id: "gate-mongo".into(),
        ..browse("events", "clicks", Some(r#"{ "$where": "sleep(100) || true" }"#))
    };
    let error = gateway::legacy_browse(&manager, &read_only, &request)
        .await
        .expect_err("$where rejected");
    assert_eq!(error.code(), Some(gateway::filter::INVALID_FILTER));
    manager.close_session("m").await;
    manager.disconnect("gate-mongo").await;
}

/// Writes into the real Downloads folder (twice, to check the " (2)" suffix), then cleans up.
#[test]
#[ignore]
fn live_save_download() {
    let first = crate::files::save_download("db-studio-selftest.csv", b"a,b\n1,2\n").expect("save");
    let second = crate::files::save_download("db-studio-selftest.csv", b"x").expect("save again");
    println!("{first}\n{second}");
    assert!(second.ends_with("db-studio-selftest (2).csv"));
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "a,b\n1,2\n");
    std::fs::remove_file(first).unwrap();
    std::fs::remove_file(second).unwrap();
}
