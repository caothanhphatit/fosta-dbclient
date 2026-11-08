//! Contract suites for SQL Server against `dev/compose/mssql.yml` (data from `dev/seed/mssql/contract.sql`), plus
//! SQL Server checks on the `ERP` demo database (`dev/seed/mssql.sql`): catalog, batches with row counts, transactions,
//! the schema graph, typed values, edits on a table without rowversion, the statement timeout and TLS errors.
//! Override the server with FOSTA_CONTRACT_MSSQL_HOST / _PORT / _USER / _PASSWORD.

use std::{sync::Arc, time::Duration};

use crate::{
    contract::{
        errors::CONFLICT, session::TRANSACTION_ALREADY_OPEN, testing::suites::*, BrowseRequest, CallContext, Category, CollectSink,
        ConnectSecrets, ConnectionSpec, Field, Filter, NodeKind, ObjectRef, Page, RawFilterLang, RowChange, RowChangeSet, RunOptions,
        RunRequest, Session, SessionOptions, TlsConfig, TlsMode, TransactionOp, TreeRequest, Value, ValueKind,
    },
    error::AppResult,
    runtime::{ConnectionManager, Handle},
};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_MSSQL_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn seed() -> Vec<String> {
    include_str!("../../../../dev/seed/mssql/contract.sql")
        .replace("\r\n", "\n")
        .split("\nGO\n")
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::Sql,
        text: format!("[id] = {key}"),
    }
}

/// The dev server's self-signed certificate is trusted explicitly.
fn spec(database: &str, trust: bool) -> ConnectionSpec {
    ConnectionSpec {
        engine: "mssql".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "1433").parse().unwrap_or(1433),
        user: env("USER", "sa"),
        database: Some(database.into()),
        tls: Some(TlsConfig {
            mode: TlsMode::Require,
            accept_invalid_certs: trust,
            ..TlsConfig::default()
        }),
        ..ConnectionSpec::default()
    }
}

fn secrets() -> ConnectSecrets {
    ConnectSecrets::password(&env("PASSWORD", "Studio#Pass2026"))
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "mssql",
        // The seed creates the database, so connect to master.
        spec: spec("master", true),
        secrets: secrets(),
        table: ObjectRef::new(Some("fosta_contract"), Some("dbo"), "contract_items", NodeKind::TABLE),
        child: ObjectRef::new(Some("fosta_contract"), Some("dbo"), "contract_children", NodeKind::TABLE),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("WAITFOR DELAY '00:00:30'"),
        syntax_error: "SELEC * FORM contract_items",
        constraint_violation: "INSERT INTO fosta_contract.dbo.contract_items (id, name, price, active) VALUES (1, 'dup', 1, 1)",
        multi_statement: "SELECT 1 AS one; SELECT TOP 3 id, name FROM fosta_contract.dbo.contract_items ORDER BY id",
        insert_probe: "INSERT INTO fosta_contract.dbo.contract_items (id, name, price, active) VALUES (5000, 'probe', 1, 1)",
        count_probe: "SELECT COUNT(*) AS n FROM fosta_contract.dbo.contract_items WHERE id = 5000",
        classify_samples: &[
            ("SELECT TOP 5 * FROM t", Category::Read),
            ("UPDATE t SET a = 1 WHERE id = 2", Category::Write),
            ("TRUNCATE TABLE t", Category::Destructive),
            ("CREATE TABLE x (id INT)", Category::Ddl),
            ("BEGIN TRANSACTION", Category::Transaction),
        ],
        language: "sql",
        gaps: &[],
    }
}

crate::contract_tests!(contract_mssql, fixture());

// --- SQL Server checks (ported from live_tests.rs) ---------------------------------------------------------------------

async fn erp() -> (ConnectionManager, Handle) {
    let manager = ConnectionManager::default();
    manager.connect("erp", spec("ERP", true), secrets()).await.expect("connect");
    let handle = manager.handle("erp").unwrap();
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

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str, database: &str, options: RunOptions) -> AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: Some(database.into()),
        offset: 0,
    };
    handle.run(Arc::clone(session), request, options, &mut sink).await?;
    Ok(sink)
}

async fn browse(handle: &Handle, request: BrowseRequest) -> Page {
    handle
        .call("browsed", |conn| async move {
            conn.tabular().unwrap().browse(&request, &CallContext::default()).await
        })
        .await
        .expect("browse")
}

