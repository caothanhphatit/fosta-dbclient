//! Contract suites for PostgreSQL against `dev/compose/postgres.yml` (database `shop`; the suite data is
//! `dev/seed/postgres/contract.sql`, schema `shop`), plus PostgreSQL-specific checks on the demo schemas: typed values,
//! row versions, the tree, relations, EXPLAIN and the read-only backstop. Override the server with
//! FOSTA_CONTRACT_POSTGRES_HOST / _PORT / _USER / _PASSWORD / _DATABASE.

use std::sync::Arc;

use crate::{
    contract::{
        testing::suites::*, BrowseRequest, CallContext, Category, CollectSink, ConnectSecrets, ConnectionSpec, Filter, NodeKind, ObjectRef,
        RawFilterLang, RunOptions, RunRequest, Session, SessionOptions, TransactionOp, TreeRequest, Value, ValueKind,
    },
    runtime::{ConnectionManager, Handle},
};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_POSTGRES_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn seed() -> Vec<String> {
    include_str!("../../../../dev/seed/postgres/contract.sql")
        .replace("\r\n", "\n")
        .split("\nGO\n")
        .map(|chunk| chunk.trim().to_owned())
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::Sql,
        text: format!("\"id\" = {key}"),
    }
}

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        engine: "postgres".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "5432").parse().unwrap_or(5432),
        user: env("USER", "postgres"),
        database: Some(env("DATABASE", "shop")),
        ..ConnectionSpec::default()
    }
}

fn secrets() -> ConnectSecrets {
    ConnectSecrets::password(&env("PASSWORD", "studio-root"))
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "postgres",
        spec: spec(),
        secrets: secrets(),
        table: ObjectRef::new(Some("shop"), Some("shop"), "contract_items", NodeKind::TABLE),
        child: ObjectRef::new(Some("shop"), Some("shop"), "contract_children", NodeKind::TABLE),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("SELECT pg_sleep(30)"),
        syntax_error: "SELEC * FORM shop.contract_items",
        constraint_violation: "INSERT INTO shop.contract_items (id, name, price, active) VALUES (1, 'dup', 1, true)",
        multi_statement: "SELECT 1; SELECT id, name FROM shop.contract_items ORDER BY id LIMIT 3",
        insert_probe: "INSERT INTO shop.contract_items (id, name, price, active) VALUES (5000, 'probe', 1, true)",
        count_probe: "SELECT COUNT(*) FROM shop.contract_items WHERE id = 5000",
        classify_samples: &[
            ("SELECT * FROM t", Category::Read),
            ("UPDATE t SET a = 1 WHERE id = 2", Category::Write),
            ("DELETE FROM t", Category::Destructive),
            ("CREATE TABLE x (id int)", Category::Ddl),
            ("GRANT ALL ON t TO x", Category::Admin),
            ("EXPLAIN ANALYZE DELETE FROM t WHERE id = 1", Category::Write),
            ("REFRESH MATERIALIZED VIEW mv", Category::Ddl),
        ],
        language: "sql",
        gaps: &[],
    }
}

crate::contract_tests!(contract_postgres, fixture());

// --- PostgreSQL-specific checks -----------------------------------------------------------------------------------------

async fn connected() -> (ConnectionManager, Handle) {
    let f = fixture();
    connect(&f).await
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

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str, options: RunOptions) -> crate::error::AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: None,
        offset: 0,
    };
    handle.run(Arc::clone(session), request, options, &mut sink).await?;
    Ok(sink)
}

