//! Contract tests for Redis against `dev/compose/redis.yml` (127.0.0.1:6379, password `studio-redis`; override with
//! `FOSTA_CONTRACT_REDIS_HOST` / `_PORT` / `_PASSWORD`). Run: `cargo test contract_redis -- --ignored --test-threads=1`.
//!
//! The shared harness has tabular suites (tree to a table, describe columns, browse, edit rows) that a key-value engine
//! cannot pass, and no `suite_kv` yet, so this module generates the suites that apply (`classify`, `connect`, `query`,
//! `cancel`) and adds the key-value suites below. They work in database 14 (the profile's database) and 15 on keys that
//! start with `contract:`, so the dev seed in databases 0 and 1 is never touched.

use serde_json::{json, Value as Json};

use super::{
    edit::RedisEdit,
    guard::{apply_guarded_after, Expect},
    types::Blob,
};
use crate::{
    contract::{
        errors::CONFLICT,
        testing::suites::{self, Fixture},
        CallContext, Category, CollectSink, ConnectSecrets, ConnectionSpec, Filter, NodeKey, NodeKind, ObjectRef, RunOptions, RunRequest,
        Session, SessionOptions, TreeRequest, Value,
    },
    gateway,
    runtime::Handle,
};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_REDIS_{name}")).unwrap_or_else(|_| default.to_owned())
}

const PAGE_KEYS: usize = 250;

/// The commands that (re)create the `contract:*` keys of database 14.
fn seed() -> Vec<String> {
    let page: Vec<String> = (0..PAGE_KEYS).map(|i| format!("contract:page:{i}")).collect();
    let mut delete = vec![
        "contract:str",
        "contract:json",
        "contract:hash",
        "contract:list",
        "contract:set",
        "contract:zset",
        "contract:stream",
        "contract:ttl",
        "contract:edit:str",
        "contract:edit:hash",
        "contract:edit:list",
        "contract:edit:set",
        "contract:edit:zset",
        "contract:edit:new",
        "contract:edit:race",
        "contract:edit:ttl",
        "contract:edit:stream",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    delete.extend(page.iter().cloned());
    let mset = page.iter().map(|key| format!("{key} v")).collect::<Vec<_>>().join(" ");
    vec![
        format!("DEL {}", delete.join(" ")),
        format!("MSET {mset}"),
        "SET contract:str hello".into(),
        r#"SET contract:json '{"name":"Ada","tags":["a","b"]}'"#.into(),
        "HSET contract:hash name Ada role admin".into(),
        "RPUSH contract:list a b c".into(),
        "SADD contract:set x y z".into(),
        "ZADD contract:zset 1 one 2 two 3 three".into(),
        "XADD contract:stream 1-1 field one".into(),
        "XADD contract:stream 2-1 field two".into(),
        "SET contract:ttl v EX 3600".into(),
    ]
}

fn key_filter(_: i64) -> Filter {
    Filter::None
}

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        engine: "redis".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "6379").parse().unwrap_or(6379),
        options: [("db".to_owned(), "14".to_owned())].into_iter().collect(),
        ..ConnectionSpec::default()
    }
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "redis",
        spec: spec(),
        secrets: ConnectSecrets::password(&env("PASSWORD", "studio-redis")),
        table: ObjectRef::new(Some("14"), None, "contract:str", NodeKind::KEY),
        child: ObjectRef::new(Some("14"), None, "contract:hash", NodeKind::KEY),
        key_column: "",
        name_column: "",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("BLPOP contract:never 30"),
        // A server-side failure each: wrong arity (ERR) and a hash command on a string (WRONGTYPE).
        syntax_error: "SET contract:k",
        constraint_violation: "HSET contract:str field value",
        multi_statement: "SET contract:multi 1\nGET contract:str",
        insert_probe: "SET contract:probe 1",
        count_probe: "GET contract:str",
        classify_samples: &[
            ("GET k", Category::Read),
            ("KEYS user:*", Category::Read),
            ("SET k v", Category::Write),
            ("HSET h f v", Category::Write),
            ("DEL k", Category::Write),
            ("FLUSHALL", Category::Destructive),
            ("FLUSHDB", Category::Destructive),
            ("CONFIG SET maxmemory 1gb", Category::Admin),
            ("KEYS *", Category::Admin),
            ("DEBUG SLEEP 0", Category::Admin),
            ("SHUTDOWN NOSAVE", Category::Admin),
        ],
        language: "redis",
        gaps: &[],
    }
}

mod contract_redis {
    use super::*;
    use crate::contract::testing::suites;

    #[test]
    fn classify() {
        suites::suite_classify(&fixture());
    }

    #[test]
    fn assistant_reads_only() {
        suites::suite_assistant_reads_only(&fixture());
    }

