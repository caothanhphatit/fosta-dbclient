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

/// The diagram's data for `events` (ported from live_tests.rs): collections with their sampled fields, `_id` as the key,
/// and no relations (MongoDB has no foreign keys).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_schema_graph() {
    let (manager, handle) = connect_as("graph", spec(), secrets()).await;
    assert!(handle.conn.capabilities().contains(Capability::SchemaGraph));
    let graph = handle
        .call("read the schema", |conn| async move {
            conn.schema_graph().unwrap().schema_graph("events", &CallContext::default()).await
        })
        .await
        .unwrap();
    assert!(!graph.tables.is_empty());
    assert!(graph.relations.is_empty(), "{:?}", graph.relations);
    assert!(graph.tables.iter().flat_map(|t| &t.columns).any(|c| c.primary && c.name == "_id"));
    manager.disconnect("graph").await;
}

/// The engine operations the MongoDB views invoke, through the gateway in a Read-only workspace: find with keyset pages
/// by `_id`, computed projections, collations and hints; exact, estimated and limited counts (a view counted, having no
/// metadata count); capped distinct values;
/// references resolved by `_id`; server-side JavaScript, credential collections and unknown names refused.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_engine_operations() {
    let f = fixture();
    let (manager, handle) = connect(&f).await;
    // A view of the active items (set up outside the Read-only gateway): views have no metadata count.
    let tab = session(&handle).await;
    run(&handle, &tab, r#"{ "drop": "contract_active_items" }"#, "fosta_contract")
        .await
        .unwrap();
    let view = r#"{ "create": "contract_active_items", "viewOn": "contract_items", "pipeline": [{ "$match": { "active": true } }] }"#;
    run(&handle, &tab, view, "fosta_contract").await.unwrap();
    let context = Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: CONNECTION.into(),
        connection_name: CONNECTION.into(),
        safety: SafetySettings::preset(Preset::ReadOnly),
    };
    let invoke = |name: &'static str, extra: serde_json::Value| {
        let mut args = json!({ "database": "fosta_contract", "collection": "contract_items" });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (manager, context) = (&manager, &context);
        async move { gateway::invoke(manager, context, gateway::Caller::EngineView, name, args, None).await }
    };
    let ids = |page: &serde_json::Value| -> Vec<i64> {
        page["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|doc| doc["_id"].as_i64().unwrap())
            .collect()
    };

    // Pages follow `_id` without a sort; `nextAfter` reads the next one.
    let first = invoke("mongo.find", json!({ "filter": { "active": true }, "limit": 10 }))
        .await
        .unwrap();
    assert_eq!(ids(&first), (1..=10).map(|i| i * 2).collect::<Vec<_>>());
    assert_eq!((first["hasMore"].as_bool(), first["keyset"].as_bool()), (Some(true), Some(true)));
    assert_eq!(first["nextAfter"], json!({ "$numberInt": "20" }));
    assert_eq!(first["documents"][0]["price"], json!({ "$numberDecimal": "2.50" }), "{first}");
    assert!(first["statement"]
        .as_str()
        .unwrap()
        .starts_with("db.getCollection(\"contract_items\").find("));
    let second = invoke(
        "mongo.find",
        json!({ "filter": "{ \"active\": true }", "limit": 10, "after": first["nextAfter"].clone() }),
    )
    .await
    .unwrap();
    assert_eq!(ids(&second)[0], 22);
    let descending = invoke("mongo.find", json!({ "sort": { "_id": -1 }, "limit": 2, "after": 500 }))
        .await
        .unwrap();
    assert_eq!(ids(&descending), [499, 498]);
    // Other sorts page by skip only.
    let by_name = invoke("mongo.find", json!({ "sort": { "name": -1 }, "skip": 1, "limit": 3 }))
        .await
        .unwrap();
    assert_eq!(ids(&by_name), [999, 998, 997]);
    assert_eq!((by_name["keyset"].as_bool(), by_name["nextAfter"].is_null()), (Some(false), true));
    assert!(invoke("mongo.find", json!({ "sort": { "name": 1 }, "after": 3 })).await.is_err());
    // Limits are capped.
    let capped = invoke("mongo.find", json!({ "limit": 100_000 })).await.unwrap();
    assert_eq!(capped["documents"].as_array().unwrap().len(), 1000);
    // Computed projection, collation, hint.
    let computed = invoke(
        "mongo.find",
        json!({ "filter": { "_id": 5 }, "projection": { "name": 1, "twice": { "$multiply": ["$meta.n", 2] } } }),
    )
    .await
    .unwrap();
    assert_eq!(computed["documents"][0], json!({ "_id": 5, "name": "item 0005", "twice": 10 }));
    let collated = invoke(
        "mongo.find",
        json!({ "filter": { "name": "ITEM 0005" }, "collation": { "locale": "en", "strength": 2 } }),
    )
    .await
    .unwrap();
    assert_eq!(ids(&collated), [5]);
    let hinted = invoke(
        "mongo.find",
        json!({ "filter": { "name": "item 0007" }, "hint": "uq_contract_items_name", "maxTimeMS": 5000 }),
    )
    .await
    .unwrap();
    assert_eq!(ids(&hinted), [7]);
    assert!(invoke("mongo.find", json!({ "hint": "no_such_index" })).await.is_err());

    // Counts.
    let count = invoke("mongo.count", json!({ "filter": { "active": true } })).await.unwrap();
    assert_eq!((count["count"].as_u64(), count["estimated"].as_bool()), (Some(500), Some(false)));
    let estimated = invoke("mongo.count", json!({ "estimated": true })).await.unwrap();
    assert_eq!(
        (estimated["count"].as_u64(), estimated["estimated"].as_bool()),
        (Some(1000), Some(true))
    );
    let limited = invoke("mongo.count", json!({ "filter": {}, "limit": 10 })).await.unwrap();
    assert_eq!(limited["count"].as_u64(), Some(10));
    let on_view = invoke("mongo.count", json!({ "collection": "contract_active_items", "estimated": true }))
        .await
        .unwrap();
    // Servers that answer the metadata count on a view run the view (an exact count); others refuse it and it is counted.
    assert_eq!(on_view["count"].as_u64(), Some(500), "{on_view}");

    // Distinct values: array elements, capped.
    let tags = invoke("mongo.distinct", json!({ "field": "meta.tags" })).await.unwrap();
    assert_eq!(tags["values"], json!(["a", "b"]));
    assert_eq!(tags["truncated"], json!(false));
    let some = invoke(
        "mongo.distinct",
        json!({ "field": "_id", "filter": { "_id": { "$lte": 10 } }, "limit": 5 }),
    )
    .await
    .unwrap();
    assert_eq!(some["values"], json!([1, 2, 3, 4, 5]));
    assert_eq!(some["truncated"], json!(true));

    // References by `_id`.
    let found = invoke("mongo.resolveRef", json!({ "id": { "$numberInt": "42" } })).await.unwrap();
    assert_eq!(found["document"]["name"], json!("item 0042"));
    let missing = invoke("mongo.resolveRef", json!({ "id": 99_999 })).await.unwrap();
    assert!(missing["document"].is_null());

    // Refused whatever the rules: server-side JavaScript and unknown names; credentials are administration.
    for (name, extra) in [
        ("mongo.find", json!({ "filter": { "$where": "sleep(100) || true" } })),
        (
            "mongo.count",
            json!({ "filter": "{ \"$expr\": { \"$function\": { \"body\": \"f\", \"args\": [], \"lang\": \"js\" } } }" }),
        ),
        ("mongo.nope", json!({})),
    ] {
        let error = invoke(name, extra).await.expect_err(name);
        assert_eq!(error.code(), Some(gateway::BLOCKED), "{name}: {error}");
    }
    let secrets = gateway::invoke(
        &manager,
        &context,
        gateway::Caller::EngineView,
        "mongo.find",
        json!({ "database": "admin", "collection": "system.users" }),
        None,
    )
    .await
    .expect_err("credentials");
    assert_eq!(secrets.code(), Some(gateway::BLOCKED), "{secrets}");
    manager.disconnect(CONNECTION).await;
}