async fn apply(handle: &Handle, set: RowChangeSet) -> AppResult<crate::contract::ApplyResult> {
    handle
        .call("applied", |conn| async move {
            conn.tabular().unwrap().apply(&set, &CallContext::default()).await
        })
        .await
}

fn table(database: &str, schema: &str, name: &str) -> ObjectRef {
    ObjectRef::new(Some(database), Some(schema), name, NodeKind::TABLE)
}

fn raw(text: &str) -> Filter {
    Filter::Raw {
        language: RawFilterLang::Sql,
        text: text.into(),
    }
}

fn column<'a>(page: &'a Page, row: usize, name: &str) -> &'a Value {
    let index = page
        .columns
        .iter()
        .position(|c| c.name == name)
        .unwrap_or_else(|| panic!("no column {name}"));
    &page.rows[row][index]
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mssql.yml"]
async fn contract_mssql_erp_catalog_and_edits() {
    let (manager, handle) = erp().await;
    let info = manager.connect_info("erp").unwrap();
    assert!(info.server.label.starts_with("SQL Server "), "{}", info.server.label);
    let top = handle
        .call("listed", |conn| async move {
            conn.children(&TreeRequest::default(), &CallContext::default()).await
        })
        .await
        .unwrap();
    assert!(top
        .nodes
        .iter()
        .any(|node| node.label == "master" && node.detail.as_deref() == Some("system")));
    let erp_node = top.nodes.iter().find(|node| node.label == "ERP").expect("ERP listed").clone();
    let request = TreeRequest {
        parent: Some(erp_node.key),
        ..TreeRequest::default()
    };
    let tables = handle
        .call(
            "listed",
            |conn| async move { conn.children(&request, &CallContext::default()).await },
        )
        .await
        .unwrap();
    let invoices = tables
        .nodes
        .iter()
        .find(|node| node.label == "sales.Invoices")
        .expect("sales.Invoices");
    assert_eq!(invoices.key.id, "db:ERP/schema:sales/table:Invoices");
    assert_eq!(invoices.object.as_ref().unwrap().schema.as_deref(), Some("sales"));
    assert_eq!(invoices.detail.as_deref(), Some("~5000 rows"));
    let warehouses = tables
        .nodes
        .iter()
        .find(|node| node.label == "Warehouses")
        .expect("dbo tables without schema");
    assert_eq!(warehouses.object.as_ref().unwrap().schema.as_deref(), Some("dbo"));
    assert!(tables
        .nodes
        .iter()
        .any(|node| node.label == "sales.vMonthlyRevenue" && node.kind == NodeKind::VIEW));

    let described = handle
        .call("described", |conn| async move {
            conn.describe(&table("ERP", "sales", "Invoices"), &CallContext::default()).await
        })
        .await
        .unwrap();
    assert_eq!(described.foreign_keys.len(), 1);
    assert_eq!(described.foreign_keys[0].ref_table, "Customers");
    assert_eq!(described.foreign_keys[0].ref_schema.as_deref(), Some("sales"));
    assert_eq!(described.column("InvoiceId").unwrap().identity.as_deref(), Some("IDENTITY(1,1)"));
    assert!(described.column("InvoiceId").unwrap().hints.read_only);
    assert_eq!(described.column("Amount").unwrap().type_name, "decimal(14,2)");
    assert!(described.unique_keys.iter().any(|key| key.columns == ["InvoiceNo"]));
    assert!(described.row_estimate.unwrap_or(0) >= 5000);
    let ddl = described.ddl.as_deref().unwrap_or_default();
    assert!(ddl.starts_with("CREATE TABLE [sales].[Invoices]"), "{ddl}");
    assert!(ddl.contains("REFERENCES [sales].[Customers] ([CustomerId])"), "{ddl}");
    let customers = handle
        .call("described", |conn| async move {
            conn.describe(&table("ERP", "sales", "Customers"), &CallContext::default()).await
        })
        .await
        .unwrap();
    assert!(customers.referenced_by.iter().any(|key| key.table == "Invoices"));
    let view = handle
        .call("described", |conn| async move {
            conn.describe(
                &ObjectRef::new(Some("ERP"), Some("sales"), "vMonthlyRevenue", NodeKind::VIEW),
                &CallContext::default(),
            )
            .await
        })
        .await
        .unwrap();
    assert_eq!(view.object.kind, NodeKind::VIEW);
    assert!(view.ddl.as_deref().unwrap_or_default().contains("CREATE VIEW"));

    // Raw filters narrow the page, also with bracket quoting (src/core/sql.ts, tsql dialect); declared SQL types.
    let all = browse(&handle, {
        let mut request = BrowseRequest::new(table("ERP", "sales", "Invoices"), 5);
        request.exact_total = true;
        request
    })
    .await;
    assert_eq!(all.rows.len(), 5);
    assert_eq!(all.total.unwrap().value, 5000);
    assert!(all.row_versions.is_none(), "no rowversion column");
    for filter in ["Status = 'paid'", "([Status] LIKE N'%paid%') AND ([InvoiceId] > 0)"] {
        let filtered = browse(&handle, {
            let mut request = BrowseRequest::new(table("ERP", "sales", "Invoices"), 5);
            request.filter = raw(filter);
            request.exact_total = true;
            request
        })
        .await;
        let total = filtered.total.unwrap().value;
        assert!(total > 0 && total < 5000, "{filter}: {total}");
    }
    let id = all.columns.iter().find(|column| column.name == "InvoiceId").unwrap();
    assert_eq!((id.type_name.as_str(), &id.kind), ("int", &ValueKind::Int));
    // An ObjectRef without a schema addresses schema.name as the pre-contract driver did.
    let dotted = browse(
        &handle,
        BrowseRequest::new(ObjectRef::new(Some("ERP"), None, "sales.Invoices", NodeKind::TABLE), 1),
    )
    .await;
    assert_eq!(dotted.rows.len(), 1);

    // New database, dropped again.
    let tab = session(&handle).await;
    handle
        .call("created", |conn| async move {
            conn.admin()
                .unwrap()
                .create_database("StudioScratch", None, &CallContext::default())
                .await
        })
        .await
        .expect("create database");
    run(&handle, &tab, "DROP DATABASE [StudioScratch]", "master", RunOptions::default())
        .await
        .expect("drop");
    let typed = run(
        &handle,
        &tab,
        "SELECT TOP 5 * FROM sales.vMonthlyRevenue ORDER BY Month",
        "ERP",
        RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(typed.sets[0].rows.len(), 5);
    let names: Vec<&str> = typed.sets[0].columns.iter().map(|c| c.type_name.as_str()).collect();
    assert_eq!(names, ["nvarchar", "int", "decimal"]);
    tab.close().await;

    // Warehouses has no rowversion: the update is guarded by the original value, then restored.
    let mut request = BrowseRequest::new(table("ERP", "dbo", "Warehouses"), 1);
    request.filter = raw("[WarehouseId] = 1");
    let city = column(&browse(&handle, request.clone()).await, 0, "City").clone();
    for (from, to) in [
        (city.clone(), Value::Text("Soc Trang".into())),
        (Value::Text("Soc Trang".into()), city),
    ] {
        let set = RowChangeSet {
            object: table("ERP", "dbo", "Warehouses"),
            changes: vec![RowChange::Update {
                key: vec![Field::new("WarehouseId", Value::Int(1))],
                values: vec![Field::new("City", to)],
                original: vec![Field::new("City", from)],
                version: None,
                force: false,
            }],
        };
        assert_eq!(apply(&handle, set).await.expect("apply").updated, 1);
    }
    manager.disconnect("erp").await;
}

/// Batches return every result set and every row count, in order, whatever comes first.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mssql.yml"]
async fn contract_mssql_batches() {
    let (_manager, handle) = erp().await;
    let tab = session(&handle).await;
    let go = |text: &'static str| {
        let (handle, tab) = (handle.clone(), Arc::clone(&tab));
        async move { run(&handle, &tab, text, "ERP", RunOptions::default()).await }
    };
    let update_first = go("UPDATE sales.Invoices SET Status = Status WHERE InvoiceId = 1; SELECT TOP 2 InvoiceId FROM sales.Invoices")
        .await
        .expect("update then select");
    assert_eq!(update_first.sets.len(), 2, "{:?}", update_first.sets);
    assert!(update_first.sets[0].columns.is_empty());
    assert_eq!(update_first.sets[0].affected, Some(1));
    assert_eq!(
        update_first.sets[0].sql,
        "UPDATE sales.Invoices SET Status = Status WHERE InvoiceId = 1"
    );
    assert_eq!(update_first.sets[1].rows.len(), 2);
    assert_eq!(update_first.sets[1].sql, "SELECT TOP 2 InvoiceId FROM sales.Invoices");

    let select_first = go("SELECT TOP 2 InvoiceId FROM sales.Invoices; UPDATE sales.Invoices SET Status = Status WHERE InvoiceId = 1")
        .await
        .expect("select then update");
    assert_eq!(select_first.sets.len(), 2, "{:?}", select_first.sets);
    assert_eq!(select_first.sets[0].rows.len(), 2);
    assert_eq!(select_first.sets[1].affected, Some(1));

    // Variables keep their scope: the batch still runs as one batch.
    let scoped = go("DECLARE @id INT = 1; UPDATE sales.Invoices SET Status = Status WHERE InvoiceId = @id; SELECT @id AS id")
        .await
        .expect("declared variable");
    assert_eq!(scoped.sets.len(), 2, "{:?}", scoped.sets);
    assert_eq!(scoped.sets[1].rows[0][0], Value::Int(1));

    // A batch without results still reports one statement.
    let ddl = go("DECLARE @x INT = 1").await.expect("no results");
    assert_eq!(ddl.sets.len(), 1);

    let error = go("SELECT * FROM sales.NoSuchTable").await.expect_err("missing table");
    assert!(error.to_string().contains("SQL Server error 208"), "{error}");
    assert!(!error.to_string().contains("executing  on line"), "{error}");
    assert_eq!(error.field("vendorCode").and_then(|v| v.as_u64()), Some(208));
    // The failing statement is named from the error's line (the added row counts keep line numbers).
    let script = "UPDATE sales.Invoices SET Status = Status WHERE InvoiceId = 1;\nSELECT 1;\nSELECT * FROM sales.Missing";
    let failed = go(script).await.expect_err("third statement fails");
    assert_eq!(failed.field("statement").and_then(|s| s["index"].as_u64()), Some(2), "{failed:?}");
    assert_eq!(failed.field("position").and_then(|p| p["line"].as_u64()), Some(3), "{failed:?}");
    // The tab still works after errors.
    assert_eq!(go("SELECT 2 AS n").await.unwrap().sets[0].rows[0][0], Value::Int(2));
    tab.close().await;
}