async fn children(handle: &Handle, parent: Option<crate::contract::NodeKey>) -> Vec<crate::contract::ObjectNode> {
    let request = TreeRequest {
        parent,
        ..TreeRequest::default()
    };
    handle
        .call(
            "listed",
            |conn| async move { conn.children(&request, &CallContext::default()).await },
        )
        .await
        .expect("children")
        .nodes
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/postgres.yml"]
async fn contract_postgres_typed_values_and_row_versions() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let mut request = BrowseRequest::new(f.table.clone(), 1);
    request.filter = key_filter(6);
    let page = handle
        .call("browsed", |conn| async move {
            conn.tabular().unwrap().browse(&request, &CallContext::default()).await
        })
        .await
        .expect("browse");
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
            ("tags", &ValueKind::Array(Box::new(ValueKind::Text))),
            ("uid", &ValueKind::Uuid),
            ("at", &ValueKind::TimestampTz),
        ]
    );
    let row = &page.rows[0];
    assert_eq!(row[0], Value::Int(6));
    assert_eq!(row[2], Value::Decimal("7.50".into()));
    assert_eq!(row[3], Value::Bool(true));
    assert_eq!(row[4].display().as_deref(), Some("2020-01-07"));
    assert!(matches!(row[5], Value::Timestamp(_)));
    assert!(matches!(&row[6], Value::Json(text) if text.contains("\"n\"")));
    assert_eq!(row[7], Value::Bytes(vec![0, 0, 0, 6]));
    assert_eq!(row[8], Value::Null);
    assert_eq!(row[9], Value::Array(vec![Value::Text("a".into()), Value::Text("b6".into())]));
    assert!(matches!(&row[10], Value::Uuid(text) if text.len() == 36));
    assert!(matches!(row[11], Value::TimestampTz(_)));
    let versions = page.row_versions.expect("a table page carries xmin per row");
    assert_eq!(versions.len(), 1);
    assert!(
        matches!(&versions[0], Value::Text(text) if text.parse::<u64>().is_ok()),
        "{:?}",
        versions[0]
    );

    // The same row through a query tab: typed the same way (binary protocol).
    let tab = session(&handle).await;
    let sink = run(
        &handle,
        &tab,
        "SELECT * FROM shop.contract_items WHERE id = 6",
        RunOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(sink.sets[0].rows[0][2], Value::Decimal("7.50".into()));
    assert_eq!(sink.sets[0].rows[0][9], row[9]);
    assert_eq!(sink.sets[0].rows[0][10], row[10]);
    tab.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/postgres.yml"]
async fn contract_postgres_tree_has_schemas_and_object_kinds() {
    let (_manager, handle) = connected().await;
    let databases = children(&handle, None).await;
    let shop = databases.iter().find(|node| node.label == "shop").expect("shop listed").clone();
    assert_eq!(shop.key.id, "database:shop");
    let schemas = children(&handle, Some(shop.key.clone())).await;
    let labels: Vec<&str> = schemas.iter().map(|node| node.label.as_str()).collect();
    assert!(labels.contains(&"public") && labels.contains(&"sales"), "{labels:?}");
    assert!(labels.contains(&"Extensions"), "{labels:?}");
    let public = schemas.iter().find(|node| node.label == "public").unwrap();
    let folders = children(&handle, Some(public.key.clone())).await;
    let folder_labels: Vec<&str> = folders.iter().map(|node| node.label.as_str()).collect();
    for wanted in ["Tables", "Functions", "Types"] {
        assert!(folder_labels.contains(&wanted), "{folder_labels:?}");
    }
    let types = folders.iter().find(|node| node.label == "Types").unwrap();
    let types = children(&handle, Some(types.key.clone())).await;
    let status = types.iter().find(|node| node.label == "order_status").expect("enum listed");
    assert_eq!(status.icon, "engine:postgres/enum");
    assert_eq!(status.default_view.as_deref(), Some("definition"));
    let description = handle
        .call("described", |conn| {
            let object = status.object.clone().unwrap();
            async move { conn.describe(&object, &CallContext::default()).await }
        })
        .await
        .unwrap();
    assert!(description
        .extra
        .iter()
        .any(|(key, value)| key == "labels" && value.contains("shipped")));
    let tables = folders.iter().find(|node| node.label == "Tables").unwrap();
    let tables = children(&handle, Some(tables.key.clone())).await;
    let orders = tables.iter().find(|node| node.label == "orders").expect("orders listed");
    assert_eq!(orders.object.as_ref().unwrap().schema.as_deref(), Some("public"));
    assert!(orders.actions.iter().any(|action| action.id == "explain"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/postgres.yml"]
async fn contract_postgres_schema_graph_explain_and_read_only() {
    let (_manager, handle) = connected().await;
    let graph = handle
        .call("graph", |conn| async move {
            conn.schema_graph().unwrap().schema_graph("shop", &CallContext::default()).await
        })
        .await
        .unwrap();
    assert!(graph.tables.iter().any(|table| table.name == "orders"));
    assert!(graph.tables.iter().any(|table| table.name.starts_with("sales.")));
    assert!(graph
        .relations
        .iter()
        .any(|relation| relation.table == "order_items" && relation.referenced_table == "orders"));

    let plan = handle
        .call("explained", |conn| async move {
            conn.invoke(
                "postgres.explain",
                serde_json::json!({ "sql": "SELECT * FROM public.orders WHERE id = 1" }),
                &CallContext::default(),
            )
            .await
        })
        .await
        .expect("explain");
    assert!(plan["plan"][0]["Plan"]["Node Type"].is_string(), "{plan}");
    let stacked = handle
        .call("explained", |conn| async move {
            conn.invoke(
                "postgres.explain",
                serde_json::json!({ "sql": "SELECT 1; DROP TABLE public.orders" }),
                &CallContext::default(),
            )
            .await
        })
        .await;
    assert!(stacked.is_err(), "a second statement must not run");

    // The read-only backstop: writes fail with SQLSTATE 25006, reads still work, and read-write comes back.
    let tab = session(&handle).await;
    let read_only = RunOptions {
        read_only: true,
        ..RunOptions::default()
    };
    let refused = run(&handle, &tab, "CREATE TABLE public.never_created (id int)", read_only.clone())
        .await
        .expect_err("read-only session");
    assert_eq!(refused.field("sqlstate").and_then(|v| v.as_str()), Some("25006"), "{refused:?}");
    run(&handle, &tab, "SELECT 1", read_only).await.expect("reads work");
    run(&handle, &tab, "SELECT 1", RunOptions::default())
        .await
        .expect("read-write again");
    tab.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/postgres.yml"]
async fn contract_postgres_creates_databases() {
    let (_manager, handle) = connected().await;
    handle
        .call("created", |conn| async move {
            conn.admin()
                .unwrap()
                .create_database("studio_scratch", None, &CallContext::default())
                .await
        })
        .await
        .expect("create database");
    let listed = children(&handle, None).await;
    assert!(listed.iter().any(|node| node.label == "studio_scratch"));
    let tab = session(&handle).await;
    run(&handle, &tab, "DROP DATABASE studio_scratch", RunOptions::default())
        .await
        .expect("drop");
    tab.close().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/postgres.yml"]
async fn contract_postgres_error_positions_and_auto_commit() {
    let (_manager, handle) = connected().await;
    let tab = session(&handle).await;
    // A plain SELECT runs through a cursor; its error position still counts from the start of the statement.
    let error = run(
        &handle,
        &tab,
        "SELECT 1;\nSELECT nope FROM shop.contract_items",
        RunOptions::default(),
    )
    .await
    .expect_err("unknown column");
    let payload = error.payload().unwrap();
    assert_eq!(payload["sqlstate"], "42703", "{payload}");
    assert_eq!(
        (payload["position"]["line"].as_u64(), payload["position"]["column"].as_u64()),
        (Some(2), Some(8)),
        "{payload}"
    );
    assert_eq!(payload["statement"]["index"], 1);
    // A statement that is not a cursor query reports the same way.
    let error = run(&handle, &tab, "UPDATE shop.contract_items SET nope = 1", RunOptions::default())
        .await
        .expect_err("unknown column");
    assert_eq!(error.payload().unwrap()["position"]["column"].as_u64(), Some(32));

    // Auto-commit off keeps a transaction open until it is committed; on ends it.
    let options = RunOptions::default();
    let off = tab
        .transaction(TransactionOp::AutoCommitOff, &options)
        .await
        .expect("auto-commit off");
    assert!(off.open && !off.auto_commit);
    let on = tab
        .transaction(TransactionOp::AutoCommitOn, &options)
        .await
        .expect("auto-commit on");
    assert!(!on.open && on.auto_commit);
    tab.close().await;
}
