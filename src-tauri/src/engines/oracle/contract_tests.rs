//! Contract suites for Oracle against `dev/compose/oracle.yml` (schema `SHOP`, service `FREEPDB1`; the suite data is
//! `dev/seed/oracle/contract.sql`), plus Oracle-specific checks: typed values, ROWID edits, error positions, the schema
//! switch, PL/SQL source and the ER diagram.
//!
//! They need Oracle Instant Client: on PATH / ORACLE_HOME, downloaded by `cargo test live_oracle_instant_client_download --
//! --ignored`, or in the folder named by FOSTA_CONTRACT_ORACLE_CLIENT_DIR (passed as the profile's Instant Client folder).
//! Override the server with FOSTA_CONTRACT_ORACLE_{HOST,PORT,USER,PASSWORD,SERVICE}.

use std::sync::Arc;

use super::ROWID;
use crate::{
    contract::{
        errors::CONFLICT, testing::suites::*, BrowseRequest, CallContext, Category, CollectSink, ConnectSecrets, ConnectionSpec, Field,
        Filter, NodeKind, ObjectRef, RawFilterLang, RowChange, RowChangeSet, RunOptions, RunRequest, Session, SessionOptions,
        TransactionOp, TreeRequest, Value, ValueKind,
    },
    runtime::Handle,
};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_ORACLE_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn seed() -> Vec<String> {
    include_str!("../../../../dev/seed/oracle/contract.sql")
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
    let mut spec = ConnectionSpec {
        engine: "oracle".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "1521").parse().unwrap_or(1521),
        user: env("USER", "shop"),
        database: Some(env("SERVICE", "FREEPDB1")),
        ..ConnectionSpec::default()
    };
    if let Ok(dir) = std::env::var("FOSTA_CONTRACT_ORACLE_CLIENT_DIR") {
        spec.options.insert(super::connect::CLIENT_DIR.into(), dir);
    }
    spec
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "oracle",
        spec: spec(),
        secrets: ConnectSecrets::password(&env("PASSWORD", "studio-root")),
        table: ObjectRef::new(Some("SHOP"), None, "CONTRACT_ITEMS", NodeKind::TABLE),
        child: ObjectRef::new(Some("SHOP"), None, "CONTRACT_CHILDREN", NodeKind::TABLE),
        key_column: "id",
        name_column: "name",
        seed: seed(),
        seed_database: None,
        key_filter,
        // A Cartesian product of two row generators: runs for minutes with little memory until OCIBreak stops it.
        sleep_statement: Some(
            "SELECT COUNT(*) FROM (SELECT LEVEL l FROM dual CONNECT BY LEVEL <= 100000) a, \
             (SELECT LEVEL l FROM dual CONNECT BY LEVEL <= 100000) b",
        ),
        syntax_error: "SELEC * FORM contract_items",
        constraint_violation: "INSERT INTO contract_items (\"id\", \"name\", \"price\", \"active\") VALUES (1, 'dup', 1, TRUE)",
        multi_statement: "SELECT 1 FROM dual;\nSELECT \"id\", \"name\" FROM contract_items ORDER BY \"id\" FETCH FIRST 3 ROWS ONLY",
        insert_probe: "INSERT INTO contract_items (\"id\", \"name\", \"price\", \"active\") VALUES (5000, 'probe', 1, TRUE)",
        count_probe: "SELECT COUNT(*) FROM contract_items WHERE \"id\" = 5000",
        classify_samples: &[
            ("SELECT * FROM t", Category::Read),
            ("UPDATE t SET a = 1 WHERE id = 2", Category::Write),
            ("DELETE FROM t", Category::Destructive),
            ("CREATE TABLE x (id NUMBER)", Category::Ddl),
            ("CREATE OR REPLACE PROCEDURE p IS BEGIN NULL; END;", Category::Ddl),
            ("BEGIN p(1); END;", Category::Write),
            ("DECLARE n NUMBER; BEGIN n := 1; END;", Category::Write),
            ("BEGIN EXECUTE IMMEDIATE 'DROP TABLE t'; END;", Category::Destructive),
            ("DROP TABLE t PURGE", Category::Destructive),
            ("TRUNCATE TABLE t", Category::Destructive),
            ("ALTER SYSTEM KILL SESSION '12,345'", Category::Admin),
            ("GRANT SELECT ON t TO bob", Category::Admin),
            ("ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD'", Category::Read),
            ("COMMIT", Category::Transaction),
        ],
        language: "sql",
        gaps: &[],
    }
}

crate::contract_tests!(contract_oracle, fixture());

// --- Oracle-specific checks ---------------------------------------------------------------------------------------------

async fn session(handle: &Handle, database: Option<&str>) -> Arc<dyn Session> {
    let options = SessionOptions {
        database: database.map(str::to_owned),
        ..SessionOptions::default()
    };
    handle
        .call("opened a session", |conn| async move {
            conn.open_session(&options, &CallContext::default()).await
        })
        .await
        .expect("session")
        .session
}

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str, database: Option<&str>) -> crate::error::AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: database.map(str::to_owned),
        offset: 0,
    };
    handle.run(Arc::clone(session), request, RunOptions::default(), &mut sink).await?;
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