/// SQL Server's self-signed certificate without "Trust server certificate" explains the fix.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mssql.yml"]
async fn contract_mssql_untrusted_certificate() {
    let error = ConnectionManager::probe(spec("ERP", false), secrets())
        .await
        .expect_err("certificate not trusted");
    assert!(error.to_string().contains("Trust server certificate"), "{error}");
    assert_eq!(error.code(), Some("CONNECTION_ERROR"));
}

/// Begin / Commit / Rollback and auto-commit report the server's transaction state and roll back.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mssql.yml"]
async fn contract_mssql_transactions() {
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
                "SELECT COUNT(*) FROM dbo.contract_children WHERE id > 900",
                "fosta_contract",
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
        "INSERT INTO dbo.contract_children VALUES (901, 1, 'tx')",
        "fosta_contract",
        options.clone(),
    )
    .await
    .unwrap();
    assert!(tab.transaction_state().await.open, "still open after the run");
    // A second Begin must be refused, not nest the open one.
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
        "INSERT INTO dbo.contract_children VALUES (902, 1, 'tx')",
        "fosta_contract",
        options.clone(),
    )
    .await
    .unwrap();
    assert!(tab.transaction_state().await.open, "implicit transaction");
    tab.transaction(TransactionOp::Commit, &options).await.unwrap();
    let back = tab.transaction(TransactionOp::AutoCommitOn, &options).await.unwrap();
    assert!(back.auto_commit && !back.open);
    assert_eq!(count(Arc::clone(&tab)).await, Value::Int(1));
    tab.close().await;
}

