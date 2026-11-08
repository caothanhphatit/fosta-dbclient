//! Contract suites for MongoDB against `dev/compose/mongo.yml`. The fixture writes the same typed documents as
//! `dev/seed/mongo/contract.js` (database fosta_contract; Decimal128 prices, dates, binary) through database commands
//! in Extended JSON before each suite. Plus MongoDB checks on the `events` demo database (`dev/seed/mongo.js`): catalog,
//! edits keeping BSON types, cursors read with getMore, connection strings without a password, the gateway, and the
//! compensation of a failed apply on a standalone server.
//! Override the server with FOSTA_CONTRACT_MONGO_HOST / _PORT / _USER / _PASSWORD.

use std::sync::Arc;

use base64::Engine as _;
use chrono::{Duration, NaiveDate};
use serde_json::json;

use crate::{
    contract::{
        errors::CONFLICT, testing::suites::*, BrowseRequest, CallContext, Capability, Category, CollectSink, ConnectSecrets,
        ConnectionSpec, Field, Filter, NodeKind, ObjectRef, Page, RawFilterLang, RowChange, RowChangeSet, RunOptions, RunRequest, Session,
        SessionOptions, TreeRequest, Value, ValueKind,
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
    std::env::var(format!("FOSTA_CONTRACT_MONGO_{name}")).unwrap_or_else(|_| default.to_owned())
}

/// One contract item as `dev/seed/mongo/contract.js` writes it, in Extended JSON.
fn item(i: i64) -> serde_json::Value {
    let born = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + Duration::days(i);
    let seen = NaiveDate::from_ymd_opt(2026, 1, 1)
        .unwrap()
        .and_hms_milli_opt(8, 0, 0, 250)
        .unwrap()
        + Duration::minutes(i);
    let blob = base64::engine::general_purpose::STANDARD.encode((i as u32).to_be_bytes());
    json!({
        "_id": i,
        "name": format!("item {i:04}"),
        "price": { "$numberDecimal": format!("{:.2}", i as f64 * 1.25) },
        "active": i % 2 == 0,
        "born": { "$date": format!("{}T00:00:00Z", born.format("%Y-%m-%d")) },
        "seen": { "$date": seen.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string() },
        "meta": { "n": i, "tags": ["a", "b"] },
        "blob_data": { "$binary": { "base64": blob, "subType": "00" } },
        "note": if i % 3 == 0 { serde_json::Value::Null } else { json!(format!("note {i}")) },
    })
}

/// Database commands that reset the collections.
fn seed() -> Vec<String> {
    let items: Vec<serde_json::Value> = (1..=1000).map(item).collect();
    let children: Vec<serde_json::Value> = (1..=200)
        .map(|i: i64| json!({ "_id": i, "item_id": i, "label": format!("child {i}") }))
        .collect();
    vec![
        json!({ "delete": "contract_items", "deletes": [{ "q": {}, "limit": 0 }] }).to_string(),
        json!({ "delete": "contract_children", "deletes": [{ "q": {}, "limit": 0 }] }).to_string(),
        json!({ "insert": "contract_items", "documents": items }).to_string(),
        json!({ "insert": "contract_children", "documents": children }).to_string(),
        json!({ "createIndexes": "contract_items", "indexes": [{ "key": { "name": 1 }, "name": "uq_contract_items_name", "unique": true }] })
            .to_string(),
    ]
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::MongoJson,
        text: json!({ "_id": key }).to_string(),
    }
}

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        engine: "mongodb".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "27017").parse().unwrap_or(27017),
        user: env("USER", "root"),
        database: Some("admin".into()),
        ..ConnectionSpec::default()
    }
}

