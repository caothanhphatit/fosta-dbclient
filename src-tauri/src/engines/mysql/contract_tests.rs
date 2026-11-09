//! Contract suites for MariaDB against `dev/compose/mariadb.yml` (data from `dev/seed/mariadb/contract.sql`), plus
//! MariaDB-specific checks on the `shop` demo database (`dev/seed/mariadb.sql`): typed values, transactions, the schema
//! graph and the vector operations. Override the server with FOSTA_CONTRACT_MARIADB_HOST / _PORT / _USER / _PASSWORD.

use std::{sync::Arc, time::Duration};

use crate::{
    contract::{
        session::TRANSACTION_ALREADY_OPEN,
        testing::{suites::*, vector::*},
        BrowseRequest, CallContext, Category, CollectSink, ConnectSecrets, ConnectionSpec, Field, Filter, NodeKind, ObjectRef,
        RawFilterLang, RowChange, RowChangeSet, RunOptions, RunRequest, Session, SessionOptions, TransactionOp, TreeRequest, Value,
        ValueKind,
    },
    error::AppResult,
    gateway::{
        self,
        rules::{Preset, SafetySettings},
        Context,
    },
    runtime::{ConnectionManager, Handle},
};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_MARIADB_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn seed() -> Vec<String> {
    include_str!("../../../../dev/seed/mariadb/contract.sql")
        .split("\nGO\n")
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::Sql,
        text: format!("`id` = {key}"),
    }
}

pub(super) fn spec(database: Option<&str>) -> ConnectionSpec {
    ConnectionSpec {
        engine: "mariadb".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "3306").parse().unwrap_or(3306),
        user: env("USER", "root"),
        database: database.map(str::to_owned),
        ..ConnectionSpec::default()
    }
}

pub(super) fn secrets() -> ConnectSecrets {
    ConnectSecrets::password(&env("PASSWORD", "studio-root"))
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "mariadb",
        // The seed creates the database, so connect without one.
        spec: spec(None),
        secrets: secrets(),
        table: ObjectRef::new(Some("fosta_contract"), None, "contract_items", NodeKind::TABLE),
        child: ObjectRef::new(Some("fosta_contract"), None, "contract_children", NodeKind::TABLE),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("SELECT SLEEP(30)"),
        syntax_error: "SELEC * FORM contract_items",
        constraint_violation: "INSERT INTO fosta_contract.contract_items (id, name, price, active) VALUES (1, 'dup', 1, 1)",
        multi_statement: "SELECT 1; SELECT id, name FROM fosta_contract.contract_items ORDER BY id LIMIT 3",
        insert_probe: "INSERT INTO fosta_contract.contract_items (id, name, price, active) VALUES (5000, 'probe', 1, 1)",
        count_probe: "SELECT COUNT(*) FROM fosta_contract.contract_items WHERE id = 5000",
        classify_samples: &[
            ("SELECT * FROM t", Category::Read),
            ("UPDATE t SET a = 1 WHERE id = 2", Category::Write),
            ("DELETE FROM t", Category::Destructive),
            ("CREATE TABLE x (id INT)", Category::Ddl),
            ("GRANT ALL ON *.* TO x", Category::Admin),
        ],
        language: "sql",
        gaps: &[],
    }
}

crate::contract_tests!(contract_mariadb, fixture());

// --- MariaDB-specific checks (ported from live_tests.rs) ----------------------------------------------------------------

async fn shop() -> (ConnectionManager, Handle) {
    let manager = ConnectionManager::default();
    manager.connect("shop", spec(Some("shop")), secrets()).await.expect("connect");
    let handle = manager.handle("shop").unwrap();
    (manager, handle)
}

async fn session(handle: &Handle) -> Arc<dyn Session> {
    handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("session")
        .session
}

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str, options: RunOptions) -> AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: Some("shop".into()),
        offset: 0,
    };
    handle.run(Arc::clone(session), request, options, &mut sink).await?;
    Ok(sink)
}