    crate::contract_tests!(@live fixture(); suite_connect suite_query suite_cancel suite_assistant);
}

// --- Key-value suites --------------------------------------------------------------------------------------------------

async fn call(handle: &Handle, name: &'static str, args: Json) -> crate::error::AppResult<Json> {
    handle
        .call("invoked an operation", move |conn| async move {
            conn.invoke(name, args, &CallContext::default()).await
        })
        .await
}

fn text(value: &Json) -> &str {
    value["text"].as_str().unwrap_or_default()
}

fn key(name: &str) -> Json {
    json!({ "text": name })
}

async fn edit(handle: &Handle, db: i64, mut edit: Json) -> crate::error::AppResult<Json> {
    edit["db"] = json!(db);
    call(handle, "redis.edit", edit).await
}

async fn value_of(handle: &Handle, db: i64, name: &str) -> Json {
    call(handle, "redis.readValue", json!({ "db": db, "key": key(name) }))
        .await
        .unwrap_or_else(|error| panic!("read {name}: {error}"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_kv_scan_pages_with_type_ttl_and_memory() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let mut seen = std::collections::HashSet::new();
    let mut cursor = String::new();
    let mut pages = 0;
    loop {
        let page = call(
            &handle,
            "redis.scan",
            json!({ "db": 14, "cursor": cursor, "pattern": "contract:page:*", "pageSize": 100, "count": 50, "withMemory": true }),
        )
        .await
        .expect("scan");
        pages += 1;
        for row in page["keys"].as_array().unwrap() {
            assert_eq!(row["keyType"], "string");
            assert_eq!(row["ttlMs"], -1);
            assert!(row["memoryBytes"].as_u64().unwrap_or(0) > 0, "memory of {row}");
            seen.insert(text(&row["key"]).to_owned());
        }
        assert!(page["dbSize"].as_u64().unwrap() >= PAGE_KEYS as u64);
        if page["done"].as_bool().unwrap() {
            break;
        }
        cursor = page["cursor"].as_str().unwrap().to_owned();
        assert!(pages < 50, "the cursor never finishes");
    }
    assert_eq!(seen.len(), PAGE_KEYS, "every key exactly once across {pages} pages");
    assert!(pages >= 2, "100 keys per page needs several pages");
    let none = call(&handle, "redis.scan", json!({ "db": 14, "pattern": "contract:nothing:*" }))
        .await
        .unwrap();
    assert!(none["keys"].as_array().unwrap().is_empty());
    let ttl = call(&handle, "redis.scan", json!({ "db": 14, "pattern": "contract:ttl" }))
        .await
        .unwrap();
    let ttl_ms = ttl["keys"][0]["ttlMs"].as_i64().unwrap();
    assert!((3_000_000..=3_600_000).contains(&ttl_ms), "{ttl_ms}");
    let typed = call(
        &handle,
        "redis.scan",
        json!({ "db": 14, "pattern": "contract:*", "typeFilter": "zset" }),
    )
    .await
    .unwrap();
    assert_eq!(typed["keys"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_kv_values_of_every_type() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let string = value_of(&handle, 14, "contract:str").await;
    assert_eq!(string["info"]["keyType"], "string");
    assert_eq!(string["value"]["type"], "string");
    assert_eq!(text(&string["value"]["data"]["value"]), "hello");
    assert_eq!(string["value"]["data"]["format"], "text");
    let json_string = value_of(&handle, 14, "contract:json").await;
    assert_eq!(json_string["value"]["data"]["format"], "json");
    assert_eq!(json_string["value"]["data"]["json"]["tags"][1], "b");
    let hash = value_of(&handle, 14, "contract:hash").await;
    assert_eq!(hash["value"]["type"], "hash");
    assert_eq!(hash["value"]["data"]["total"], 2);
    assert_eq!(hash["value"]["data"]["items"].as_array().unwrap().len(), 2);
    let list = value_of(&handle, 14, "contract:list").await;
    assert_eq!(
        list["value"]["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(text)
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    let set = value_of(&handle, 14, "contract:set").await;
    assert_eq!(set["value"]["data"]["total"], 3);
    let zset = value_of(&handle, 14, "contract:zset").await;
    assert_eq!(text(&zset["value"]["data"]["items"][2]["member"]), "three");
    assert_eq!(zset["value"]["data"]["items"][2]["score"], 3.0);
    let stream = value_of(&handle, 14, "contract:stream").await;
    assert_eq!(stream["value"]["type"], "stream");
    assert_eq!(stream["value"]["data"]["page"]["entries"].as_array().unwrap().len(), 2);
    assert_eq!(stream["value"]["data"]["info"]["lastId"], "2-1");
    let missing = value_of(&handle, 14, "contract:does-not-exist").await;
    assert_eq!(missing["value"]["type"], "none");
    assert_eq!(string["info"]["length"], 5);
    assert!(string["info"]["memoryBytes"].as_u64().unwrap() > 0);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_kv_edits_set_and_remove_ttl_and_append_to_streams() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let set = |name: &str, value: &str| json!({ "op": "setString", "key": key(name), "value": key(value), "ttlMs": null, "keepTtl": false, "condition": "always" });
    edit(&handle, 14, set("contract:edit:ttl", "v")).await.unwrap();
    edit(
        &handle,
        14,
        json!({ "op": "expire", "key": key("contract:edit:ttl"), "ttlMs": 120_000 }),
    )
    .await
    .unwrap();
    let info = call(&handle, "redis.keyInfo", json!({ "db": 14, "key": key("contract:edit:ttl") }))
        .await
        .unwrap();
    assert!((100_000..=120_000).contains(&info["ttlMs"].as_i64().unwrap()), "{info}");
    edit(&handle, 14, json!({ "op": "persist", "key": key("contract:edit:ttl") }))
        .await
        .unwrap();
    let info = call(&handle, "redis.keyInfo", json!({ "db": 14, "key": key("contract:edit:ttl") }))
        .await
        .unwrap();
    assert_eq!(info["ttlMs"], -1);
    let appended = edit(
        &handle,
        14,
        json!({ "op": "streamAdd", "key": key("contract:edit:stream"), "id": null, "fields": [{ "field": key("kind"), "value": key("click") }], "maxLen": null }),
    )
    .await
    .unwrap();
    assert_eq!(appended["applied"], true);
    let stream = value_of(&handle, 14, "contract:edit:stream").await;
    assert_eq!(text(&stream["value"]["data"]["page"]["entries"][0]["fields"][0]["value"]), "click");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_kv_a_concurrent_change_is_a_conflict() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let name = "contract:edit:str";
    let set = |value: &str, expect: Json| json!({ "op": "setString", "key": key(name), "value": key(value), "ttlMs": null, "keepTtl": false, "condition": "always", "expect": expect });
    edit(&handle, 14, set("one", Json::Null)).await.unwrap();
    // The value the user loaded still holds: the write goes through.
    edit(&handle, 14, set("two", json!({ "kind": "string", "value": key("one") })))
        .await
        .unwrap();
    // Somebody else changed it: the stale expectation is a CONFLICT, nothing is written, the current value comes back.
    edit(&handle, 14, set("three", Json::Null)).await.unwrap();
    let error = edit(&handle, 14, set("lost", json!({ "kind": "string", "value": key("two") })))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(CONFLICT), "{error:?}");
    assert_eq!(text(&error.payload().unwrap()["current"]["value"]), "three");
    assert_eq!(text(&value_of(&handle, 14, name).await["value"]["data"]["value"]), "three");
    // A key that appeared meanwhile blocks "create".
    let error = edit(&handle, 14, set("new", json!({ "kind": "missing" }))).await.unwrap_err();
    assert_eq!(error.code(), Some(CONFLICT));
    // Hash field, list element, set member and sorted set score, each against its own part of the key.
    edit(
        &handle,
        14,
        json!({ "op": "hashSet", "key": key("contract:edit:hash"), "fields": [{ "field": key("f"), "value": key("1") }] }),
    )
    .await
    .unwrap();
    let stale = json!({ "kind": "hash", "fields": [{ "field": key("f"), "value": key("0") }] });
    let error = edit(
        &handle,
        14,
        json!({ "op": "hashSet", "key": key("contract:edit:hash"), "fields": [{ "field": key("f"), "value": key("2") }], "expect": stale }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), Some(CONFLICT));
    assert_eq!(text(&error.payload().unwrap()["current"]["fields"][0]["value"]), "1");
    edit(
        &handle,
        14,
        json!({ "op": "listPush", "key": key("contract:edit:list"), "values": [key("a"), key("b")], "end": "tail" }),
    )
    .await
    .unwrap();
    let ok = json!({ "op": "listSet", "key": key("contract:edit:list"), "index": 1, "value": key("B"), "expect": { "kind": "listItem", "index": 1, "value": key("b") } });
    edit(&handle, 14, ok).await.unwrap();
    let stale = json!({ "op": "listSet", "key": key("contract:edit:list"), "index": 1, "value": key("X"), "expect": { "kind": "listItem", "index": 1, "value": key("b") } });
    assert_eq!(edit(&handle, 14, stale).await.unwrap_err().code(), Some(CONFLICT));
    edit(
        &handle,
        14,
        json!({ "op": "setAdd", "key": key("contract:edit:set"), "members": [key("m")] }),
    )
    .await
    .unwrap();
    let gone = json!({ "op": "setRemove", "key": key("contract:edit:set"), "members": [key("m")], "expect": { "kind": "set", "members": [{ "member": key("m"), "present": false }] } });
    assert_eq!(edit(&handle, 14, gone).await.unwrap_err().code(), Some(CONFLICT));
    edit(
        &handle,
        14,
        json!({ "op": "zsetAdd", "key": key("contract:edit:zset"), "members": [{ "member": key("m"), "score": 1.0 }] }),
    )
    .await
    .unwrap();
    let stale = json!({ "op": "zsetAdd", "key": key("contract:edit:zset"), "members": [{ "member": key("m"), "score": 9.0 }], "expect": { "kind": "zset", "members": [{ "member": key("m"), "score": 2.0 }] } });
    assert_eq!(edit(&handle, 14, stale).await.unwrap_err().code(), Some(CONFLICT));
}

/// A change between the comparison and EXEC is caught by WATCH: the transaction aborts and nothing is written.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_kv_watch_aborts_a_change_made_before_exec() {
    use super::connection::{RedisConnectOptions, RedisEngine};
    let options = RedisConnectOptions {
        host: env("HOST", "127.0.0.1"),
        port: Some(env("PORT", "6379").parse().unwrap_or(6379)),
        password: Some(env("PASSWORD", "studio-redis")),
        db: 14,
        ..Default::default()
    };
    let engine = RedisEngine::connect(options).await.expect("Redis from dev/compose/redis.yml");
    let name = Blob::text("contract:edit:race");
    let set = |value: &str| RedisEdit::SetString {
        key: name.clone(),
        value: Blob::text(value),
        ttl_ms: None,
        keep_ttl: false,
        condition: Default::default(),
    };
    engine.apply_edit(&set("before")).await.unwrap();
    let expect = Expect::String {
        value: Some(Blob::text("before")),
    };
    let rival = engine.clone();
    let rival_edit = set("rival");
    let error = apply_guarded_after(&engine, &set("mine"), &expect, async move {
        rival.apply_edit(&rival_edit).await.unwrap();
    })
    .await
    .unwrap_err();
    assert_eq!(error.code(), Some(CONFLICT), "{error:?}");
    assert!(error.to_string().contains("while it was being saved"), "{error}");
    assert_eq!(engine.read_string(&name).await.unwrap().value, Blob::text("rival"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_kv_databases_are_routed_and_the_tree_pages_keys() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let children = |parent: Option<NodeKey>, cursor: Option<String>, filter: Option<String>| {
        let handle = handle.clone();
        async move {
            let request = TreeRequest {
                parent,
                cursor,
                limit: Some(100),
                filter,
            };
            handle
                .call("listed children", |conn| async move {
                    conn.children(&request, &CallContext::default()).await
                })
                .await
                .expect("children")
        }
    };
    let top = children(None, None, None).await;
    assert_eq!(top.nodes.len(), 16, "databases 0..15");
    assert_eq!(top.nodes[15].label, "db15");
    assert_eq!(top, children(None, None, None).await, "keys must be stable");
    let db14 = top.nodes[14].clone();
    assert_eq!(db14.kind, NodeKind::DATABASE);
    let first = children(Some(db14.key.clone()), None, Some("contract:page:*".into())).await;
    assert!(!first.nodes.is_empty());
    assert!(first.next_cursor.is_some(), "250 keys do not fit one page of 100");
    let key_node = &first.nodes[0];
    assert_eq!(key_node.kind, NodeKind::KEY);
    assert_eq!(key_node.badges[0].text, "string");
    assert_eq!(key_node.default_view.as_deref(), Some("value"));
    // A page ends with a SCAN call (COUNT = the limit), so it holds about the limit and may run past it by one batch.
    assert!(
        (1..=250).contains(&first.nodes.len()),
        "{} keys on the first page",
        first.nodes.len()
    );
    // Following the cursors yields every key. A page never repeats a key, but SCAN may return a key
    // again on a later page (the server can rehash between calls), so the tree drops repeats when it appends a page.
    let mut ids: std::collections::HashSet<String> = first.nodes.iter().map(|node| node.key.id.clone()).collect();
    let mut cursor = first.next_cursor.clone();
    let mut pages = 1;
    while let Some(next) = cursor {
        let page = children(Some(db14.key.clone()), Some(next), Some("contract:page:*".into())).await;
        let unique: std::collections::HashSet<&String> = page.nodes.iter().map(|node| &node.key.id).collect();
        assert_eq!(unique.len(), page.nodes.len(), "a page repeats a key");
        ids.extend(page.nodes.iter().map(|node| node.key.id.clone()));
        cursor = page.next_cursor;
        pages += 1;
        assert!(pages < 20, "the cursor never finishes");
    }
    assert_eq!(ids.len(), PAGE_KEYS);
    // A write in db15 is invisible in db14 and the other way round.
    edit(
        &handle,
        15,
        json!({ "op": "setString", "key": key("contract:routing"), "value": key("fifteen"), "ttlMs": 60_000, "keepTtl": false, "condition": "always" }),
    )
    .await
    .unwrap();
    let in15 = call(&handle, "redis.scan", json!({ "db": 15, "pattern": "contract:routing" }))
        .await
        .unwrap();
    let in14 = call(&handle, "redis.scan", json!({ "db": 14, "pattern": "contract:routing" }))
        .await
        .unwrap();
    assert_eq!(in15["keys"].as_array().unwrap().len(), 1);
    assert!(in14["keys"].as_array().unwrap().is_empty());
    edit(&handle, 15, json!({ "op": "delete", "keys": [key("contract:routing")] }))
        .await
        .unwrap();
    let error = call(&handle, "redis.scan", json!({ "db": 99 })).await.unwrap_err();
    assert!(error.to_string().contains("does not exist"), "{error}");
    let description = handle
        .call("described", |conn| async move {
            conn.describe(
                &ObjectRef::new(Some("14"), None, "contract:ttl", NodeKind::KEY),
                &CallContext::default(),
            )
            .await
        })
        .await
        .unwrap();
    assert!(description.extra.iter().any(|(name, value)| name == "Type" && value == "string"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_kv_info_and_slowlog() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let info = call(&handle, "redis.info", Json::Null).await.unwrap();
    assert!(!info["version"].as_str().unwrap().is_empty());
    assert_eq!(info["mode"], "standalone");
    assert!(info["sections"]["server"]["redis_version"].is_string() || info["sections"]["server"]["valkey_version"].is_string());
    assert!(info["keyspace"].as_array().unwrap().iter().any(|db| db["db"] == 14));
    let slow = call(&handle, "redis.slowlog", json!({ "count": 10 })).await.unwrap();
    assert!(slow["entries"].is_array());
    assert!(slow["length"].as_i64().is_some());
    assert!(slow["slowerThanUs"].is_string());
    let wrong = call(&handle, "redis.flush", json!({})).await.unwrap_err();
    assert_eq!(wrong.code(), Some("UNSUPPORTED_CAPABILITY"));
}

async fn run_in(handle: &Handle, session: &std::sync::Arc<dyn Session>, text: &str, database: Option<&str>) -> CollectSink {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: database.map(str::to_owned),
        offset: 0,
    };
    handle
        .run(std::sync::Arc::clone(session), request, RunOptions::default(), &mut sink)
        .await
        .unwrap_or_else(|error| panic!("{text}: {error}"));
    sink
}

fn first_cell(sink: &CollectSink) -> Value {
    sink.sets
        .last()
        .and_then(|set| set.rows.first())
        .map(|row| row[0].clone())
        .expect("a reply")
}

/// The console's database picker and "Flush database…" send the database with each run: it must land there, not on the
/// database the session was opened on (here 14, the profile's).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_a_run_lands_in_the_database_it_names() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let session = handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("session")
        .session;
    for db in [14, 15] {
        edit(&handle, db, json!({ "op": "delete", "keys": [key("contract:dbprobe")] }))
            .await
            .unwrap();
    }
    run_in(&handle, &session, "SET contract:dbprobe fifteen", Some("15")).await;
    let in15 = call(&handle, "redis.scan", json!({ "db": 15, "pattern": "contract:dbprobe" }))
        .await
        .unwrap();
    let in14 = call(&handle, "redis.scan", json!({ "db": 14, "pattern": "contract:dbprobe" }))
        .await
        .unwrap();
    assert_eq!(in15["keys"].as_array().unwrap().len(), 1, "the write must land in db15");
    assert!(in14["keys"].as_array().unwrap().is_empty(), "and not in the profile's db14");
    // The session stays where the last run put it, and another database reads what is there.
    let stays = run_in(&handle, &session, "GET contract:dbprobe", None).await;
    assert_eq!(first_cell(&stays), Value::Text("fifteen".into()));
    let other = run_in(&handle, &session, "GET contract:dbprobe", Some("db14")).await;
    assert_eq!(first_cell(&other), Value::Null);
    // A typed SELECT moves the session too.
    run_in(&handle, &session, "SELECT 15", None).await;
    let typed = run_in(&handle, &session, "GET contract:dbprobe", None).await;
    assert_eq!(first_cell(&typed), Value::Text("fifteen".into()));
    // A database that does not exist is the server's error.
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: "PING".into(),
        database: Some("99".into()),
        offset: 0,
    };
    let error = handle
        .run(session.clone(), request, RunOptions::default(), &mut sink)
        .await
        .unwrap_err();
    assert!(error.field("vendorCode").is_some(), "{error:?}");
    edit(&handle, 15, json!({ "op": "delete", "keys": [key("contract:dbprobe")] }))
        .await
        .unwrap();
}

// --- Vector suite --------------------------------------------------------------------------------------------------------

/// Search indexes live in database 0 only, so the vector data is there, under its own prefix and index (never FLUSHDB).
const VECTOR_INDEX: &str = "fosta_contract_vectors";
const VECTOR_PREFIX: &str = "fosta_contract:vec:";
const VECTOR_DOCS: u32 = 300;
/// A small JSON index for the attribute names a query must escape (dropped with its documents at the end).
const JSON_INDEX: &str = "fosta_contract_vectors_json";
const JSON_PREFIX: &str = "fosta_contract:jvec:";

/// (Re)creates the vector documents and their HNSW cosine index in database 0; `None` without the Query Engine.
async fn seed_vectors() -> Option<()> {
    let url = format!(
        "redis://:{}@{}:{}/0",
        env("PASSWORD", "studio-redis"),
        env("HOST", "127.0.0.1"),
        env("PORT", "6379")
    );
    let client = ::redis::Client::open(url).expect("redis url");
    let mut conn = client.get_multiplexed_async_connection().await.expect("connect to database 0");
    let modules = ::redis::cmd("MODULE")
        .arg("LIST")
        .query_async::<::redis::Value>(&mut conn)
        .await
        .map(super::types::as_items)
        .unwrap_or_default();
    let search = modules.into_iter().any(|module| {
        super::types::as_pairs(module).into_iter().any(|(key, value)| {
            super::types::as_string(key).as_deref() == Some("name")
                && super::types::as_string(value).as_deref() == Some(super::vector::SEARCH_MODULE)
        })
    });
    if !search {
        return None;
    }
    let _: ::redis::RedisResult<()> = ::redis::cmd("FT.DROPINDEX").arg(VECTOR_INDEX).query_async(&mut conn).await;
    let mut pipe = ::redis::pipe();
    for id in 1..=VECTOR_DOCS {
        let n = id as f32;
        let values = [
            (id % 10 + 1) as f32 / 10.0,
            ((id * 3) % 11 + 1) as f32 / 11.0,
            ((id * 7) % 13 + 1) as f32 / 13.0,
            ((id * 9) % 17 + 1) as f32 / 17.0,
        ];
        let blob: Vec<u8> = values.iter().flat_map(|value| value.to_le_bytes()).collect();
        let key = format!("{VECTOR_PREFIX}{id}");
        pipe.cmd("DEL").arg(&key).ignore();
        pipe.cmd("HSET")
            .arg(&key)
            .arg("label")
            .arg(format!("vector {id}"))
            .arg("rank")
            .arg(n)
            .arg("embedding")
            .arg(blob)
            .ignore();
    }
    let _: () = pipe.query_async(&mut conn).await.expect("seed vector documents");
    let _: () = ::redis::cmd("FT.CREATE")
        .arg(VECTOR_INDEX)
        .arg(&[
            "ON",
            "HASH",
            "PREFIX",
            "1",
            VECTOR_PREFIX,
            "SCHEMA",
            "label",
            "TEXT",
            "rank",
            "NUMERIC",
        ])
        .arg(&[
            "embedding",
            "VECTOR",
            "HNSW",
            "8",
            "TYPE",
            "FLOAT32",
            "DIM",
            "4",
            "DISTANCE_METRIC",
            "COSINE",
            "M",
            "8",
        ])
        .query_async(&mut conn)
        .await
        .expect("FT.CREATE");
    // Indexing existing documents runs in the background.
    for _ in 0..50 {
        let info: ::redis::Value = ::redis::cmd("FT.INFO")
            .arg(VECTOR_INDEX)
            .query_async(&mut conn)
            .await
            .expect("FT.INFO");
        let indexed = super::types::as_pairs(info).into_iter().find_map(|(key, value)| {
            (super::types::as_string(key).as_deref() == Some("num_docs")).then(|| super::types::as_i64(&value))?
        });
        if indexed == Some(i64::from(VECTOR_DOCS)) {
            return Some(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("{VECTOR_INDEX} did not index its documents");
}

/// The shared vector suite on a Redis 8 search index (HASH documents, a FLOAT32 HNSW cosine field). Redis 7.4 without the
/// search module skips it (and passes), so CI's Redis 7.4 job stays green.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_vector() {
    if seed_vectors().await.is_none() {
        println!("redis: no search module (Query Engine) on this server; vector suite skipped");
        return;
    }
    let vectors = crate::contract::testing::vector::VectorFixture {
        database: "0",
        table: VECTOR_INDEX,
        column: "embedding",
        dims: 4,
        key_column: super::vector::KEY_COLUMN,
        key: 1,
        indexed: true,
    };
    let Some(run) = crate::contract::testing::vector::suite_vector(&fixture(), &vectors).await else {
        return;
    };
    assert_eq!(run.info["columns"][0]["typeName"], "VECTOR HNSW FLOAT32 DIM 4 COSINE");
    let index = &run.info["indexes"][0];
    assert_eq!(
        (
            index["name"].as_str(),
            index["kind"].as_str(),
            index["metric"].as_str(),
            index["m"].as_u64()
        ),
        (Some(VECTOR_INDEX), Some("hnsw"), Some("cosine"), Some(8))
    );
    let search = &run.search;
    assert_eq!(search["indexUsed"], VECTOR_INDEX, "{search}");
    assert!(search["sql"].as_str().unwrap().contains("KNN 5 @embedding $vec"), "{search}");
    assert!(search["runnableSql"].as_str().unwrap().contains("\\x"), "{search}");
    let columns: Vec<&str> = search["result"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|column| column["name"].as_str().unwrap())
        .collect();
    assert_eq!(columns, ["vec_distance", "__key", "label", "rank"]);
    assert_eq!(search["result"]["rows"][0][1]["v"], format!("{VECTOR_PREFIX}1"), "{search}");

    // Another metric than the field's scans every document; searches take no filter.
    let (manager, handle) = suites::connect(&fixture()).await;
    let args = |metric: &str, filter: Json| {
        json!({ "database": "0", "table": VECTOR_INDEX, "column": "embedding", "k": 3, "metric": metric, "filter": filter,
                "query": { "kind": "literal", "text": "[0.2, 0.4, 0.6, 0.8]" } })
    };
    let scanned = call(&handle, "vector.search", args("euclidean", Json::Null)).await.unwrap();
    assert!(scanned["indexUsed"].is_null(), "{scanned}");
    assert!(
        scanned["plan"]["rows"][0][0]["v"].as_str().unwrap().starts_with("Exact scan"),
        "{scanned}"
    );
    assert!(
        scanned["plan"]["rows"][1][0]["v"]
            .as_str()
            .unwrap()
            .starts_with(&format!("Read all {VECTOR_DOCS} of {VECTOR_DOCS}")),
        "{scanned}"
    );
    // The vector_search command checks the filter with the gateway first (Redis has no filter language), and the
    // operation itself refuses one, so a filter reached through invoke_capability is not spliced into the query either.
    let context = gateway::Context {
        workspace_id: "contract".into(),
        workspace_name: "Contract".into(),
        connection_id: suites::CONNECTION.into(),
        connection_name: suites::CONNECTION.into(),
        safety: gateway::rules::SafetySettings::preset(gateway::rules::Preset::ReadOnly),
    };
    let breakout = "*) | (@label:x";
    for filter in ["@rank:[1 10]", breakout] {
        assert!(
            gateway::check_filter(&manager, &context, suites::CONNECTION, Some(filter)).is_err(),
            "{filter}"
        );
        let refused = gateway::invoke(
            &manager,
            &context,
            gateway::Caller::EngineView,
            "vector.search",
            args("auto", json!(filter)),
            None,
        )
        .await
        .expect_err("a filter");
        assert!(refused.to_string().contains("takes no filter"), "{refused}");
    }
    assert!(gateway::check_filter(&manager, &context, suites::CONNECTION, None).is_ok());
    let unfiltered = gateway::invoke(
        &manager,
        &context,
        gateway::Caller::EngineView,
        "vector.search",
        args("auto", Json::Null),
        None,
    )
    .await
    .unwrap();
    assert_eq!(unfiltered["result"]["rows"].as_array().unwrap().len(), 3, "{unfiltered}");

    // JSON documents: the attribute `$.embedding` and a name with `-` are escaped in the KNN query.
    let url = format!(
        "redis://:{}@{}:{}/0",
        env("PASSWORD", "studio-redis"),
        env("HOST", "127.0.0.1"),
        env("PORT", "6379")
    );
    let mut conn = ::redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let _: ::redis::RedisResult<()> = ::redis::cmd("FT.DROPINDEX").arg(JSON_INDEX).arg("DD").query_async(&mut conn).await;
    let mut pipe = ::redis::pipe();
    for (id, vector) in [(1, "[1, 0]"), (2, "[0, 1]"), (3, "[1, 1]")] {
        pipe.cmd("JSON.SET")
            .arg(format!("{JSON_PREFIX}{id}"))
            .arg("$")
            .arg(format!(r#"{{"embedding": {vector}, "my-vec": {vector}}}"#))
            .ignore();
    }
    // A document whose vector has another length: no index entry, and no vector that decodes.
    pipe.cmd("JSON.SET")
        .arg(format!("{JSON_PREFIX}4"))
        .arg("$")
        .arg(r#"{"embedding": [1, 0, 0], "my-vec": [1, 0, 0]}"#)
        .ignore();
    let _: () = pipe.query_async(&mut conn).await.expect("seed JSON documents");
    let _: () = ::redis::cmd("FT.CREATE")
        .arg(JSON_INDEX)
        .arg(&["ON", "JSON", "PREFIX", "1", JSON_PREFIX, "SCHEMA"])
        .arg(&[
            "$.embedding",
            "VECTOR",
            "FLAT",
            "6",
            "TYPE",
            "FLOAT32",
            "DIM",
            "2",
            "DISTANCE_METRIC",
            "COSINE",
        ])
        .arg(&[
            "$.my-vec",
            "AS",
            "my-vec",
            "VECTOR",
            "FLAT",
            "6",
            "TYPE",
            "FLOAT32",
            "DIM",
            "2",
            "DISTANCE_METRIC",
            "L2",
        ])
        .query_async(&mut conn)
        .await
        .expect("FT.CREATE JSON");
    for _ in 0..50 {
        let info: ::redis::Value = ::redis::cmd("FT.INFO").arg(JSON_INDEX).query_async(&mut conn).await.unwrap();
        let indexed = super::types::as_pairs(info).into_iter().find_map(|(key, value)| {
            (super::types::as_string(key).as_deref() == Some("num_docs")).then(|| super::types::as_i64(&value))?
        });
        if indexed.is_some_and(|docs| docs >= 3) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    for (column, metric) in [("$.embedding", "auto"), ("my-vec", "auto"), ("$.embedding", "euclidean")] {
        let found = call(
            &handle,
            "vector.search",
            json!({ "database": "0", "table": JSON_INDEX, "column": column, "k": 2, "metric": metric, "filter": null,
                    "query": { "kind": "literal", "text": "[1, 0]" } }),
        )
        .await
        .unwrap_or_else(|error| panic!("{column} {metric}: {error}"));
        let rows = found["result"]["rows"].as_array().unwrap();
        assert_eq!(rows[0][1]["v"], format!("{JSON_PREFIX}1"), "{column} {metric}: {found}");
        assert!(
            rows.iter().all(|row| row[1]["v"] != format!("{JSON_PREFIX}4")),
            "{column} {metric}: {found}"
        );
    }
    let fetched = call(
        &handle,
        "vector.fetch",
        json!({ "database": "0", "table": JSON_INDEX, "column": "$.embedding",
                "key": [{ "column": super::vector::KEY_COLUMN, "value": { "t": "text", "v": "4" } }] }),
    )
    .await;
    assert!(fetched.is_err(), "a vector of another length does not decode: {fetched:?}");
    let _: () = ::redis::cmd("FT.DROPINDEX")
        .arg(JSON_INDEX)
        .arg("DD")
        .query_async(&mut conn)
        .await
        .unwrap();

    let other_db = call(&handle, "vector.info", json!({ "database": "14", "table": VECTOR_INDEX }))
        .await
        .unwrap();
    assert_eq!(other_db["supported"], false, "search indexes live in database 0");
    manager.disconnect(suites::CONNECTION).await;
}

/// The assistant's `describe_table` finds a key the cut `list_tables` listing left out. Database 14 holds more keys than
/// the listing shows, and `SCAN` orders them differently on every server start, so the key is chosen from what the listing
/// did not return this time.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis.yml"]
async fn contract_redis_assistant_describes_a_key_the_listing_left_out() {
    use crate::ai::testing::tool;
    let (manager, _handle) = suites::connect(&fixture()).await;
    let listing = tool(&manager, suites::CONNECTION, Some("14"), None, "list_tables", json!({}))
        .await
        .unwrap();
    assert!(
        listing.contains("Note: only the first"),
        "the seed must exceed the listing cap:\n{listing}"
    );
    let listed: Vec<&str> = listing.lines().filter_map(|line| line.split('\t').next()).collect();
    let left_out = (0..PAGE_KEYS)
        .map(|i| format!("contract:page:{i}"))
        .find(|name| !listed.contains(&name.as_str()))
        .expect("a seeded key the listing did not show");
    let described = tool(
        &manager,
        suites::CONNECTION,
        Some("14"),
        None,
        "describe_table",
        json!({ "table": left_out }),
    )
    .await
    .unwrap_or_else(|error| panic!("describe_table {left_out}: {error}"));
    assert!(described.contains("- Type: string"), "{left_out}: {described}");
    manager.disconnect(suites::CONNECTION).await;
}