fn secrets() -> ConnectSecrets {
    ConnectSecrets::password(&env("PASSWORD", "studio-root"))
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "mongodb",
        spec: spec(),
        secrets: secrets(),
        table: ObjectRef::new(Some("fosta_contract"), None, "contract_items", NodeKind::COLLECTION),
        child: ObjectRef::new(Some("fosta_contract"), None, "contract_children", NodeKind::COLLECTION),
        key_column: "_id",
        name_column: "name",
        seed: seed(),
        seed_database: Some("fosta_contract"),
        key_filter,
        sleep_statement: None,
        syntax_error: r#"{ "find": "#,
        constraint_violation: r#"{ "insert": "contract_items", "documents": [{ "_id": 1, "name": "dup" }] }"#,
        multi_statement: r#"{ "find": "contract_items", "limit": 3 }"#,
        insert_probe: r#"{ "insert": "contract_items", "documents": [{ "_id": 5000, "name": "probe" }] }"#,
        count_probe: r#"{ "count": "contract_items", "query": { "_id": 5000 } }"#,
        classify_samples: &[
            (r#"{ "find": "t" }"#, Category::Read),
            (r#"{ "insert": "t", "documents": [] }"#, Category::Write),
            (r#"{ "drop": "t" }"#, Category::Destructive),
            (r#"{ "createIndexes": "t", "indexes": [] }"#, Category::Ddl),
            (r#"{ "delete": "t", "deletes": [{ "q": {}, "limit": 0 }] }"#, Category::Destructive),
        ],
        language: "mongo-command",
        gaps: &[],
    }
}

crate::contract_tests!(contract_mongo, fixture());

// --- MongoDB checks (ported from live_tests.rs) ------------------------------------------------------------------------

async fn connect_as(id: &str, spec: ConnectionSpec, secrets: ConnectSecrets) -> (ConnectionManager, Handle) {
    let manager = ConnectionManager::default();
    manager.connect(id, spec, secrets).await.expect("connect");
    let handle = manager.handle(id).unwrap();
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

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str, database: &str) -> AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: Some(database.into()),
        offset: 0,
    };
    handle.run(Arc::clone(session), request, RunOptions::default(), &mut sink).await?;
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

fn collection(database: &str, name: &str) -> ObjectRef {
    ObjectRef::new(Some(database), None, name, NodeKind::COLLECTION)
}

fn raw(text: &str) -> Filter {
    Filter::Raw {
        language: RawFilterLang::MongoJson,
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

/// The `n` of a `count` command's reply.
fn count_of(sink: &CollectSink) -> i64 {
    let set = &sink.sets[0];
    let n = set.columns.iter().position(|c| c.name == "n").expect("n");
    match &set.rows[0][n] {
        Value::Int(n) => *n,
        other => panic!("count {other:?}"),
    }
}

fn update(key: i64, column: &str, value: Value, original: Value) -> RowChange {
    RowChange::Update {
        key: vec![Field::new("_id", Value::Int(key))],
        values: vec![Field::new(column, value)],
        original: vec![Field::new(column, original)],
        version: None,
        force: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_events_catalog_and_edits() {
    let (manager, handle) = connect_as("events", spec(), secrets()).await;
    let info = manager.connect_info("events").unwrap();
    assert!(info.server.label.starts_with("MongoDB "), "{}", info.server.label);
    println!("{} ({:?})", info.server.label, info.server.flavor);
    let top = handle
        .call("listed", |conn| async move {
            conn.children(&TreeRequest::default(), &CallContext::default()).await
        })
        .await
        .unwrap();
    assert!(top
        .nodes
        .iter()
        .any(|node| node.label == "admin" && node.detail.as_deref() == Some("system")));
    let events = top.nodes.iter().find(|node| node.label == "events").expect("events listed").clone();
    let request = TreeRequest {
        parent: Some(events.key),
        ..TreeRequest::default()
    };
    let collections = handle
        .call(
            "listed",
            |conn| async move { conn.children(&request, &CallContext::default()).await },
        )
        .await
        .unwrap();
    let clicks = collections.nodes.iter().find(|node| node.label == "clicks").expect("clicks");
    assert_eq!(clicks.key.id, "db:events/collection:clicks");
    assert_eq!(clicks.detail.as_deref(), Some("~20000 docs"));
    let described = handle
        .call("described", |conn| async move {
            conn.describe(&collection("events", "users"), &CallContext::default()).await
        })
        .await
        .unwrap();
    assert_eq!(described.column("signedUpAt").unwrap().kind, ValueKind::TimestampTz);
    assert_eq!(described.column("_id").unwrap().kind, ValueKind::Int);
    assert!(described.indexes.iter().any(|index| index.primary));
    assert!(described.ddl.as_deref().unwrap().contains("createIndexes"));

    let page = browse(&handle, BrowseRequest::new(collection("events", "clicks"), 5)).await;
    assert_eq!(page.rows.len(), 5);
    let mut all = BrowseRequest::new(collection("events", "clicks"), 5);
    all.exact_total = true;
    let total = browse(&handle, all.clone()).await.total.unwrap().value;
    all.filter = raw(r#"{ "type": "purchase" }"#);
    let filtered = browse(&handle, all).await.total.unwrap();
    assert!(filtered.exact && filtered.value < total, "{filtered:?} vs {total}");

    let tab = session(&handle).await;
    let grouped = run(
        &handle,
        &tab,
        r#"{ "aggregate": "clicks", "pipeline": [ { "$group": { "_id": "$device", "n": { "$sum": 1 } } } ], "cursor": {} }"#,
        "events",
    )
    .await
    .expect("aggregate");
    assert!(!grouped.sets[0].rows.is_empty());

    handle
        .call("created", |conn| async move {
            conn.admin()
                .unwrap()
                .create_database("studio_scratch", Some("first"), &CallContext::default())
                .await
        })
        .await
        .expect("create database");
    let listed = handle
        .call("listed", |conn| async move {
            conn.children(&TreeRequest::default(), &CallContext::default()).await
        })
        .await
        .unwrap();
    assert!(listed.nodes.iter().any(|node| node.label == "studio_scratch"));
    run(&handle, &tab, r#"{ "dropDatabase": 1 }"#, "studio_scratch")
        .await
        .expect("drop");

    // Flip a plan and restore it, each guarded by the value read before.
    for (from, to) in [("free", "pro"), ("pro", "free")] {
        let result = apply(
            &handle,
            RowChangeSet {
                object: collection("events", "users"),
                changes: vec![update(1, "plan", Value::Text(to.into()), Value::Text(from.into()))],
            },
        )
        .await
        .expect("apply");
        assert_eq!(result.updated, 1);
    }
    manager.disconnect("events").await;
}

/// Edits keep BSON types (dates, Decimal128, binary subtypes, nested numbers), and query-tab cursors are read past the
/// first batch with getMore.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_types_and_cursors() {
    let f = fixture();
    let (_manager, handle) = connect(&f).await;
    let tab = session(&handle).await;

    let mut request = BrowseRequest::new(collection("events", "users"), 1);
    request.filter = raw(r#"{ "_id": 1 }"#);
    let user = browse(&handle, request).await;
    let signed_up = column(&user, 0, "signedUpAt").clone();
    assert!(matches!(signed_up, Value::TimestampTz(_)), "{signed_up:?}");
    assert_eq!(user.columns.iter().find(|c| c.name == "signedUpAt").unwrap().type_name, "date");
    // Save the shown value back, as the grid does after an edit.
    let users = collection("events", "users");
    apply(
        &handle,
        RowChangeSet {
            object: users.clone(),
            changes: vec![update(1, "signedUpAt", signed_up.clone(), signed_up.clone())],
        },
    )
    .await
    .expect("apply");
    let still_date = run(
        &handle,
        &tab,
        r#"{ "count": "users", "query": { "_id": 1, "signedUpAt": { "$type": "date" } } }"#,
        "events",
    )
    .await
    .unwrap();
    assert_eq!(count_of(&still_date), 1, "signedUpAt is still a BSON date");
    let refused = apply(
        &handle,
        RowChangeSet {
            object: users,
            changes: vec![update(1, "signedUpAt", Value::Text("not a date".into()), signed_up)],
        },
    )
    .await
    .expect_err("text is not a date");
    assert!(refused.to_string().contains("holds a date"), "{refused}");

    let five_hundred = run(&handle, &tab, r#"{ "find": "clicks", "limit": 500 }"#, "events").await.unwrap();
    assert_eq!(five_hundred.sets[0].rows.len(), 500);
    assert!(!five_hundred.sets[0].truncated);
    let capped = run(&handle, &tab, r#"{ "find": "clicks", "limit": 5000 }"#, "events")
        .await
        .unwrap();
    assert_eq!(capped.sets[0].rows.len(), RunOptions::default().row_limit);
    assert!(capped.sets[0].truncated);
    assert!(capped.batches > 1, "rows are streamed in batches");

    // Typed values of the contract items, and type-preserving write-back.
    let items = f.table.clone();
    let mut one = BrowseRequest::new(items.clone(), 1);
    one.filter = key_filter(7);
    let page = browse(&handle, one.clone()).await;
    assert_eq!(column(&page, 0, "_id"), &Value::Int(7));
    assert_eq!(column(&page, 0, "price"), &Value::Decimal("8.75".into()));
    assert_eq!(column(&page, 0, "active"), &Value::Bool(false));
    assert_eq!(column(&page, 0, "born").display().unwrap(), "2020-01-08 00:00:00+00:00");
    assert_eq!(column(&page, 0, "seen").display().unwrap(), "2026-01-01 08:07:00.250+00:00");
    assert_eq!(column(&page, 0, "blob_data"), &Value::Bytes(vec![0, 0, 0, 7]));
    assert!(matches!(column(&page, 0, "meta"), Value::Document(fields) if fields[0] == ("n".to_owned(), Value::Int(7))));
    assert!(column(&page, 0, "note").is_null() || column(&page, 0, "note") == &Value::Text("note 7".into()));
    let kinds: Vec<(&str, ValueKind)> = page.columns.iter().map(|c| (c.name.as_str(), c.kind.clone())).collect();
    for (name, kind) in [
        ("price", ValueKind::Decimal),
        ("born", ValueKind::TimestampTz),
        ("blob_data", ValueKind::Bytes),
        ("meta", ValueKind::Document),
    ] {
        assert!(kinds.contains(&(name, kind.clone())), "{name}: {kinds:?}");
    }
    let changes = vec![RowChange::Update {
        key: vec![Field::new("_id", Value::Int(7))],
        values: vec![
            Field::new("price", Value::Decimal("10.50".into())),
            Field::new("born", Value::Text("2021-02-03".into())),
            Field::new("blob_data", Value::Bytes(vec![1, 2])),
            Field::new("meta", Value::Json(r#"{"n": 70, "tags": ["c"]}"#.into())),
            Field::new("meta.n", Value::Int(71)),
        ],
        original: vec![Field::new("price", Value::Decimal("8.75".into()))],
        version: None,
        force: false,
    }];
    // Two values for overlapping paths are refused by the server; the edit without the nested path applies.
    assert!(apply(
        &handle,
        RowChangeSet {
            object: items.clone(),
            changes: changes.clone()
        }
    )
    .await
    .is_err());
    let RowChange::Update {
        key, mut values, original, ..
    } = changes.into_iter().next().unwrap()
    else {
        unreachable!()
    };
    values.pop();
    apply(
        &handle,
        RowChangeSet {
            object: items.clone(),
            changes: vec![RowChange::Update {
                key,
                values,
                original,
                version: None,
                force: false,
            }],
        },
    )
    .await
    .expect("typed update");
    let typed = run(
        &handle,
        &tab,
        r#"{ "count": "contract_items", "query": { "_id": 7, "price": { "$type": "decimal" }, "born": { "$type": "date" }, "blob_data": { "$type": "binData" }, "meta.n": { "$type": "int" }, "meta.tags": "c" } }"#,
        "fosta_contract",
    )
    .await
    .unwrap();
    assert_eq!(count_of(&typed), 1, "types kept on write-back");
    let nested = apply(
        &handle,
        RowChangeSet {
            object: items.clone(),
            changes: vec![update(7, "meta.n", Value::Int(72), Value::Int(70))],
        },
    )
    .await
    .expect("dotted path update");
    assert_eq!(nested.updated, 1);
    let page = browse(&handle, one).await;
    assert!(matches!(column(&page, 0, "meta"), Value::Document(fields) if fields[0] == ("n".to_owned(), Value::Int(72))));
    // Keyset paging on ObjectIds.
    let mut paged = BrowseRequest::new(collection("events", "clicks"), 50);
    let first = browse(&handle, paged.clone()).await;
    assert!(matches!(column(&first, 0, "_id"), Value::ObjectId(_) | Value::Int(_)));
    paged.after = first.next_keyset.clone();
    let second = browse(&handle, paged).await;
    assert_ne!(column(&first, 49, "_id"), column(&second, 0, "_id"));
}

/// A saved connection string has no password; the adapter adds the stored one.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_uri_without_password() {
    let mut with_url = spec();
    with_url.url = Some(format!(
        "mongodb://{}@{}:{}/?authSource=admin",
        with_url.user, with_url.host, with_url.port
    ));
    let (manager, handle) = connect_as("uri", with_url, secrets()).await;
    let top = handle
        .call("listed", |conn| async move {
            conn.children(&TreeRequest::default(), &CallContext::default()).await
        })
        .await
        .unwrap();
    assert!(top.nodes.iter().any(|node| node.label == "events"));
    manager.disconnect("uri").await;
}

/// Read-only refuses write commands, filters must be query documents, sessions have no transactions.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_gateway() {
    let (manager, handle) = connect_as("gate-mongo", spec(), secrets()).await;
    assert!(!handle.conn.capabilities().contains(Capability::Transactions));
    manager
        .open_session("gate-mongo", "m".into(), SessionOptions::default())
        .await
        .expect("session");
    let read_only = Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: "gate-mongo".into(),
        connection_name: "gate-mongo".into(),
        safety: SafetySettings::preset(Preset::ReadOnly),
    };
    let request = |text: &str| RunRequest {
        text: text.into(),
        database: Some("events".into()),
        offset: 0,
    };
    let mut sink = CollectSink::default();
    let insert = request(r#"{ "insert": "clicks", "documents": [ { "type": "probe" } ] }"#);
    let blocked = gateway::run(&manager, &read_only, "m", &insert, None, &mut sink)
        .await
        .expect_err("blocked");
    assert_eq!(blocked.code(), Some(gateway::BLOCKED));
    let out = request(r#"{ "aggregate": "clicks", "pipeline": [ { "$out": "copy" } ], "cursor": {} }"#);
    assert!(gateway::run(&manager, &read_only, "m", &out, None, &mut sink).await.is_err());
    let summary = gateway::run(&manager, &read_only, "m", &request(r#"{ "count": "clicks" }"#), None, &mut sink)
        .await
        .expect("count runs");
    assert!(!summary.transaction.supported);
    let mut browse_request = BrowseRequest::new(collection("events", "clicks"), 5);
    browse_request.filter = raw(r#"{ "$where": "sleep(100) || true" }"#);
    let error = gateway::browse(&manager, &read_only, "gate-mongo", &browse_request)
        .await
        .expect_err("$where rejected");
    assert_eq!(error.code(), Some(gateway::filter::INVALID_FILTER));
    manager.close_session("m").await;
    manager.disconnect("gate-mongo").await;
}

/// On a standalone server a failed apply undoes the writes it already made: here the update of row 20 is restored
/// after the insert of an existing key fails.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_failed_apply_is_compensated() {
    let f = fixture();
    let (manager, handle) = connect(&f).await;
    let standalone = manager.connect_info(CONNECTION).unwrap().server.flavor.is_none();
    let error = apply(
        &handle,
        RowChangeSet {
            object: f.table.clone(),
            changes: vec![
                update(20, "name", Value::Text("renamed".into()), Value::Text("item 0020".into())),
                RowChange::Insert {
                    values: vec![Field::new("_id", Value::Int(21)), Field::new("name", Value::Text("dup".into()))],
                },
                RowChange::Delete {
                    key: vec![Field::new("_id", Value::Int(22))],
                    original: Vec::new(),
                    version: None,
                    force: false,
                },
            ],
        },
    )
    .await
    .expect_err("duplicate key");
    assert_eq!(error.code(), Some("ENGINE_ERROR"), "{error:?}");
    assert_eq!(error.field("vendorCode"), Some(&json!(11000)));
    if standalone {
        assert_eq!(error.field("compensated"), Some(&json!(true)), "{error:?}");
    }
    let mut row = BrowseRequest::new(f.table.clone(), 5);
    row.filter = key_filter(20);
    assert_eq!(column(&browse(&handle, row).await, 0, "name"), &Value::Text("item 0020".into()));
    let mut kept = BrowseRequest::new(f.table.clone(), 5);
    kept.filter = key_filter(22);
    assert_eq!(browse(&handle, kept).await.rows.len(), 1, "the delete after the failure never ran");
    // A conflict found before writing leaves everything untouched and lists the current document.
    let stale = apply(
        &handle,
        RowChangeSet {
            object: f.table.clone(),
            changes: vec![
                RowChange::Insert {
                    values: vec![Field::new("_id", Value::Int(3001)), Field::new("name", Value::Text("never".into()))],
                },
                update(23, "price", Value::Decimal("1".into()), Value::Decimal("999".into())),
            ],
        },
    )
    .await
    .expect_err("stale price");
    assert_eq!(stale.code(), Some(CONFLICT));
    let conflicts = stale.field("conflicts").and_then(|c| c.as_array()).unwrap();
    assert_eq!(conflicts[0]["index"], 1);
    assert!(conflicts[0]["current"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["column"] == "price" && f["value"]["v"] == "28.75"));
    let mut never = BrowseRequest::new(f.table.clone(), 5);
    never.filter = key_filter(3001);
    assert!(browse(&handle, never).await.rows.is_empty());
}
