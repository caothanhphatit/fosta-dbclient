//! Contract suites for SQLite against a file in the temp folder (data from `dev/seed/sqlite/contract.sql`; no container
//! is needed), plus SQLite-specific checks: typed values, attached databases, the read-only switch and EXPLAIN.
//! Override the file with FOSTA_CONTRACT_SQLITE_FILE.

use std::sync::Arc;

use crate::{
    contract::{
        errors::UNSUPPORTED_CAPABILITY, testing::suites::*, BrowseRequest, CallContext, Capability, Category, CollectSink, ConnectSecrets,
        ConnectionSpec, Field, Filter, NodeKind, ObjectRef, RawFilterLang, RowChange, RowChangeSet, RunOptions, RunRequest, Session,
        SessionOptions, TransactionOp, TreeRequest, Value, ValueKind,
    },
    runtime::{ConnectionManager, Handle},
};

fn path() -> String {
    std::env::var("FOSTA_CONTRACT_SQLITE_FILE")
        .unwrap_or_else(|_| std::env::temp_dir().join("fosta-contract-sqlite.db").to_string_lossy().into_owned())
}

fn seed() -> Vec<String> {
    include_str!("../../../../dev/seed/sqlite/contract.sql")
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

fn spec(options: &[(&str, &str)]) -> ConnectionSpec {
    let mut spec = ConnectionSpec {
        engine: "sqlite".into(),
        file: Some(path()),
        ..ConnectionSpec::default()
    };
    spec.options.insert("create".into(), "true".into());
    for (key, value) in options {
        spec.options.insert((*key).to_owned(), (*value).to_owned());
    }
    spec
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "sqlite",
        spec: spec(&[]),
        secrets: ConnectSecrets::default(),
        table: ObjectRef::new(Some("main"), None, "contract_items", NodeKind::TABLE),
        child: ObjectRef::new(Some("main"), None, "contract_children", NodeKind::TABLE),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        // SQLite has no sleep: a recursive count that runs for minutes until interrupted.
        sleep_statement: Some(
            "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 4000000000) SELECT count(*) FROM c",
        ),
        syntax_error: "SELEC * FORM contract_items",
        constraint_violation: "INSERT INTO contract_items (id, name, price, active) VALUES (1, 'dup', 1, 1)",
        multi_statement: "SELECT 1; SELECT id, name FROM contract_items ORDER BY id LIMIT 3",
        insert_probe: "INSERT INTO contract_items (id, name, price, active) VALUES (5000, 'probe', 1, 1)",
        count_probe: "SELECT COUNT(*) FROM contract_items WHERE id = 5000",
        classify_samples: &[
            ("SELECT * FROM t", Category::Read),
            ("UPDATE t SET a = 1 WHERE id = 2", Category::Write),
            ("DELETE FROM t", Category::Destructive),
            ("CREATE TABLE x (id INTEGER)", Category::Ddl),
            ("ATTACH DATABASE 'x.db' AS x", Category::Admin),
            ("PRAGMA table_info(t)", Category::Read),
        ],
        language: "sql",
        gaps: &[],
    }
}

crate::contract_tests!(contract_sqlite, fixture());

// --- SQLite-specific checks ---------------------------------------------------------------------------------------------

async fn session(handle: &Handle) -> Arc<dyn Session> {
    handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("session")
        .session
}

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str) -> crate::error::AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: None,
        offset: 0,
    };
    handle.run(Arc::clone(session), request, RunOptions::default(), &mut sink).await?;
    Ok(sink)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes the contract database file; run with --ignored --test-threads=1"]