/// Aggregations: the per-stage preview on a bounded sample (counts, documents, the failing stage, `$lookup`, random
/// samples seen alike by every stage), refusals, and the full run reading or writing with `$merge` / `$out`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_aggregations() {
    let f = fixture();
    let (manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    run(&handle, &tab, r#"{ "drop": "contract_agg_out" }"#, "fosta_contract").await.ok();
    let context = |preset: Preset| Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: CONNECTION.into(),
        connection_name: CONNECTION.into(),
        safety: SafetySettings::preset(preset),
    };
    let invoke = |name: &'static str, extra: serde_json::Value, preset: Preset, confirmation: Option<gateway::Confirmation>| {
        let mut args = json!({ "database": "fosta_contract", "collection": "contract_items" });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (manager, context) = (&manager, context(preset));
        async move { gateway::invoke(manager, &context, gateway::Caller::EngineView, name, args, confirmation.as_ref()).await }
    };
    let counts = |preview: &serde_json::Value| -> Vec<serde_json::Value> {
        preview["stages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|stage| stage["count"].clone())
            .collect()
    };

    // Per-stage counts and documents on the first 100 documents; stages given as values or text; `$lookup` joins.
    let pipeline = json!([
        { "$match": { "active": true } },
        { "$lookup": { "from": "contract_children", "localField": "_id", "foreignField": "item_id", "as": "kids" } },
        "{ \"$unwind\": \"$kids\" }",
        { "$sort": { "_id": -1 } },
        { "$project": { "label": "$kids.label" } },
        { "$group": { "_id": null, "n": { "$sum": 1 } } },
    ]);
    let preview = invoke(
        "mongo.aggregatePreview",
        json!({ "pipeline": pipeline, "sampleSize": 100, "limit": 3 }),
        Preset::ReadOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        preview["sample"],
        json!({ "mode": "first", "size": 100, "count": 100 }),
        "{preview}"
    );
    assert_eq!(counts(&preview), [json!(50), json!(50), json!(50), json!(50), json!(50), json!(1)]);
    let stages = preview["stages"].as_array().unwrap();
    assert_eq!(stages[0]["operator"], "$match");
    assert_eq!(stages[0]["documents"].as_array().unwrap().len(), 3);
    assert_eq!(stages[1]["documents"][0]["kids"][0]["label"], "child 2", "{preview}");
    assert_eq!(stages[3]["documents"][0]["_id"], json!(100));
    assert_eq!(stages[4]["documents"][0], json!({ "_id": 100, "label": "child 100" }));
    assert_eq!(stages[5]["documents"][0]["n"], json!(50));
    assert!(stages.iter().all(|stage| stage["error"].is_null() && stage["elapsedMs"].is_u64()));
    // Up to a stage only.
    let partial = invoke(
        "mongo.aggregatePreview",
        json!({ "pipeline": pipeline, "sampleSize": 100, "upTo": 1 }),
        Preset::ReadOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(counts(&partial), [json!(50), json!(50)]);

    // The sample is bounded: never more than the hard maximum, here the whole collection (1000) at most.
    let bounded = invoke(
        "mongo.aggregatePreview",
        json!({ "pipeline": [{ "$match": {} }], "sampleSize": 1_000_000 }),
        Preset::ReadOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(bounded["sample"]["size"], json!(10_000));
    assert_eq!(counts(&bounded), [json!(1000)]);
    assert_eq!(bounded["stages"][0]["documents"].as_array().unwrap().len(), 20);
    let small = invoke(
        "mongo.aggregatePreview",
        json!({ "pipeline": [{ "$match": {} }, { "$count": "c" }], "sampleSize": 10 }),
        Preset::ReadOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(counts(&small), [json!(10), json!(1)]);
    assert_eq!(small["stages"][1]["documents"][0]["c"], json!(10));
    // A random sample: every stage sees the same documents.
    let random = invoke(
        "mongo.aggregatePreview",
        json!({
            "pipeline": [{ "$match": {} }, { "$group": { "_id": null, "ids": { "$sum": "$_id" } } }],
            "sampleSize": 20,
            "sampleMode": "random",
            "limit": 100,
        }),
        Preset::ReadOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(random["sample"]["mode"], "random");
    assert_eq!(counts(&random)[0], json!(20), "{random}");
    let sum: i64 = random["stages"][0]["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|doc| doc["_id"].as_i64().unwrap())
        .sum();
    assert_eq!(random["stages"][1]["documents"][0]["ids"].as_i64(), Some(sum), "{random}");

    // The first failing stage carries the error and stops the preview.
    let failing = invoke(
        "mongo.aggregatePreview",
        json!({ "pipeline": [{ "$match": {} }, { "$project": { "x": { "$divide": [1, 0] } } }, { "$limit": 1 }], "sampleSize": 5 }),
        Preset::ReadOnly,
        None,
    )
    .await
    .unwrap();
    let stages = failing["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2, "{failing}");
    assert_eq!(stages[0]["count"], json!(5));
    assert!(stages[0]["error"].is_null());
    assert!(stages[1]["count"].is_null());
    assert!(stages[1]["error"].as_str().unwrap().contains("divide"), "{failing}");

    // Refused whatever the rules: writes in a preview, server-side JavaScript.
    for (name, pipeline) in [
        ("mongo.aggregatePreview", json!([{ "$out": "contract_agg_out" }])),
        ("mongo.aggregatePreview", json!([{ "$merge": { "into": "contract_agg_out" } }])),
        ("mongo.aggregate", json!([{ "$match": { "$where": "true" } }])),
        (
            "mongo.aggregatePreview",
            json!(["{ \"$group\": { \"_id\": null, \"x\": { \"$accumulator\": {} } } }"]),
        ),
    ] {
        let error = invoke(name, json!({ "pipeline": pipeline }), Preset::Open, None)
            .await
            .expect_err(name);
        assert_eq!(error.code(), Some(gateway::BLOCKED), "{name} {pipeline}: {error}");
    }
    // Credentials read through `$lookup` / `$unionWith` are administration: blocked in a Read-only workspace.
    for pipeline in [
        json!([{ "$lookup": { "from": "system.users", "localField": "a", "foreignField": "user", "as": "u" } }]),
        json!([{ "$unionWith": "system.users" }]),
    ] {
        let error = invoke("mongo.aggregatePreview", json!({ "pipeline": pipeline }), Preset::ReadOnly, None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some(gateway::BLOCKED), "{pipeline}: {error}");
    }

    // The full run reads, capped, in a Read-only workspace.
    let read = invoke(
        "mongo.aggregate",
        json!({ "pipeline": [{ "$match": { "active": true } }, { "$sort": { "_id": 1 } }], "limit": 5, "allowDiskUse": true }),
        Preset::ReadOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(read["documents"].as_array().unwrap().len(), 5);
    assert_eq!(read["documents"][0]["_id"], json!(2));
    assert_eq!((read["hasMore"].as_bool(), read["wrote"].is_null()), (Some(true), true));
    assert!(read["statement"]
        .as_str()
        .unwrap()
        .starts_with("db.getCollection(\"contract_items\").aggregate(["));

    // $merge writes: blocked in Read-only, confirmed in Guarded; $out (destructive) replaces the collection.
    let merge = json!({ "pipeline": [{ "$match": { "_id": { "$lte": 10 } } }, { "$merge": { "into": "contract_agg_out" } }] });
    assert_eq!(
        invoke("mongo.aggregate", merge.clone(), Preset::ReadOnly, None)
            .await
            .unwrap_err()
            .code(),
        Some(gateway::BLOCKED)
    );
    let confirmed = |extra: serde_json::Value| {
        let invoke = &invoke;
        async move {
            let ask = invoke("mongo.aggregate", extra.clone(), Preset::Guarded, None).await.unwrap_err();
            assert_eq!(ask.code(), Some(gateway::CONFIRMATION_REQUIRED), "{ask}");
            let payload = ask.payload().unwrap();
            let token = payload["token"].as_str().unwrap().to_owned();
            let typed = payload["expected"].as_str().map(str::to_owned);
            invoke(
                "mongo.aggregate",
                extra,
                Preset::Guarded,
                Some(gateway::Confirmation { token, typed }),
            )
            .await
            .unwrap()
        }
    };
    let merged = confirmed(merge).await;
    assert_eq!(merged["wrote"], json!({ "stage": "$merge", "target": "contract_agg_out" }));
    assert_eq!(merged["documents"], json!([]));
    let total = || {
        let invoke = &invoke;
        async move {
            invoke("mongo.count", json!({ "collection": "contract_agg_out" }), Preset::ReadOnly, None)
                .await
                .unwrap()["count"]
                .as_u64()
        }
    };
    assert_eq!(total().await, Some(10));
    let out_args = json!({ "pipeline": [{ "$match": { "_id": { "$lte": 3 } } }, { "$out": "contract_agg_out" }] });
    // $out drops and replaces its target: destructive, so Guarded asks for the typed confirmation.
    let ask = invoke("mongo.aggregate", out_args.clone(), Preset::Guarded, None)
        .await
        .unwrap_err();
    assert!(ask.payload().unwrap()["expected"].as_str().is_some(), "{ask}");
    let out = confirmed(out_args).await;
    assert_eq!(out["wrote"]["stage"], "$out");
    assert_eq!(total().await, Some(3));
    // A change stream never ends: refused before it reaches the server, for the run and for explain.
    for (name, extra) in [
        ("mongo.aggregate", json!({ "pipeline": [{ "$changeStream": {} }] })),
        (
            "mongo.explain",
            json!({ "kind": "aggregate", "pipeline": [{ "$changeStream": {} }] }),
        ),
    ] {
        let refused = invoke(name, extra, Preset::Open, None).await.unwrap_err();
        assert!(refused.to_string().contains("change stream"), "{name}: {refused}");
    }
    // Names are used as given: a padded collection name is refused instead of trimmed.
    let padded = invoke(
        "mongo.aggregate",
        json!({ "collection": " contract_items", "pipeline": [] }),
        Preset::Open,
        None,
    )
    .await
    .unwrap_err();
    assert!(padded.to_string().contains("leading or trailing spaces"), "{padded}");
    run(&handle, &tab, r#"{ "drop": "contract_agg_out" }"#, "fosta_contract").await.ok();
    manager.disconnect(CONNECTION).await;
}
/// The collection workspace's edits: nested edits by dotted path (the rest of the top-level field untouched, stored
/// number types kept), conflicts on a changed path only, Read-only blocking, and the bulk operations with their
/// confirmations.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_document_edits_and_bulk() {
    use crate::engines::mongo::values::{exact, unset};
    let f = fixture();
    let (manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    let docs = json!([
        { "_id": 1, "address": { "city": "Oslo", "zip": "0150" }, "price": 10.5, "n": { "$numberLong": "5" }, "tags": ["a", "b"], "status": "new" },
        { "_id": 2, "address": { "city": "Rome", "zip": "00100" }, "price": 20.0, "n": { "$numberLong": "6" }, "tags": [], "status": "new" },
        { "_id": 3, "address": { "city": "Kyiv" }, "price": 30.0, "status": "paid", "qty": 2 },
    ]);
    run(&handle, &tab, r#"{ "drop": "contract_m5" }"#, "fosta_contract").await.ok();
    run(
        &handle,
        &tab,
        &json!({ "insert": "contract_m5", "documents": docs }).to_string(),
        "fosta_contract",
    )
    .await
    .unwrap();
    let context = |preset: Preset| Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: CONNECTION.into(),
        connection_name: CONNECTION.into(),
        safety: SafetySettings::preset(preset),
    };
    let object = collection("fosta_contract", "contract_m5");
    let read = |id: i64| {
        let (manager, context) = (&manager, context(Preset::Open));
        async move {
            let args = json!({ "database": "fosta_contract", "collection": "contract_m5", "id": id });
            gateway::invoke(manager, &context, gateway::Caller::EngineView, "mongo.resolveRef", args, None)
                .await
                .unwrap()["document"]
                .clone()
        }
    };
    let edit = |values: Vec<Field>, original: Vec<Field>| RowChangeSet {
        object: object.clone(),
        changes: vec![RowChange::Update {
            key: vec![Field::new("_id", exact(r#"{"$numberInt":"1"}"#))],
            values,
            original,
            version: None,
            force: false,
        }],
    };
    let save = |set: RowChangeSet, preset: Preset| {
        let (manager, context) = (&manager, context(preset));
        async move { gateway::apply_edits(manager, &context, gateway::Caller::GridSave, &set, None, None).await }
    };

    // A nested edit and a removed nested field; the rest of `address` and the stored types stay.
    let first = edit(
        vec![
            Field::new("address.city", exact(r#""Bergen""#)),
            Field::new("address.zip", unset()),
            Field::new("added.at", exact(r#"{"$date":"2026-01-02T00:00:00Z"}"#)),
            Field::new("n", exact("7")),
        ],
        vec![
            Field::new("address.city", exact(r#""Oslo""#)),
            Field::new("address.zip", exact(r#""0150""#)),
            Field::new("added.at", exact("null")),
            Field::new("n", exact("5")),
        ],
    );
    assert_eq!(
        save(first.clone(), Preset::ReadOnly).await.unwrap_err().code(),
        Some(gateway::BLOCKED)
    );
    let result = save(first, Preset::Open).await.unwrap();
    assert_eq!(result.updated, 1);
    let now = read(1).await;
    assert_eq!(now["address"], json!({ "city": "Bergen" }), "{now}");
    assert_eq!(now["price"], json!(10.5));
    assert_eq!(now["n"], json!(7), "an exact plain 7 is an int32 now: {now}");
    assert_eq!(now["added"]["at"], json!({ "$date": "2026-01-02T00:00:00Z" }));

    // A sub-document edited as a whole is written at its path.
    let geo = edit(
        vec![Field::new("address", exact(r#"{ "city": "Bergen", "zip": "5003" }"#))],
        vec![Field::new("address", exact(r#"{ "city": "Bergen" }"#))],
    );
    save(geo, Preset::Open).await.unwrap();
    assert_eq!(read(1).await["address"], json!({ "city": "Bergen", "zip": "5003" }));

    // Someone else changes the edited path: a conflict with the path as it is now; nothing written.
    run(
        &handle,
        &tab,
        r#"{ "update": "contract_m5", "updates": [{ "q": { "_id": 1 }, "u": { "$set": { "address.city": "Tromso", "status": "elsewhere" } } }] }"#,
        "fosta_contract",
    )
    .await
    .unwrap();
    let stale = edit(
        vec![Field::new("address.city", exact(r#""Mine""#))],
        vec![Field::new("address.city", exact(r#""Bergen""#))],
    );
    let error = save(stale, Preset::Open).await.unwrap_err();
    assert_eq!(error.code(), Some(CONFLICT), "{error}");
    let payload = error.payload().unwrap();
    let current = payload["conflicts"][0]["current"].as_array().unwrap();
    assert!(
        current
            .iter()
            .any(|field| field["column"] == "address.city" && field["value"]["v"] == "\"Tromso\""),
        "{payload}"
    );
    assert_eq!(read(1).await["address"]["city"], json!("Tromso"));
    // Another field changed (status), the edited path did not: no conflict.
    let fresh = edit(
        vec![Field::new("address.zip", exact(r#""9000""#))],
        vec![Field::new("address.zip", exact(r#""5003""#))],
    );
    save(fresh, Preset::Open).await.unwrap();
    assert_eq!(read(1).await["address"], json!({ "city": "Tromso", "zip": "9000" }));

    // Bulk: the preview reads (allowed in a Read-only workspace) and simulates the update.
    let invoke = |name: &'static str, extra: serde_json::Value, preset: Preset, confirmation: Option<gateway::Confirmation>| {
        let mut args = json!({ "database": "fosta_contract", "collection": "contract_m5" });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (manager, context) = (&manager, context(preset));
        async move { gateway::invoke(manager, &context, gateway::Caller::EngineView, name, args, confirmation.as_ref()).await }
    };
    let preview = invoke(
        "mongo.updatePreview",
        json!({ "filter": { "status": "new" }, "update": { "$inc": { "qty": 1 }, "$set": { "status": "done" }, "$rename": { "tags": "labels" } } }),
        Preset::ReadOnly,
        None,
    )
    .await
    .unwrap();
    assert_eq!(preview["matched"], json!(1), "{preview}");
    let sample = &preview["sample"][0];
    assert_eq!(sample["before"]["_id"], json!(2));
    assert_eq!(sample["after"]["qty"], json!(1));
    assert_eq!(sample["after"]["status"], json!("done"));
    assert_eq!(sample["after"]["labels"], json!([]));
    assert!(sample["after"].get("tags").is_none(), "{sample}");
    assert_eq!(sample["changed"], json!(true));
    assert!(preview["statement"].as_str().unwrap().contains(".updateMany("));
    // The preview changed nothing.
    assert_eq!(read(2).await["status"], json!("new"));
    let refused = invoke(
        "mongo.updatePreview",
        json!({ "update": { "$addToSet": { "tags": "x" } } }),
        Preset::Open,
        None,
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("cannot be previewed"), "{refused}");

    // updateMany: blocked in Read-only, confirmed in Guarded (typed with an empty filter).
    let update = json!({ "filter": { "status": "new" }, "update": { "$set": { "status": "done" } } });
    assert_eq!(
        invoke("mongo.updateMany", update.clone(), Preset::ReadOnly, None)
            .await
            .unwrap_err()
            .code(),
        Some(gateway::BLOCKED)
    );
    let ask = invoke("mongo.updateMany", update.clone(), Preset::Guarded, None).await.unwrap_err();
    assert_eq!(ask.code(), Some(gateway::CONFIRMATION_REQUIRED));
    assert_eq!(ask.payload().unwrap()["level"], "confirm");
    let token = ask.payload().unwrap()["token"].as_str().unwrap().to_owned();
    let done = invoke(
        "mongo.updateMany",
        update,
        Preset::Guarded,
        Some(gateway::Confirmation { token, typed: None }),
    )
    .await
    .unwrap();
    assert_eq!((done["matched"].as_u64(), done["modified"].as_u64()), (Some(1), Some(1)));
    assert_eq!(read(2).await["status"], json!("done"));
    let everything = json!({ "filter": {}, "update": [{ "$set": { "seen": true } }] });
    let ask = invoke("mongo.updateMany", everything.clone(), Preset::Guarded, None)
        .await
        .unwrap_err();
    assert_eq!(ask.payload().unwrap()["level"], "typed");
    assert_eq!(ask.payload().unwrap()["expected"], "contract_m5");
    let token = ask.payload().unwrap()["token"].as_str().unwrap().to_owned();
    let all = invoke(
        "mongo.updateMany",
        everything,
        Preset::Guarded,
        Some(gateway::Confirmation {
            token,
            typed: Some("contract_m5".into()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(all["modified"].as_u64(), Some(3));
    // Filters that trivially match everything ask the same typed confirmation as an empty one.
    for filter in [
        json!({ "$expr": true }),
        json!({ "_id": { "$exists": true } }),
        json!({ "$and": [{}] }),
    ] {
        let ask = invoke(
            "mongo.updateMany",
            json!({ "filter": filter, "update": { "$set": { "seen": true } } }),
            Preset::Guarded,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(ask.payload().unwrap()["level"], "typed", "{filter}");
    }

    // deleteMany is destructive: typed in Guarded even with a filter.
    let delete = json!({ "filter": { "status": "paid" } });
    let ask = invoke("mongo.deleteMany", delete.clone(), Preset::Guarded, None).await.unwrap_err();
    assert_eq!(ask.payload().unwrap()["level"], "typed");
    let token = ask.payload().unwrap()["token"].as_str().unwrap().to_owned();
    let deleted = invoke(
        "mongo.deleteMany",
        delete,
        Preset::Guarded,
        Some(gateway::Confirmation {
            token,
            typed: Some("contract_m5".into()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(deleted["deleted"].as_u64(), Some(1));
    assert!(read(3).await.is_null());
    run(&handle, &tab, r#"{ "drop": "contract_m5" }"#, "fosta_contract").await.ok();
    manager.disconnect(CONNECTION).await;
}

/// The canonical Extended JSON v2 wrappers the query bar's shell parser (`src/lang/mongo/shell.ts`) emits parse to
/// the right BSON types, through the filter check, `values::parse_document` and the operations' own argument path.
#[test]
fn shell_parser_wrappers_parse_to_their_bson_types() {
    use mongodb::bson::{spec::BinarySubtype, spec::ElementType};
    let text = r#"{
        "oid": { "$oid": "65f1a2b3c4d5e6f708192a3b" },
        "date": { "$date": { "$numberLong": "1735689600000" } },
        "long": { "$numberLong": "9007199254740993" },
        "int": { "$numberInt": "42" },
        "decimal": { "$numberDecimal": "12.50" },
        "double": { "$numberDouble": "1.0" },
        "infinity": { "$numberDouble": "Infinity" },
        "binary": { "$binary": { "base64": "AAECAw==", "subType": "00" } },
        "uuid": { "$binary": { "base64": "q83vEjRWeJCrze8SNFZ4kA==", "subType": "04" } },
        "timestamp": { "$timestamp": { "t": 1735689600, "i": 3 } },
        "regex": { "$regularExpression": { "pattern": "^a.c$", "options": "imsx" } },
        "min": { "$minKey": 1 },
        "max": { "$maxKey": 1 },
        "plainInt": 7,
        "plainDouble": 2.5,
        "text": "x",
        "flag": true,
        "nothing": null
    }"#;
    assert!(gateway::filter::validate_mongo_filter(text).is_ok());
    let parsed = super::values::parse_document(text).unwrap();
    let from_arg = super::ops::document_arg(
        &json!({ "filter": serde_json::from_str::<serde_json::Value>(text).unwrap() }),
        "filter",
    )
    .unwrap()
    .unwrap();
    let from_text = super::ops::document_arg(&json!({ "filter": text }), "filter").unwrap().unwrap();
    assert_eq!(parsed, from_arg);
    assert_eq!(parsed, from_text);
    for (key, expected) in [
        ("oid", ElementType::ObjectId),
        ("date", ElementType::DateTime),
        ("long", ElementType::Int64),
        ("int", ElementType::Int32),
        ("decimal", ElementType::Decimal128),
        ("double", ElementType::Double),
        ("infinity", ElementType::Double),
        ("binary", ElementType::Binary),
        ("uuid", ElementType::Binary),
        ("timestamp", ElementType::Timestamp),
        ("regex", ElementType::RegularExpression),
        ("min", ElementType::MinKey),
        ("max", ElementType::MaxKey),
        ("plainInt", ElementType::Int32),
        ("plainDouble", ElementType::Double),
        ("text", ElementType::String),
        ("flag", ElementType::Boolean),
        ("nothing", ElementType::Null),
    ] {
        assert_eq!(parsed.get(key).map(mongodb::bson::Bson::element_type), Some(expected), "{key}");
    }
    assert_eq!(parsed.get_i64("long").unwrap(), 9_007_199_254_740_993);
    assert_eq!(parsed.get_datetime("date").unwrap().timestamp_millis(), 1_735_689_600_000);
    assert_eq!(parsed.get_f64("infinity").unwrap(), f64::INFINITY);
    match parsed.get("uuid") {
        Some(mongodb::bson::Bson::Binary(binary)) => assert_eq!(binary.subtype, BinarySubtype::Uuid),
        other => panic!("uuid: {other:?}"),
    }
    match parsed.get("timestamp") {
        Some(mongodb::bson::Bson::Timestamp(ts)) => assert_eq!((ts.time, ts.increment), (1_735_689_600, 3)),
        other => panic!("timestamp: {other:?}"),
    }
    match parsed.get("regex") {
        Some(mongodb::bson::Bson::RegularExpression(regex)) => {
            assert_eq!((regex.pattern.as_str(), regex.options.as_str()), ("^a.c$", "imsx"))
        }
        other => panic!("regex: {other:?}"),
    }
    // A wrapper used as a query value still passes the check, server-side JavaScript next to it does not.
    assert!(gateway::filter::validate_mongo_filter(r#"{ "_id": { "$gt": { "$oid": "65f1a2b3c4d5e6f708192a3b" } } }"#).is_ok());
    assert!(gateway::filter::validate_mongo_filter(r#"{ "d": { "$date": { "$numberLong": "0" } }, "$where": "true" }"#).is_err());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_schema() {
    let f = fixture();
    let (manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    // Orders referencing customers by ObjectId, for the inferred references.
    let customers: Vec<serde_json::Value> = (0..5)
        .map(|i| json!({ "_id": { "$oid": format!("65f1a2b3c4d5e6f70819{i:04}") }, "name": format!("c{i}") }))
        .collect();
    let orders: Vec<serde_json::Value> = (0..20)
        .map(|i| json!({ "_id": i, "customerId": { "$oid": format!("65f1a2b3c4d5e6f70819{:04}", i % 5) }, "total": i }))
        .collect();
    for command in [
        json!({ "drop": "contract_schema_customers" }),
        json!({ "drop": "contract_schema_orders" }),
    ] {
        run(&handle, &tab, &command.to_string(), "fosta_contract").await.ok();
    }
    for (collection, documents) in [("contract_schema_customers", customers), ("contract_schema_orders", orders)] {
        let insert = json!({ "insert": collection, "documents": documents }).to_string();
        run(&handle, &tab, &insert, "fosta_contract").await.unwrap();
    }
    let context = Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: CONNECTION.into(),
        connection_name: CONNECTION.into(),
        safety: SafetySettings::preset(Preset::ReadOnly),
    };
    let invoke = |collection: &'static str, extra: serde_json::Value| {
        let mut args = json!({ "database": "fosta_contract", "collection": collection });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (manager, context) = (&manager, &context);
        async move { gateway::invoke(manager, context, gateway::Caller::EngineView, "mongo.schema", args, None).await }
    };
    let path = |schema: &serde_json::Value, name: &str| -> serde_json::Value {
        schema["paths"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name}: {schema}"))
    };

    // The first 300 items: presence, types, arrays, nested paths, min / max and top values.
    let schema = invoke("contract_items", json!({ "sampleSize": 300, "references": false }))
        .await
        .unwrap();
    assert_eq!(schema["sample"], json!({ "mode": "first", "size": 300, "count": 300 }), "{schema}");
    assert_eq!(schema["truncated"], false);
    assert_eq!(path(&schema, "_id")["min"], 1);
    assert_eq!(path(&schema, "_id")["max"], 300);
    assert_eq!(path(&schema, "price")["types"], json!([{ "type": "decimal", "count": 300 }]));
    assert_eq!(path(&schema, "born")["types"][0]["type"], "date");
    assert_eq!(path(&schema, "blob_data")["types"][0]["type"], "binData");
    let note = path(&schema, "note");
    assert_eq!(note["count"], 300);
    assert_eq!(
        note["types"],
        json!([{ "type": "string", "count": 200 }, { "type": "null", "count": 100 }])
    );
    assert_eq!(note["mixed"], false);
    let active = path(&schema, "active");
    assert_eq!(active["distinct"], 2);
    assert_eq!(active["topValues"].as_array().unwrap().len(), 2);
    let tags = path(&schema, "meta.tags");
    assert_eq!(tags["depth"], 1);
    assert_eq!(tags["array"]["minLength"], 2);
    assert_eq!(tags["array"]["maxLength"], 2);
    assert_eq!(
        tags["topValues"],
        json!([{ "value": "a", "count": 300 }, { "value": "b", "count": 300 }])
    );
    assert_eq!(path(&schema, "name")["distinct"], serde_json::Value::Null);

    // A filtered sample, and a random one.
    let filtered = invoke(
        "contract_items",
        json!({ "sampleMode": "filtered", "filter": "{ \"active\": true }", "sampleSize": 50, "references": false }),
    )
    .await
    .unwrap();
    assert_eq!(filtered["sample"]["count"], 50);
    assert_eq!(path(&filtered, "active")["topValues"], json!([{ "value": true, "count": 50 }]));
    let random = invoke("contract_items", json!({ "sampleMode": "random", "sampleSize": 40 }))
        .await
        .unwrap();
    assert_eq!(random["sample"]["count"], 40);

    // Inferred references: customerId holds _ids of contract_schema_customers.
    let orders = invoke("contract_schema_orders", json!({})).await.unwrap();
    let reference = &path(&orders, "customerId")["reference"];
    assert_eq!(reference["collection"], "contract_schema_customers", "{orders}");
    assert_eq!((reference["matched"].as_u64(), reference["probed"].as_u64()), (Some(5), Some(5)));
    assert_eq!(path(&orders, "total")["reference"], serde_json::Value::Null);

    // Server-side JavaScript in the filter is refused; a bad mode fails.
    let refused = invoke(
        "contract_items",
        json!({ "sampleMode": "filtered", "filter": { "$where": "true" } }),
    )
    .await;
    assert!(refused.is_err());
    assert!(invoke("contract_items", json!({ "sampleMode": "all" })).await.is_err());

    for command in [
        json!({ "drop": "contract_schema_customers" }),
        json!({ "drop": "contract_schema_orders" }),
    ] {
        run(&handle, &tab, &command.to_string(), "fosta_contract").await.ok();
    }
    manager.disconnect(CONNECTION).await;
}

/// The Indexes section's operations: the listing with options, sizes and usage; creating (compound with a partial filter
/// and a collation, TTL, text, wildcard, hidden), hiding and unhiding, dropping with a confirmation; `_id_` and
/// server-side JavaScript refused, Read-only blocking DDL.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_indexes() {
    let f = fixture();
    let (manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    run(&handle, &tab, r#"{ "drop": "contract_indexes" }"#, "fosta_contract").await.ok();
    let documents: Vec<serde_json::Value> = (0..30)
        .map(|i| {
            json!({
                "_id": i,
                "status": if i % 3 == 0 { "paid" } else { "open" },
                "createdAt": { "$date": format!("2026-01-{:02}T00:00:00Z", i % 28 + 1) },
                "title": format!("order number {i}"),
                "meta": { "k": i },
            })
        })
        .collect();
    let insert = json!({ "insert": "contract_indexes", "documents": documents }).to_string();
    run(&handle, &tab, &insert, "fosta_contract").await.unwrap();
    let context = |preset: Preset| Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: CONNECTION.into(),
        connection_name: CONNECTION.into(),
        safety: SafetySettings::preset(preset),
    };
    let invoke = |name: &'static str, extra: serde_json::Value, preset: Preset, confirmation: Option<gateway::Confirmation>| {
        let mut args = json!({ "database": "fosta_contract", "collection": "contract_indexes" });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (manager, context) = (&manager, context(preset));
        async move { gateway::invoke(manager, &context, gateway::Caller::EngineView, name, args, confirmation.as_ref()).await }
    };
    let list = || {
        let invoke = &invoke;
        async move { invoke("mongo.indexes", json!({}), Preset::ReadOnly, None).await.unwrap() }
    };
    let index = |listed: &serde_json::Value, name: &str| -> serde_json::Value {
        listed["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|index| index["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name}: {listed}"))
    };

    // Only _id_ at first, with a size and usage.
    let listed = list().await;
    assert_eq!(listed["indexes"].as_array().unwrap().len(), 1, "{listed}");
    let id = index(&listed, "_id_");
    assert_eq!(id["keys"], json!([{ "field": "_id", "kind": 1 }]));
    assert!(id["size"].as_i64().is_some_and(|size| size > 0), "{listed}");
    assert!(id["usage"]["ops"].as_i64().is_some(), "{listed}");
    assert_eq!(listed["stats"]["sizes"], true);
    assert_eq!(listed["stats"]["usage"], true);

    // Read-only blocks DDL; Open builds without asking.
    let compound = json!({
        "keys": { "status": 1, "createdAt": -1 },
        "unique": false,
        "partialFilterExpression": "{ \"status\": { \"$eq\": \"paid\" } }",
        "collation": { "locale": "en", "strength": 2 },
    });
    assert_eq!(
        invoke("mongo.createIndex", compound.clone(), Preset::ReadOnly, None)
            .await
            .unwrap_err()
            .code(),
        Some(gateway::BLOCKED)
    );
    let created = invoke("mongo.createIndex", compound.clone(), Preset::Open, None).await.unwrap();
    assert_eq!(created["name"], "status_1_createdAt_-1");
    assert_eq!(created["created"], true);
    assert!(created["statement"]
        .as_str()
        .unwrap()
        .starts_with(r#"db.getCollection("contract_indexes").createIndex({"status":1,"createdAt":-1}, {"name":"status_1_createdAt_-1""#));
    let again = invoke("mongo.createIndex", compound, Preset::Open, None).await.unwrap();
    assert_eq!(again["created"], false);
    // A TTL index deletes documents on its own: destructive, typed in Guarded.
    let ttl = json!({ "keys": { "createdAt": 1 }, "name": "ttl_created", "expireAfterSeconds": 86_400 });
    let ask = invoke("mongo.createIndex", ttl.clone(), Preset::Guarded, None).await.unwrap_err();
    assert_eq!(ask.payload().unwrap()["level"], "typed", "{ask}");
    let ask = invoke("mongo.createIndex", ttl.clone(), Preset::Open, None).await.unwrap_err();
    assert_eq!(ask.code(), Some(gateway::CONFIRMATION_REQUIRED), "{ask}");
    let payload = ask.payload().unwrap();
    let confirmation = gateway::Confirmation {
        token: payload["token"].as_str().unwrap().to_owned(),
        typed: payload["expected"].as_str().map(str::to_owned),
    };
    invoke("mongo.createIndex", ttl, Preset::Open, Some(confirmation)).await.unwrap();
    for extra in [
        json!({ "keys": { "title": "text" }, "weights": { "title": 5 }, "defaultLanguage": "english" }),
        json!({ "keys": { "meta.$**": 1 } }),
        json!({ "keys": { "title": 1 }, "name": "title_hidden", "hidden": true, "sparse": true }),
    ] {
        invoke("mongo.createIndex", extra.clone(), Preset::Open, None)
            .await
            .unwrap_or_else(|error| panic!("{extra}: {error}"));
    }
    let listed = list().await;
    assert_eq!(listed["indexes"].as_array().unwrap().len(), 6, "{listed}");
    let compound = index(&listed, "status_1_createdAt_-1");
    assert_eq!(
        compound["keys"],
        json!([{ "field": "status", "kind": 1 }, { "field": "createdAt", "kind": -1 }])
    );
    assert_eq!(
        compound["options"]["partialFilterExpression"],
        json!({ "status": { "$eq": "paid" } })
    );
    assert_eq!(compound["options"]["collation"]["locale"], "en");
    assert_eq!(index(&listed, "ttl_created")["options"]["expireAfterSeconds"], 86_400);
    let text = index(&listed, "title_text");
    assert!(text["keys"].as_array().unwrap().iter().any(|key| key["kind"] == "text"), "{text}");
    assert_eq!(text["options"]["weights"], json!({ "title": 5 }));
    assert_eq!(text["options"]["default_language"], "english");
    assert_eq!(index(&listed, "meta.$**_1")["keys"], json!([{ "field": "meta.$**", "kind": 1 }]));
    let hidden = index(&listed, "title_hidden");
    assert_eq!(hidden["options"]["hidden"], true);
    assert_eq!(hidden["options"]["sparse"], true);
    assert!(listed["totalSize"].as_i64().is_some_and(|total| total > 0));

    // Unhide, hide; the _id index stays.
    let shown = invoke("mongo.unhideIndex", json!({ "name": "title_hidden" }), Preset::Open, None)
        .await
        .unwrap();
    assert_eq!(shown["hidden"], false);
    assert_eq!(shown["changed"], true);
    assert_eq!(index(&list().await, "title_hidden")["options"]["hidden"], serde_json::Value::Null);
    let hid = invoke("mongo.hideIndex", json!({ "name": "title_hidden" }), Preset::Open, None)
        .await
        .unwrap();
    assert_eq!(
        hid["statement"],
        r#"db.getCollection("contract_indexes").hideIndex("title_hidden")"#
    );
    assert_eq!(index(&list().await, "title_hidden")["options"]["hidden"], true);
    for name in ["mongo.dropIndex", "mongo.hideIndex"] {
        assert!(invoke(name, json!({ "name": "_id_" }), Preset::Open, None).await.is_err(), "{name}");
    }

    // Server-side JavaScript in a partial filter and bad keys are refused.
    let refused = invoke(
        "mongo.createIndex",
        json!({ "keys": { "a": 1 }, "partialFilterExpression": { "$where": "true" } }),
        Preset::Open,
        None,
    )
    .await;
    assert!(refused.is_err());
    assert!(invoke("mongo.createIndex", json!({ "keys": { "a": 5 } }), Preset::Open, None)
        .await
        .is_err());

    // Dropping asks (typed in Guarded: the collection name).
    let ask = invoke("mongo.dropIndex", json!({ "name": "ttl_created" }), Preset::Guarded, None)
        .await
        .unwrap_err();
    assert_eq!(ask.code(), Some(gateway::CONFIRMATION_REQUIRED), "{ask}");
    let payload = ask.payload().unwrap();
    assert_eq!(payload["level"], "typed");
    let token = payload["token"].as_str().unwrap().to_owned();
    let typed = payload["expected"].as_str().map(str::to_owned);
    let dropped = invoke(
        "mongo.dropIndex",
        json!({ "name": "ttl_created" }),
        Preset::Guarded,
        Some(gateway::Confirmation { token, typed }),
    )
    .await
    .unwrap();
    assert_eq!(
        dropped["statement"],
        r#"db.getCollection("contract_indexes").dropIndex("ttl_created")"#
    );
    assert_eq!(list().await["indexes"].as_array().unwrap().len(), 5);

    run(&handle, &tab, r#"{ "drop": "contract_indexes" }"#, "fosta_contract").await.ok();
    manager.disconnect(CONNECTION).await;
}

/// The Validation section's operations: reading the rules (none, then a `$jsonSchema` with a level and an action),
/// checking existing documents against a validator (count and samples, `$jsonSchema` and query operators), applying
/// through the gateway (Read-only blocks DDL), writes refused by the rules, removing them; server-side JavaScript refused.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_validation() {
    let f = fixture();
    let (manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    run(&handle, &tab, r#"{ "drop": "contract_validation" }"#, "fosta_contract")
        .await
        .ok();
    let documents: Vec<serde_json::Value> = (0..20)
        .map(|i| {
            if i % 5 == 0 {
                json!({ "_id": i, "qty": format!("{i}") })
            } else {
                json!({ "_id": i, "status": if i % 2 == 0 { "paid" } else { "open" }, "qty": i })
            }
        })
        .collect();
    let insert = json!({ "insert": "contract_validation", "documents": documents }).to_string();
    run(&handle, &tab, &insert, "fosta_contract").await.unwrap();
    let context = |preset: Preset| Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: CONNECTION.into(),
        connection_name: CONNECTION.into(),
        safety: SafetySettings::preset(preset),
    };
    let invoke = |name: &'static str, extra: serde_json::Value, preset: Preset| {
        let mut args = json!({ "database": "fosta_contract", "collection": "contract_validation" });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (manager, context) = (&manager, context(preset));
        async move { gateway::invoke(manager, &context, gateway::Caller::EngineView, name, args, None).await }
    };

    // No rules yet: the server's defaults.
    let rules = invoke("mongo.validation", json!({}), Preset::ReadOnly).await.unwrap();
    assert_eq!(rules["validator"], serde_json::Value::Null, "{rules}");
    assert_eq!(rules["validationLevel"], "strict");
    assert_eq!(rules["validationAction"], "error");
    assert_eq!(rules["type"], "collection");

    // Checking: 4 documents lack a status and hold a string qty.
    let schema = json!({ "$jsonSchema": {
        "bsonType": "object",
        "required": ["status", "qty"],
        "properties": { "status": { "enum": ["paid", "open"] }, "qty": { "bsonType": ["int", "long"], "minimum": 0 } },
    } });
    let check = invoke(
        "mongo.validationCheck",
        json!({ "validator": schema.clone(), "limit": 2 }),
        Preset::ReadOnly,
    )
    .await
    .unwrap();
    assert_eq!(check["failing"], 4, "{check}");
    assert_eq!(check["capped"], false);
    assert_eq!(check["total"], 20);
    assert_eq!(check["samples"].as_array().unwrap().len(), 2);
    assert_eq!(check["samples"][0]["_id"], 0);
    assert!(check["statement"].as_str().unwrap().contains("$nor"), "{check}");
    let operators = invoke(
        "mongo.validationCheck",
        json!({ "validator": "{ \"qty\": { \"$gte\": 10 } }" }),
        Preset::ReadOnly,
    )
    .await
    .unwrap();
    // qty < 10 (1..9 without 5) and the string quantities fail.
    assert_eq!(operators["failing"], 12, "{operators}");
    let none = invoke("mongo.validationCheck", json!({ "validator": {} }), Preset::ReadOnly)
        .await
        .unwrap();
    assert_eq!(none["failing"], 0);

    // Applying is DDL: Read-only blocks it, Open applies.
    let apply = json!({ "validator": schema.clone(), "validationLevel": "moderate", "validationAction": "error" });
    assert_eq!(
        invoke("mongo.setValidation", apply.clone(), Preset::ReadOnly)
            .await
            .unwrap_err()
            .code(),
        Some(gateway::BLOCKED)
    );
    let applied = invoke("mongo.setValidation", apply, Preset::Open).await.unwrap();
    assert!(applied["statement"]
        .as_str()
        .unwrap()
        .starts_with(r#"db.runCommand({"collMod":"contract_validation","validator":"#));
    let rules = invoke("mongo.validation", json!({}), Preset::ReadOnly).await.unwrap();
    assert_eq!(rules["validator"]["$jsonSchema"]["required"], json!(["status", "qty"]), "{rules}");
    assert_eq!(rules["validationLevel"], "moderate");
    assert_eq!(rules["validationAction"], "error");

    // The server refuses a document that breaks the rules.
    let bad = json!({ "insert": "contract_validation", "documents": [{ "_id": 100, "qty": 1 }] }).to_string();
    let refused = run(&handle, &tab, &bad, "fosta_contract").await;
    assert!(refused.is_err() || format!("{refused:?}").contains("writeErrors"), "{refused:?}");

    // Level and action alone; an empty validator removes the rules.
    invoke("mongo.setValidation", json!({ "validationAction": "warn" }), Preset::Open)
        .await
        .unwrap();
    let rules = invoke("mongo.validation", json!({}), Preset::ReadOnly).await.unwrap();
    assert_eq!(rules["validationAction"], "warn");
    assert_eq!(rules["validationLevel"], "moderate");
    invoke(
        "mongo.setValidation",
        json!({ "validator": {}, "validationLevel": "strict" }),
        Preset::Open,
    )
    .await
    .unwrap();
    let rules = invoke("mongo.validation", json!({}), Preset::ReadOnly).await.unwrap();
    assert_eq!(rules["validator"], serde_json::Value::Null, "{rules}");

    // Server-side JavaScript and bad levels are refused.
    for extra in [
        json!({ "validator": { "$expr": { "$function": { "body": "function() { return true }", "args": [], "lang": "js" } } } }),
        json!({ "validator": { "$where": "true" } }),
        json!({ "validationLevel": "lax" }),
        json!({}),
    ] {
        assert!(invoke("mongo.setValidation", extra.clone(), Preset::Open).await.is_err(), "{extra}");
    }
    assert!(
        invoke("mongo.validationCheck", json!({ "validator": { "$where": "true" } }), Preset::Open)
            .await
            .is_err()
    );

    run(&handle, &tab, r#"{ "drop": "contract_validation" }"#, "fosta_contract")
        .await
        .ok();
    manager.disconnect(CONNECTION).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/mongo.yml"]
async fn contract_mongo_explain() {
    let f = fixture();
    let (manager, handle) = connect(&f).await;
    let tab = session(&handle).await;
    run(&handle, &tab, r#"{ "drop": "contract_explain" }"#, "fosta_contract").await.ok();
    const STATUSES: [&str; 4] = ["open", "paid", "shipped", "void"];
    let documents: Vec<serde_json::Value> = (0..500)
        .map(|i| json!({ "_id": i, "status": STATUSES[i % 4], "total": i, "note": format!("note {i}") }))
        .collect();
    let insert = json!({ "insert": "contract_explain", "documents": documents }).to_string();
    run(&handle, &tab, &insert, "fosta_contract").await.unwrap();
    let indexes = json!({ "createIndexes": "contract_explain", "indexes": [
        { "key": { "status": 1 }, "name": "status_1" },
        { "key": { "total": 1 }, "name": "total_1" },
    ] })
    .to_string();
    run(&handle, &tab, &indexes, "fosta_contract").await.unwrap();
    let context = Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: CONNECTION.into(),
        connection_name: CONNECTION.into(),
        safety: SafetySettings::preset(Preset::ReadOnly),
    };
    let explain = |extra: serde_json::Value| {
        let mut args = json!({ "database": "fosta_contract", "collection": "contract_explain" });
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (manager, context) = (&manager, &context);
        async move { gateway::invoke(manager, context, gateway::Caller::EngineView, "mongo.explain", args, None).await }
    };

    // A filtered, sorted find: the plan uses an index, the summary has the counters (a read, allowed in Read-only).
    let found = explain(json!({
        "kind": "find",
        "filter": { "status": "paid", "total": { "$gt": 100 } },
        "sort": { "note": 1 },
        "limit": 10,
        "verbosity": "allPlansExecution",
    }))
    .await
    .unwrap();
    assert_eq!(found["verbosity"], "allPlansExecution");
    assert!(found["explain"]["queryPlanner"].is_object(), "{found}");
    assert!(found["statement"]
        .as_str()
        .unwrap()
        .contains(".explain(\"allPlansExecution\").find("));
    let plan = &found["plan"];
    assert_eq!(plan["shape"], "find", "{plan}");
    let summary = &plan["summary"];
    assert_eq!(summary["nReturned"], 10, "{summary}");
    assert_eq!(summary["isCollScan"], false, "{summary}");
    assert_eq!(summary["sortInMemory"], true, "{summary}");
    assert!(
        summary["usedIndexes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "status_1" || name == "total_1"),
        "{summary}"
    );
    assert!(summary["docsExamined"].as_i64().unwrap() >= 10, "{summary}");
    assert!(!plan["rejectedPlans"].as_array().unwrap().is_empty(), "{plan}");

    // queryPlanner only: no counters; a scan without an index is a COLLSCAN.
    let scanned = explain(json!({ "filter": "{ \"note\": \"note 7\" }" })).await.unwrap();
    assert_eq!(scanned["plan"]["summary"]["isCollScan"], true, "{scanned}");
    assert_eq!(scanned["plan"]["summary"]["nReturned"], serde_json::Value::Null);

    // A pipeline: the group above the query layer (or pushed into it), with the index of the $match.
    let grouped = explain(json!({
        "kind": "aggregate",
        "pipeline": [{ "$match": { "status": "open" } }, { "$group": { "_id": "$status", "n": { "$sum": 1 } } }, { "$sort": { "n": -1 } }],
        "verbosity": "executionStats",
    }))
    .await
    .unwrap();
    let plan = &grouped["plan"];
    assert!(plan.is_object(), "{grouped}");
    assert_eq!(plan["summary"]["usedIndexes"], json!(["status_1"]), "{plan}");
    assert_eq!(plan["summary"]["nReturned"], 1, "{plan}");

    // $out / $merge, server-side JavaScript and bad verbosities are refused.
    assert!(explain(json!({ "kind": "aggregate", "pipeline": [{ "$out": "copy" }] }))
        .await
        .is_err());
    assert!(explain(json!({ "filter": { "$where": "true" } })).await.is_err());
    assert!(explain(json!({ "verbosity": "everything" })).await.is_err());

    run(&handle, &tab, r#"{ "drop": "contract_explain" }"#, "fosta_contract").await.ok();
    manager.disconnect(CONNECTION).await;
}