async fn browse(handle: &Handle, request: BrowseRequest) -> crate::contract::Page {
    handle
        .call("browsed", |conn| async move {
            conn.tabular().unwrap().browse(&request, &CallContext::default()).await
        })
        .await
        .expect("browse")
}

fn shop_table(name: &str) -> ObjectRef {
    ObjectRef::new(Some("shop"), None, name, NodeKind::TABLE)
}

fn column<'a>(page: &'a crate::contract::Page, row: usize, name: &str) -> &'a Value {
    let index = page
        .columns
        .iter()
        .position(|c| c.name == name)
        .unwrap_or_else(|| panic!("no column {name}"));
    &page.rows[row][index]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn contract_mariadb_shop_catalog_and_edits() {
    let (manager, handle) = shop().await;
    let info = manager.connect_info("shop").unwrap();
    assert!(info.server.label.starts_with("MariaDB "), "{}", info.server.label);
    let ctx = CallContext::default();
    let top = handle
        .call("listed", |conn| async move {
            conn.children(&TreeRequest::default(), &CallContext::default()).await
        })
        .await
        .unwrap();
    let shop_node = top.nodes.iter().find(|node| node.label == "shop").expect("shop listed").clone();
    assert!(top
        .nodes
        .iter()
        .any(|node| node.label == "mysql" && node.detail.as_deref() == Some("system")));
    let request = TreeRequest {
        parent: Some(shop_node.key),
        ..TreeRequest::default()
    };
    let tables = handle
        .call(
            "listed",
            |conn| async move { conn.children(&request, &CallContext::default()).await },
        )
        .await
        .unwrap();
    // A table with rows never shows a stale "~0" estimate (InnoDB statistics lag after the bulk seed).
    let order_items = tables.nodes.iter().find(|node| node.label == "order_items").expect("order_items");
    assert_ne!(order_items.detail.as_deref(), Some("~0 rows"));
    assert_eq!(order_items.key.id, "db:shop/table:order_items");

    let products = handle
        .call(
            "described",
            |conn| async move { conn.describe(&shop_table("products"), &ctx).await },
        )
        .await
        .unwrap();
    assert!(!products.foreign_keys.is_empty() && !products.referenced_by.is_empty());
    assert_eq!(products.column("embedding").unwrap().hints.vector_dims, Some(4));
    assert!(products.ddl.as_deref().unwrap_or_default().contains("CREATE TABLE"));
    assert!(products.indexes.iter().any(|index| index.primary));
    assert!(products.extra.iter().any(|(label, _)| label == "Engine"));

    // Declared SQL types on table pages; a raw filter narrows the page.
    let mut request = BrowseRequest::new(shop_table("order_items"), 5);
    request.estimate_total = true;
    let page = browse(&handle, request).await;
    let id = page.columns.iter().find(|c| c.name == "id").unwrap();
    assert!(
        id.type_name.starts_with("int") || id.type_name.starts_with("bigint"),
        "{}",
        id.type_name
    );
    assert_eq!(page.rows.len(), 5);
    let all = browse(&handle, {
        let mut request = BrowseRequest::new(shop_table("products"), 5);
        request.exact_total = true;
        request
    })
    .await;
    let filtered = browse(&handle, {
        let mut request = BrowseRequest::new(shop_table("products"), 5);
        request.filter = Filter::Raw {
            language: RawFilterLang::Sql,
            text: "stock < 5".into(),
        };
        request.exact_total = true;
        request
    })
    .await;
    assert!(filtered.total.unwrap().value < all.total.unwrap().value);
    assert!(matches!(column(&all, 0, "embedding"), Value::Other { type_name, .. } if type_name == "vector"));

    // Query results carry SQL type names and typed values.
    let tab = session(&handle).await;
    let typed = run(
        &handle,
        &tab,
        "SELECT 1 AS n, CAST(1.5 AS DECIMAL(4,2)) AS d, CAST('2025-01-02' AS DATE) AS t",
        RunOptions::default(),
    )
    .await
    .unwrap();
    let names: Vec<&str> = typed.sets[0].columns.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(names, ["int", "decimal", "date"]);
    assert_eq!(typed.sets[0].rows[0][0], Value::Int(1));
    assert_eq!(typed.sets[0].rows[0][1], Value::Decimal("1.50".into()));
    assert_eq!(typed.sets[0].rows[0][2].display().as_deref(), Some("2025-01-02"));
    let grouped = run(
        &handle,
        &tab,
        "SELECT category_id, COUNT(*) FROM products GROUP BY category_id",
        RunOptions::default(),
    )
    .await
    .unwrap();
    assert!(!grouped.sets[0].rows.is_empty());

    // New database, dropped again.
    handle
        .call("created", |conn| async move {
            conn.admin()
                .unwrap()
                .create_database("studio_scratch", None, &CallContext::default())
                .await
        })
        .await
        .expect("create database");
    run(&handle, &tab, "DROP DATABASE `studio_scratch`", RunOptions::default())
        .await
        .expect("drop");
    tab.close().await;

    // An update that writes a different value, then restores it; each guarded by what it read.
    let mut request = BrowseRequest::new(shop_table("categories"), 1);
    request.filter = key_filter(8);
    let name = column(&browse(&handle, request).await, 0, "name").clone();
    for (from, to) in [
        (name.clone(), Value::Text("Outdoor (edited)".into())),
        (Value::Text("Outdoor (edited)".into()), name),
    ] {
        let set = RowChangeSet {
            object: shop_table("categories"),
            changes: vec![RowChange::Update {
                key: vec![Field::new("id", Value::Int(8))],
                values: vec![Field::new("name", to)],
                original: vec![Field::new("name", from)],
                version: None,
                force: false,
            }],
        };
        let result = handle
            .call("applied", |conn| async move {
                conn.tabular().unwrap().apply(&set, &CallContext::default()).await
            })
            .await
            .expect("apply");
        assert_eq!(result.updated, 1);
    }
    manager.disconnect("shop").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn contract_mariadb_typed_values() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let mut request = BrowseRequest::new(f.table.clone(), 1);
    request.filter = key_filter(6);
    let page = browse(&handle, request.clone()).await;
    let kinds: Vec<(&str, &ValueKind)> = page.columns.iter().map(|c| (c.name.as_str(), &c.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("id", &ValueKind::Int),
            ("name", &ValueKind::Text),
            ("price", &ValueKind::Decimal),
            ("active", &ValueKind::Bool),
            ("born", &ValueKind::Date),
            ("seen", &ValueKind::Timestamp),
            ("meta", &ValueKind::Json),
            ("blob_data", &ValueKind::Bytes),
            ("note", &ValueKind::Text),
        ]
    );
    let row = &page.rows[0];
    assert_eq!(row[0], Value::Int(6));
    assert_eq!(row[1], Value::Text("item 0006".into()));
    assert_eq!(row[2], Value::Decimal("7.50".into()));
    assert_eq!(row[3], Value::Bool(true));
    assert_eq!(row[4].display().as_deref(), Some("2020-01-07"));
    assert!(matches!(row[5], Value::Timestamp(_)));
    assert_eq!(row[5].display().as_deref(), Some("2026-01-01 08:06:00.250"));
    assert!(matches!(&row[6], Value::Json(text) if text.contains("\"n\"")));
    assert_eq!(row[7], Value::Bytes(vec![0, 0, 0, 6]));
    assert_eq!(row[8], Value::Null);

    // The same row through a query tab (text protocol) reads the same, JSON aside (MariaDB sends it as text).
    let tab = session(&handle).await;
    let mut sink = CollectSink::default();
    let query = RunRequest {
        text: "SELECT * FROM fosta_contract.contract_items WHERE id = 6".into(),
        database: None,
        offset: 0,
    };
    handle.run(Arc::clone(&tab), query, RunOptions::default(), &mut sink).await.unwrap();
    let queried = &sink.sets[0].rows[0];
    for index in [0, 1, 2, 3, 4, 5, 7, 8] {
        assert_eq!(queried[index], row[index], "column {index}");
    }
    tab.close().await;

    // Typed values round-trip through an edit, and text typed into a typed column is parsed.
    let original: Vec<Field> = page.columns.iter().zip(row).map(|(c, v)| Field::new(&c.name, v.clone())).collect();
    let set = RowChangeSet {
        object: f.table.clone(),
        changes: vec![RowChange::Update {
            key: vec![Field::new("id", Value::Int(6))],
            values: vec![
                Field::new("active", Value::Bool(false)),
                Field::new("born", Value::Text("2021-02-03".into())),
                Field::new("seen", row[5].clone()),
                Field::new("blob_data", Value::Text("0xcafe".into())),
                Field::new("meta", Value::Json(r#"{"n":6}"#.into())),
            ],
            original: original[3..8].to_vec(),
            version: None,
            force: false,
        }],
    };
    let result = handle
        .call("applied", |conn| async move {
            conn.tabular().unwrap().apply(&set, &CallContext::default()).await
        })
        .await
        .expect("typed update");
    assert_eq!(result.updated, 1);
    let after = browse(&handle, request).await;
    assert_eq!(after.rows[0][3], Value::Bool(false));
    assert_eq!(after.rows[0][4].display().as_deref(), Some("2021-02-03"));
    assert_eq!(after.rows[0][5], row[5]);
    assert_eq!(after.rows[0][7], Value::Bytes(vec![0xca, 0xfe]));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn contract_mariadb_transactions_and_backstops() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    let options = RunOptions::default();
    let count = |tab: Arc<dyn Session>| {
        let handle = handle.clone();
        async move {
            let sink = run(
                &handle,
                &tab,
                "SELECT COUNT(*) FROM fosta_contract.contract_children WHERE id > 900",
                RunOptions::default(),
            )
            .await
            .unwrap();
            sink.sets[0].rows[0][0].clone()
        }
    };
    let idle = tab.transaction(TransactionOp::Status, &options).await.unwrap();
    assert!(idle.supported && !idle.open && idle.auto_commit, "{idle:?}");
    assert!(tab.transaction(TransactionOp::Begin, &options).await.unwrap().open);
    run(
        &handle,
        &tab,
        "INSERT INTO fosta_contract.contract_children VALUES (901, 1, 'tx')",
        options.clone(),
    )
    .await
    .unwrap();
    assert!(tab.transaction_state().await.open, "still open after the run");
    // A second Begin must be refused, not commit the open one.
    let nested = tab.transaction(TransactionOp::Begin, &options).await.expect_err("nested begin");
    assert_eq!(nested.to_string(), TRANSACTION_ALREADY_OPEN);
    assert!(!tab.transaction(TransactionOp::Rollback, &options).await.unwrap().open);
    assert_eq!(count(Arc::clone(&tab)).await, Value::Int(0));
    // Auto-commit off: the first write opens a transaction implicitly.
    let manual = tab.transaction(TransactionOp::AutoCommitOff, &options).await.unwrap();
    assert!(!manual.auto_commit && !manual.open, "{manual:?}");
    run(
        &handle,
        &tab,
        "INSERT INTO fosta_contract.contract_children VALUES (902, 1, 'tx')",
        options.clone(),
    )
    .await
    .unwrap();
    assert!(tab.transaction_state().await.open, "implicit transaction");
    tab.transaction(TransactionOp::Commit, &options).await.unwrap();
    let back = tab.transaction(TransactionOp::AutoCommitOn, &options).await.unwrap();
    assert!(back.auto_commit && !back.open);
    assert_eq!(count(Arc::clone(&tab)).await, Value::Int(1));

    // Read-only session: the server refuses the write even without the gateway.
    let read_only = RunOptions {
        read_only: true,
        ..RunOptions::default()
    };
    let refused = run(
        &handle,
        &tab,
        "INSERT INTO fosta_contract.contract_children VALUES (903, 1, 'ro')",
        read_only,
    )
    .await
    .expect_err("READ ONLY session");
    assert_eq!(refused.code(), Some("ENGINE_ERROR"));
    assert_eq!(refused.field("statement").and_then(|s| s["index"].as_u64()), Some(0));
    // Statement timeout (max_statement_time).
    let slow = RunOptions {
        timeout: Some(Duration::from_secs(1)),
        ..RunOptions::default()
    };
    let started = std::time::Instant::now();
    let timed_out = run(
        &handle,
        &tab,
        "SELECT SUM(a.seq * b.seq) FROM seq_1_to_100000 a, seq_1_to_100000 b",
        slow,
    )
    .await
    .expect_err("timed out");
    assert_eq!(timed_out.code(), Some("TIMEOUT"), "{timed_out:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
    // Errors name the failing statement of a script.
    let failed = run(&handle, &tab, "SELECT 1; SELEC 2", RunOptions::default())
        .await
        .expect_err("second fails");
    assert_eq!(failed.field("statement").and_then(|s| s["index"].as_u64()), Some(1), "{failed:?}");
    tab.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn contract_mariadb_read_only_is_reasserted_every_run() {
    let (_manager, handle) = shop().await;
    let tab = session(&handle).await;
    let read_only = RunOptions {
        read_only: true,
        ..RunOptions::default()
    };
    let session_read_only = |sink: CollectSink| sink.sets.last().unwrap().rows[0][0].clone();
    // Statements that lift the session's read-only mode (classified Admin; here run past the gateway) do not survive into
    // the next read-only run.
    for lift in [
        "SET SESSION TRANSACTION READ WRITE",
        "SET SESSION `tx_read_only` = 0",
        "SET @@session.tx_read_only = OFF",
    ] {
        run(&handle, &tab, lift, read_only.clone()).await.unwrap();
        let after = run(&handle, &tab, "SELECT @@session.tx_read_only", read_only.clone())
            .await
            .unwrap();
        assert_eq!(session_read_only(after), Value::Int(1), "after {lift}");
    }
    // Inside a read-only transaction the re-assertion is accepted.
    assert!(tab.transaction(TransactionOp::Begin, &read_only).await.unwrap().open);
    run(&handle, &tab, "SELECT 1", read_only.clone()).await.unwrap();
    run(&handle, &tab, "SELECT 2", read_only.clone()).await.unwrap();
    tab.transaction(TransactionOp::Rollback, &read_only).await.unwrap();
    // Read-write runs switch it off again.
    let back = run(&handle, &tab, "SELECT @@session.tx_read_only", RunOptions::default())
        .await
        .unwrap();
    assert_eq!(session_read_only(back), Value::Int(0));
    tab.close().await;
}

/// The diagram's keys agree with `describe` (an independent catalog path): each table's primary key and each foreign key.
async fn assert_graph_matches_descriptions(handle: &Handle, graph: &crate::contract::SchemaGraph, object: impl Fn(&str) -> ObjectRef) {
    for table in graph.tables.iter().filter(|table| table.kind == "table") {
        let target = object(&table.name);
        let described = handle
            .call(
                "described",
                |conn| async move { conn.describe(&target, &CallContext::default()).await },
            )
            .await
            .unwrap();
        let mut graph_keys: Vec<&str> = table.columns.iter().filter(|c| c.primary).map(|c| c.name.as_str()).collect();
        let mut described_keys: Vec<&str> = described
            .primary_key
            .iter()
            .flat_map(|key| key.columns.iter().map(String::as_str))
            .collect();
        graph_keys.sort_unstable();
        described_keys.sort_unstable();
        assert_eq!(graph_keys, described_keys, "{}", table.name);
        for relation in graph.relations.iter().filter(|relation| relation.table == table.name) {
            let referenced = relation.referenced_table.rsplit('.').next().unwrap_or_default();
            assert!(
                described.foreign_keys.iter().any(|key| key.columns == relation.columns
                    && key.ref_table == referenced
                    && key.ref_columns == relation.referenced_columns),
                "{relation:?} not in {:?}",
                described.foreign_keys
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn contract_mariadb_schema_graph() {
    let (manager, handle) = shop().await;
    let graph = handle
        .call("read the schema", |conn| async move {
            conn.schema_graph().unwrap().schema_graph("shop", &CallContext::default()).await
        })
        .await
        .unwrap();
    assert_eq!(graph.relations.len(), 4, "{:?}", graph.relations);
    assert!(graph.tables.iter().flat_map(|t| &t.columns).any(|c| c.primary));
    assert_graph_matches_descriptions(&handle, &graph, shop_table).await;
    manager.disconnect("shop").await;
}

/// The shared vector suite on `shop.products` (VECTOR(4), cosine MHNSW index), plus MariaDB's own statement text.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn contract_mariadb_vector() {
    let vectors = VectorFixture {
        database: "shop",
        table: "products",
        column: "embedding",
        dims: 4,
        key_column: "id",
        key: 1,
        indexed: true,
    };
    let Some(run) = suite_vector(&fixture(), &vectors).await else {
        return;
    };
    assert_eq!(run.info["columns"][0]["typeName"], "VECTOR(4)");
    assert_eq!(run.info["indexes"][0]["metric"], "cosine");
    assert_eq!(run.info["indexes"][0]["kind"], "mhnsw");
    assert!(run.search["sql"].as_str().unwrap().contains("VEC_DISTANCE_COSINE"));
    // Typed result sets: the distance is a float, the id an int; vector columns are not selected.
    let columns = run.search["result"]["columns"].as_array().unwrap();
    assert_eq!(run.search["result"]["rows"][0][0]["t"], "float");
    let id = columns.iter().position(|column| column["name"] == "id").unwrap();
    assert_eq!(run.search["result"]["rows"][0][id]["t"], "int");
    assert!(columns.iter().all(|column| column["name"] != "embedding"));
}

// --- The statement gateway on a live server (ported from live_tests.rs) ---------------------------------------------------

const INJECTED_FILTER: &str = "1=0; DELETE FROM gate_probe; SELECT 1 WHERE 1";

fn gate(preset: Preset) -> Context {
    Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: "shop".into(),
        connection_name: "shop".into(),
        safety: SafetySettings::preset(preset),
    }
}

fn raw_filter(text: &str) -> Filter {
    Filter::Raw {
        language: RawFilterLang::Sql,
        text: text.to_owned(),
    }
}

/// Creates `shop.gate_probe` with the given rows, outside the gateway (the test's own fixture).
async fn gate_probe(handle: &Handle, rows: &str) {
    let tab = session(handle).await;
    let setup = format!("CREATE TABLE IF NOT EXISTS gate_probe (id INT PRIMARY KEY); DELETE FROM gate_probe; {rows}");
    run(handle, &tab, setup.trim_end_matches("; "), RunOptions::default())
        .await
        .expect("setup");
    tab.close().await;
}

async fn drop_gate_probe(handle: &Handle) {
    let tab = session(handle).await;
    run(handle, &tab, "DROP TABLE gate_probe", RunOptions::default())
        .await
        .expect("drop");
    tab.close().await;
}

/// A filter that tries to stack statements is refused by the gateway, and even the engine alone cannot run it (browse
/// queries are prepared statements); the probe row survives.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn contract_mariadb_gateway_rejects_injected_filters() {
    let (manager, handle) = shop().await;
    gate_probe(&handle, "INSERT INTO gate_probe VALUES (1)").await;
    let context = gate(Preset::Open);
    let request = |filter: &str| {
        let mut request = BrowseRequest::new(shop_table("gate_probe"), 10);
        request.filter = raw_filter(filter);
        request
    };
    let error = gateway::browse(&manager, &context, "shop", &request(INJECTED_FILTER))
        .await
        .expect_err("injected filter must fail");
    assert_eq!(error.code(), Some(gateway::filter::INVALID_FILTER), "{error}");
    let direct = request(INJECTED_FILTER);
    let bypass = handle
        .call("browsed", |conn| async move {
            conn.tabular().unwrap().browse(&direct, &CallContext::default()).await
        })
        .await;
    assert!(bypass.is_err(), "a prepared statement cannot carry a second statement");
    let fine = gateway::browse(&manager, &context, "shop", &request("id = 1"))
        .await
        .expect("valid filter");
    assert_eq!(fine.rows.len(), 1, "the probe row survived");
    drop_gate_probe(&handle).await;
    manager.disconnect("shop").await;
}

/// Read-only blocks writes; Guarded asks for a confirmation whose token runs the statement once; the rules' statement
/// timeout reaches the server.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mariadb.yml"]
async fn contract_mariadb_gateway_rules_and_confirmation() {
    let (manager, handle) = shop().await;
    gate_probe(&handle, "").await;
    manager
        .open_session("shop", "tab".into(), SessionOptions::default())
        .await
        .expect("session");
    let request = |text: &str| RunRequest {
        text: text.to_owned(),
        database: Some("shop".into()),
        offset: 0,
    };
    let insert = request("INSERT INTO gate_probe VALUES (7)");
    let mut sink = CollectSink::default();

    let read_only = gate(Preset::ReadOnly);
    let blocked = gateway::run(&manager, &read_only, "tab", &insert, None, &mut sink)
        .await
        .expect_err("read-only blocks INSERT");
    assert_eq!(blocked.code(), Some(gateway::BLOCKED), "{blocked}");
    let mut counted = CollectSink::default();
    gateway::run(
        &manager,
        &read_only,
        "tab",
        &request("SELECT COUNT(*) FROM gate_probe"),
        None,
        &mut counted,
    )
    .await
    .expect("reads still run");
    assert_eq!(counted.sets[0].rows[0][0], Value::Int(0));

    let guarded = gate(Preset::Guarded);
    let asked = gateway::run(&manager, &guarded, "tab", &insert, None, &mut sink)
        .await
        .expect_err("guarded asks first");
    assert_eq!(asked.code(), Some(gateway::CONFIRMATION_REQUIRED));
    let token = asked.payload().unwrap()["token"].as_str().unwrap().to_owned();
    let confirmation = gateway::Confirmation { token, typed: None };
    let mut inserted = CollectSink::default();
    gateway::run(&manager, &guarded, "tab", &insert, Some(&confirmation), &mut inserted)
        .await
        .expect("runs with the token");
    assert_eq!(inserted.sets[0].affected, Some(1));
    let reused = gateway::run(&manager, &guarded, "tab", &insert, Some(&confirmation), &mut sink).await;
    assert_eq!(
        reused.unwrap_err().code(),
        Some(gateway::CONFIRMATION_INVALID),
        "tokens are single-use"
    );

    let mut slow = gate(Preset::Custom);
    slow.safety.rules.write = gateway::rules::Action::Allow;
    slow.safety.statement_timeout_secs = 1;
    let started = std::time::Instant::now();
    let timed_out = gateway::run(
        &manager,
        &slow,
        "tab",
        &request("SELECT SUM(a.seq * b.seq) FROM seq_1_to_100000 a, seq_1_to_100000 b"),
        None,
        &mut sink,
    )
    .await
    .expect_err("max_statement_time stops the query");
    assert_eq!(timed_out.code(), Some("TIMEOUT"), "{timed_out:?}");
    assert!(started.elapsed() < Duration::from_secs(10));

    manager.close_session("tab").await;
    drop_gate_probe(&handle).await;
    manager.disconnect("shop").await;
}