/// The statement timeout stops a long batch and the tab works again afterwards.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mssql.yml"]
async fn contract_mssql_statement_timeout() {
    let (_manager, handle) = erp().await;
    let tab = session(&handle).await;
    let slow = RunOptions {
        timeout: Some(Duration::from_secs(1)),
        ..RunOptions::default()
    };
    let started = std::time::Instant::now();
    let timed_out = run(&handle, &tab, "WAITFOR DELAY '00:00:20'", "ERP", slow)
        .await
        .expect_err("timed out");
    assert_eq!(timed_out.code(), Some("TIMEOUT"), "{timed_out:?}");
    assert!(started.elapsed() < Duration::from_secs(6), "{:?}", started.elapsed());
    let after = run(&handle, &tab, "SELECT DB_NAME() AS db", "ERP", RunOptions::default())
        .await
        .expect("usable after the timeout");
    assert_eq!(after.sets[0].rows[0][0], Value::Text("ERP".into()));
    tab.close().await;
}

/// The diagram's data: the expected foreign keys, the same tables and keys as the pre-contract driver's bulk path.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mssql.yml"]
async fn contract_mssql_schema_graph() {
    let (manager, handle) = erp().await;
    let graph = handle
        .call("read the schema", |conn| async move {
            conn.schema_graph().unwrap().schema_graph("ERP", &CallContext::default()).await
        })
        .await
        .unwrap();
    assert_eq!(graph.relations.len(), 3, "{:?}", graph.relations);
    assert!(graph.tables.iter().flat_map(|t| &t.columns).any(|c| c.primary));
    let legacy = manager.legacy_driver("erp").unwrap();
    let old = crate::schema::schema_graph(legacy.as_ref(), "ERP").await.unwrap();
    let names = |tables: &[crate::contract::SchemaTable]| tables.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&graph.tables), names(&old.tables));
    let links = |graph: &crate::contract::SchemaGraph| {
        let mut links: Vec<_> = graph
            .relations
            .iter()
            .map(|r| {
                (
                    r.table.clone(),
                    r.columns.clone(),
                    r.referenced_table.clone(),
                    r.referenced_columns.clone(),
                )
            })
            .collect();
        links.sort();
        links
    };
    assert_eq!(links(&graph), links(&old));
    for (new, legacy) in graph.tables.iter().zip(&old.tables) {
        let keys = |table: &crate::contract::SchemaTable| {
            table
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.primary, c.nullable))
                .collect::<Vec<_>>()
        };
        assert_eq!(keys(new), keys(legacy), "{}", new.name);
    }
    manager.disconnect("erp").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mssql.yml"]
