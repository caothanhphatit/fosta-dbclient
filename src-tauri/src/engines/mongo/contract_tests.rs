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