async fn contract_sqlite_typed_values() {
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
        ]
    );
    let row = &page.rows[0];
    assert_eq!(row[0], Value::Int(6));
    assert_eq!(row[3], Value::Bool(true));
    assert_eq!(row[4].display().as_deref(), Some("2020-01-07"));
    assert!(matches!(row[5], Value::Timestamp(_)), "{:?}", row[5]);
    assert!(matches!(&row[6], Value::Json(text) if text.contains("\"n\"")));
    assert_eq!(row[7], Value::Bytes(vec![0, 0, 0, 6]));
    assert_eq!(row[8], Value::Null);
    assert!(
        page.row_versions.is_none(),
        "SQLite has no row versions: edits compare the original values"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes the contract database file; run with --ignored --test-threads=1"]
async fn contract_sqlite_attached_databases_read_only_and_explain() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    // A second file attached through the option shows up next to `main` and answers queries.
    let other = std::env::temp_dir().join("fosta-contract-sqlite-attached.db");
    let _ = std::fs::remove_file(&other);
    let manager = ConnectionManager::default();
    let attach = format!("audit={}", other.to_string_lossy());
    manager
        .connect("attached", spec(&[("attach", attach.as_str())]), ConnectSecrets::default())
        .await
        .expect("connect with an attached database");
    let attached = manager.handle("attached").unwrap();
    let tab = session(&attached).await;
    run(
        &attached,
        &tab,
        "CREATE TABLE audit.log (id INTEGER PRIMARY KEY, what TEXT); INSERT INTO audit.log (what) VALUES ('x')",
    )
    .await
    .expect("write into the attached database");
    let top = attached
        .call("listed", |conn| async move {
            conn.children(&TreeRequest::default(), &CallContext::default()).await
        })
        .await
        .unwrap();
    let labels: Vec<&str> = top.nodes.iter().map(|node| node.label.as_str()).collect();
    assert_eq!(labels, ["main", "audit"]);
    let page = attached
        .call("browsed", |conn| async move {
            conn.tabular()
                .unwrap()
                .browse(
                    &BrowseRequest::new(ObjectRef::new(Some("audit"), None, "log", NodeKind::TABLE), 10),
                    &CallContext::default(),
                )
                .await
        })
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    tab.close().await;
    manager.disconnect("attached").await;
    let _ = std::fs::remove_file(&other);

    // Opened read-only: no editing capability, edits are refused, writes in a query tab fail.
    let manager = ConnectionManager::default();
    manager
        .connect("ro", spec(&[("readOnly", "true")]), ConnectSecrets::default())
        .await
        .expect("connect read-only");
    let ro = manager.handle("ro").unwrap();
    assert!(!ro.conn.capabilities().contains(Capability::EditRows));
    let refused = ro
        .call("applied", |conn| async move {
            conn.tabular()
                .unwrap()
                .apply(
                    &RowChangeSet {
                        object: ObjectRef::new(Some("main"), None, "contract_items", NodeKind::TABLE),
                        changes: vec![RowChange::Delete {
                            key: vec![Field::new("id", Value::Int(1))],
                            original: Vec::new(),
                            version: None,
                            force: false,
                        }],
                    },
                    &CallContext::default(),
                )
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code(), Some(UNSUPPORTED_CAPABILITY));
    let tab = session(&ro).await;
    assert!(run(&ro, &tab, "DELETE FROM contract_items WHERE id = 1").await.is_err());
    tab.close().await;
    manager.disconnect("ro").await;

    // EXPLAIN QUERY PLAN through invoke.
    let plan = handle
        .call("explained", |conn| async move {
            conn.invoke(
                "sqlite.explain",
                serde_json::json!({ "sql": "SELECT * FROM contract_items WHERE id = 3" }),
                &CallContext::default(),
            )
            .await
        })
        .await
        .expect("explain");
    assert!(plan["plan"][0]["detail"].as_str().unwrap().contains("SEARCH"), "{plan}");

    // The diagram and the relations.
    let graph = handle
        .call("graph", |conn| async move {
            conn.schema_graph().unwrap().schema_graph("main", &CallContext::default()).await
        })
        .await
        .unwrap();
    assert!(graph
        .relations
        .iter()
        .any(|relation| relation.table == "contract_children" && relation.referenced_table == "contract_items"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes the contract database file; run with --ignored --test-threads=1"]
async fn contract_sqlite_error_positions_and_auto_commit() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    let error = run(&handle, &tab, "SELECT 1;\nSELECT nope FROM contract_items")
        .await
        .expect_err("unknown column");
    let payload = error.payload().unwrap();
    assert_eq!(payload["statement"]["index"], 1, "{payload}");
    assert_eq!(payload["position"]["line"].as_u64(), Some(2), "{payload}");
    assert!(payload["vendorCode"].is_string() || payload["vendorCode"].is_number(), "{payload}");

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