async fn contract_mssql_typed_values() {
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
            ("meta", &ValueKind::Text),
            ("blob_data", &ValueKind::Bytes),
            ("note", &ValueKind::Text),
            ("rv", &ValueKind::Bytes),
        ]
    );
    let rv = page.columns.iter().find(|c| c.name == "rv").unwrap();
    assert!(rv.hints.read_only && rv.type_name == "rowversion");
    let row = &page.rows[0];
    assert_eq!(row[0], Value::Int(6));
    assert_eq!(row[1], Value::Text("item 0006".into()));
    assert_eq!(row[2], Value::Decimal("7.50".into()));
    assert_eq!(row[3], Value::Bool(true));
    assert_eq!(row[4].display().as_deref(), Some("2020-01-07"));
    assert!(matches!(row[5], Value::Timestamp(_)));
    assert_eq!(row[5].display().as_deref(), Some("2026-01-01 08:06:00.250"));
    assert_eq!(row[7], Value::Bytes(vec![0, 0, 0, 6]));
    assert_eq!(row[8], Value::Null);
    let versions = page.row_versions.clone().expect("row versions");
    assert_eq!(versions, vec![row[9].clone()]);

    // Asked without the rowversion column, the page still carries the versions.
    let mut narrow = request.clone();
    narrow.columns = Some(vec!["id".into(), "name".into()]);
    let narrow = browse(&handle, narrow).await;
    assert_eq!(narrow.columns.len(), 2);
    assert_eq!(narrow.row_versions, Some(versions.clone()));

    // The same row through a query tab reads the same.
    let tab = session(&handle).await;
    let sink = run(
        &handle,
        &tab,
        "SELECT * FROM dbo.contract_items WHERE id = 6",
        "fosta_contract",
        RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(&sink.sets[0].rows[0], row);
    tab.close().await;

    // Typed values round-trip through an edit guarded by the row version; text typed into typed columns is parsed.
    let set = RowChangeSet {
        object: f.table.clone(),
        changes: vec![RowChange::Update {
            key: vec![Field::new("id", Value::Int(6))],
            values: vec![
                Field::new("active", Value::Bool(false)),
                Field::new("born", Value::Text("2021-02-03".into())),
                Field::new("seen", row[5].clone()),
                Field::new("blob_data", Value::Text("0xcafe".into())),
                Field::new("price", Value::Decimal("-0.05".into())),
            ],
            original: Vec::new(),
            version: Some(versions[0].clone()),
            force: false,
        }],
    };
    assert_eq!(apply(&handle, set.clone()).await.expect("typed update").updated, 1);
    let after = browse(&handle, request).await;
    assert_eq!(after.rows[0][3], Value::Bool(false));
    assert_eq!(after.rows[0][4].display().as_deref(), Some("2021-02-03"));
    assert_eq!(after.rows[0][5], row[5]);
    assert_eq!(after.rows[0][2], Value::Decimal("-0.05".into()));
    assert_eq!(after.rows[0][7], Value::Bytes(vec![0xca, 0xfe]));
    assert_ne!(after.row_versions, Some(versions), "the update bumps the version");
    // The same change again carries the old version: a conflict.
    let error = apply(&handle, set).await.expect_err("stale version");
    assert_eq!(error.code(), Some(CONFLICT));
}