async fn apply(handle: &Handle, set: RowChangeSet) -> crate::error::AppResult<crate::contract::ApplyResult> {
    handle
        .call("applied", |conn| async move {
            conn.tabular().unwrap().apply(&set, &CallContext::default()).await
        })
        .await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_oracle_typed_values() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let mut request = BrowseRequest::new(f.table.clone(), 1);
    request.filter = key_filter(6);
    let page = browse(&handle, request).await;
    let kinds: Vec<(&str, &ValueKind)> = page.columns.iter().map(|c| (c.name.as_str(), &c.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("id", &ValueKind::Int),
            ("name", &ValueKind::Text),
            ("price", &ValueKind::Decimal),
            ("active", &ValueKind::Bool),
            ("born", &ValueKind::Timestamp),
            ("seen", &ValueKind::Timestamp),
            ("meta", &ValueKind::Json),
            ("blob_data", &ValueKind::Bytes),
            ("doc", &ValueKind::Text),
            ("note", &ValueKind::Text),
        ]
    );
    let row = &page.rows[0];
    assert_eq!(row[0], Value::Int(6));
    assert_eq!(row[2], Value::Decimal("7.5".into()));
    assert_eq!(row[3], Value::Bool(true));
    assert_eq!(row[4].display().as_deref(), Some("2020-01-07 00:00:00"));
    assert!(matches!(&row[6], Value::Json(text) if text.contains("\"n\"")), "{:?}", row[6]);
    assert_eq!(row[7], Value::Bytes(vec![0, 0, 0, 6]));
    assert_eq!(row[8], Value::Text("doc 6".into()));
    assert_eq!(row[9], Value::Null);
    assert!(page.row_versions.is_none(), "edits compare the original values");

    // A typed update round trip, checked by every comparable original value (the CLOB, JSON and BLOB are left out).
    let original: Vec<Field> = page.columns.iter().zip(row).map(|(c, v)| Field::new(&c.name, v.clone())).collect();
    let set = RowChangeSet {
        object: f.table.clone(),
        changes: vec![RowChange::Update {
            key: vec![Field::new("id", Value::Int(6))],
            values: vec![
                Field::new("active", Value::Bool(false)),
                Field::new("price", Value::Decimal("8.25".into())),
                Field::new("meta", Value::Json("{\"n\":60}".into())),
            ],
            original,
            version: None,
            force: false,
        }],
    };
    assert_eq!(apply(&handle, set.clone()).await.expect("typed update").updated, 1);
    let error = apply(&handle, set).await.expect_err("the originals are stale now");
    assert_eq!(error.code(), Some(CONFLICT));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_oracle_rowid_edits() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let loose = ObjectRef::new(Some("SHOP"), None, "CONTRACT_LOOSE", NodeKind::TABLE);
    let mut request = BrowseRequest::new(loose.clone(), 10);
    request.sort = vec![crate::contract::SortKey::asc("qty")];
    let page = browse(&handle, request.clone()).await;
    assert_eq!(page.key_columns, [ROWID], "a table without a primary key is keyed by ROWID");
    assert_eq!(page.columns[0].name, ROWID);
    assert!(page.columns[0].hints.read_only);
    let rowid = page.rows[1][0].clone();
    let update = |name: &str, original: &str| RowChange::Update {
        key: vec![Field::new(ROWID, rowid.clone())],
        values: vec![Field::new("name", Value::Text(name.into()))],
        original: vec![
            Field::new("name", Value::Text(original.into())),
            Field::new("doc", Value::Text("not compared".into())),
        ],
        version: None,
        force: false,
    };
    let set = |change| RowChangeSet {
        object: loose.clone(),
        changes: vec![change],
    };
    assert_eq!(apply(&handle, set(update("renamed", "loose 2"))).await.unwrap().updated, 1);
    let error = apply(&handle, set(update("again", "loose 2"))).await.expect_err("stale");
    assert_eq!(error.code(), Some(CONFLICT));
    let conflict = &error.field("conflicts").unwrap()[0];
    assert_eq!(conflict["current"][1]["value"]["v"], "renamed", "{conflict}");
    let page = browse(&handle, request).await;
    assert_eq!(page.rows[1][1], Value::Text("renamed".into()));
    // Views have no key: read-only.
    let view = RowChangeSet {
        object: ObjectRef::new(Some("SHOP"), None, "ORDER_TOTALS", NodeKind::VIEW),
        changes: vec![RowChange::Delete {
            key: vec![Field::new("ORDER_ID", Value::Int(1))],
            original: Vec::new(),
            version: None,
            force: false,
        }],
    };
    assert!(apply(&handle, view).await.unwrap_err().to_string().contains("read-only"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_oracle_errors_sessions_and_schemas() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    // The service name is not a schema: the session stays in the login schema.
    let s = session(&handle, Some("FREEPDB1")).await;
    let error = run(&handle, &s, "SELECT 1 FROM dual;\nSELECT * FORM contract_items", None)
        .await
        .expect_err("syntax error");
    assert_eq!(error.code(), Some("ENGINE_ERROR"));
    assert_eq!(error.field("vendorCode").and_then(|v| v.as_str()), Some("ORA-00923"), "{error:?}");
    let position = error.field("position").expect("position");
    assert_eq!(position["line"], 2, "{error:?}");
    assert_eq!(position["column"], 10, "{error:?}");
    assert_eq!(error.field("statement").unwrap()["index"], 1);

    // Each statement commits until a transaction is begun; Begin keeps the changes open until Rollback.
    run(&handle, &s, "UPDATE contract_items SET \"note\" = 'auto' WHERE \"id\" = 1", None)
        .await
        .unwrap();
    let options = RunOptions::default();
    assert!(!s.transaction(TransactionOp::Status, &options).await.unwrap().open);
    let other = session(&handle, Some("SHOP")).await;
    let sink = run(&handle, &other, "SELECT \"note\" FROM contract_items WHERE \"id\" = 1", None)
        .await
        .unwrap();
    assert_eq!(sink.sets[0].rows[0][0], Value::Text("auto".into()), "committed for other sessions");
    assert!(s.transaction(TransactionOp::Begin, &options).await.unwrap().open);
    run(&handle, &s, "UPDATE contract_items SET \"note\" = 'held' WHERE \"id\" = 1", None)
        .await
        .unwrap();
    let sink = run(&handle, &other, "SELECT \"note\" FROM contract_items WHERE \"id\" = 1", None)
        .await
        .unwrap();
    assert_eq!(sink.sets[0].rows[0][0], Value::Text("auto".into()), "not committed yet");
    assert!(!s.transaction(TransactionOp::Rollback, &options).await.unwrap().open);

    // Native JSON columns arrive as JSON text (the query is rewritten through JSON_SERIALIZE).
    let sink = run(&handle, &s, "SELECT * FROM contract_items WHERE \"id\" = 6", None)
        .await
        .unwrap();
    let meta = sink.sets[0].columns.iter().position(|c| c.name == "meta").unwrap();
    assert_eq!(sink.sets[0].columns[meta].kind, ValueKind::Json);
    assert!(matches!(&sink.sets[0].rows[0][meta], Value::Json(text) if text.contains("\"tags\"")));

    // Another schema as the tab's database: unqualified names resolve there.
    let sys = session(&handle, None).await;
    let sink = run(
        &handle,
        &sys,
        "SELECT SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') FROM dual",
        Some("SHOP"),
    )
    .await
    .unwrap();
    assert_eq!(sink.sets[0].rows[0][0], Value::Text("SHOP".into()));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the engine's container (dev/compose)"]
async fn contract_oracle_plsql_tree_definitions_and_diagram() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let children = |parent| {
        let handle = handle.clone();
        async move {
            let request = TreeRequest {
                parent,
                ..TreeRequest::default()
            };
            handle
                .call("listed children", |conn| async move {
                    conn.children(&request, &CallContext::default()).await
                })
                .await
                .expect("children")
                .nodes
        }
    };
    let top = children(None).await;
    assert_eq!(top[0].label, "SHOP", "the login schema comes first");
    assert_eq!(top[0].kind, NodeKind::DATABASE);
    let folders = children(Some(top[0].key.clone())).await;
    let labels: Vec<&str> = folders.iter().map(|n| n.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Tables",
            "Views",
            "Materialized views",
            "Sequences",
            "Synonyms",
            "Packages",
            "Procedures",
            "Functions"
        ]
    );
    let packages = children(Some(folders[5].key.clone())).await;
    let package = packages.iter().find(|n| n.label == "SHOP_PKG").expect("SHOP_PKG");
    assert_eq!(package.default_view.as_deref(), Some(super::nodes::DEFINITION_VIEW));
    let object = package.object.clone().unwrap();
    assert_eq!(object.database.as_deref(), Some("SHOP"));
    let described = handle
        .call(
            "described",
            |conn| async move { conn.describe(&object, &CallContext::default()).await },
        )
        .await
        .expect("describe package");
    let ddl = described.ddl.expect("package source");
    assert!(ddl.contains("PACKAGE BODY"), "{ddl}");
    assert!(
        described.extra.iter().any(|(k, v)| k == "status" && v == "VALID"),
        "{:?}",
        described.extra
    );
    let tables = children(Some(folders[0].key.clone())).await;
    assert!(tables
        .iter()
        .any(|n| n.label == "CONTRACT_ITEMS" && n.default_view.as_deref() == Some("data")));

    let graph = handle
        .call("graph", |conn| async move {
            conn.schema_graph().unwrap().schema_graph("SHOP", &CallContext::default()).await
        })
        .await
        .expect("schema graph");
    assert!(graph.tables.iter().any(|t| t.name == "CONTRACT_ITEMS"));
    assert!(
        graph
            .relations
            .iter()
            .any(|r| r.table == "CONTRACT_CHILDREN" && r.referenced_table == "CONTRACT_ITEMS" && r.columns == ["item_id"]),
        "{:?}",
        graph.relations
    );
}