/// Tables without a rowversion column are guarded by the original values (NULL-safe, datetime rounding, xml as text).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mssql.yml"]
async fn contract_mssql_original_values_without_rowversion() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    run(
        &handle,
        &tab,
        "IF OBJECT_ID('dbo.contract_plain') IS NOT NULL DROP TABLE dbo.contract_plain; \
         CREATE TABLE dbo.contract_plain (id INT PRIMARY KEY, name NVARCHAR(50) NULL, seen DATETIME NULL, doc XML NULL, \
           ratio REAL NULL); \
         INSERT INTO dbo.contract_plain VALUES (1, NULL, '2026-01-01T08:00:00.003', N'<a n=\"1\"/>', 0.1), \
           (2, N'two', NULL, NULL, NULL)",
        "fosta_contract",
        RunOptions::default(),
    )
    .await
    .expect("create contract_plain");
    let plain = table("fosta_contract", "dbo", "contract_plain");
    let mut request = BrowseRequest::new(plain.clone(), 10);
    request.sort = vec![crate::contract::SortKey::asc("id")];
    let page = browse(&handle, request.clone()).await;
    assert!(page.row_versions.is_none());
    let original: Vec<Field> = page
        .columns
        .iter()
        .zip(&page.rows[0])
        .map(|(c, v)| Field::new(&c.name, v.clone()))
        .collect();
    let rename = |original: Vec<Field>, name: &str| RowChangeSet {
        object: plain.clone(),
        changes: vec![RowChange::Update {
            key: vec![Field::new("id", Value::Int(1))],
            values: vec![Field::new("name", Value::Text(name.into()))],
            original,
            version: None,
            force: false,
        }],
    };
    // Every column as read (NULL name, datetime, xml, real) matches the row.
    apply(&handle, rename(original.clone(), "one"))
        .await
        .expect("original values match");
    // The same originals are stale now (name is no longer NULL).
    let error = apply(&handle, rename(original, "uno")).await.expect_err("stale");
    assert_eq!(error.code(), Some(CONFLICT));
    let page = browse(&handle, request).await;
    assert_eq!(column(&page, 0, "name"), &Value::Text("one".into()));
    // A stale delete is refused; one with the current values goes through.
    let delete = |name: &str| RowChangeSet {
        object: plain.clone(),
        changes: vec![RowChange::Delete {
            key: vec![Field::new("id", Value::Int(2))],
            original: vec![Field::new("name", Value::Text(name.into())), Field::new("seen", Value::Null)],
            version: None,
            force: false,
        }],
    };
    assert_eq!(apply(&handle, delete("deux")).await.unwrap_err().code(), Some(CONFLICT));
    assert_eq!(apply(&handle, delete("two")).await.unwrap().deleted, 1);
    run(
        &handle,
        &tab,
        "DROP TABLE dbo.contract_plain",
        "fosta_contract",
        RunOptions::default(),
    )
    .await
    .unwrap();
    tab.close().await;
}
