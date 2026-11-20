//! Contract tests for Redis Cluster against `dev/compose/redis-cluster.yml` (seed 127.0.0.1:7001, six nodes on
//! 7001-7006, password `studio-redis`; override with `FOSTA_CONTRACT_REDIS_CLUSTER_HOST` / `_PORT` / `_PASSWORD`).
//! Run: `cargo test redis::cluster_contract_tests:: -- --ignored --test-threads=1`.
//!
//! The keys start with `ccontract:` and spread over every shard. Test names avoid the standalone suite's filter so the
//! two CI jobs never run each other's tests.

use std::{sync::Arc, time::Duration};

use serde_json::{json, Value as Json};

use super::adapter::REDIS;
use crate::{
    contract::{
        errors::CONFLICT,
        testing::suites::{self, Fixture},
        CallContext, Capability, Category, CollectSink, ConnectRequest, ConnectSecrets, ConnectionSpec, Endpoint, Engine, Filter, NodeKind,
        ObjectRef, RunOptions, RunRequest, Session, SessionOptions, TreeRequest, Value,
    },
    runtime::Handle,
};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_REDIS_CLUSTER_{name}")).unwrap_or_else(|_| default.to_owned())
}

const PAGE_KEYS: usize = 300;

fn page_keys() -> Vec<String> {
    (0..PAGE_KEYS).map(|i| format!("ccontract:page:{i}")).collect()
}

/// The commands that (re)create the `ccontract:*` keys. DEL and MSET span slots: the cluster client splits them per shard.
fn seed() -> Vec<String> {
    let mut delete: Vec<String> = [
        "ccontract:str",
        "ccontract:json",
        "ccontract:hash",
        "ccontract:list",
        "ccontract:set",
        "ccontract:zset",
        "ccontract:stream",
        "ccontract:edit:str",
        "ccontract:edit:from",
        "ccontract:edit:to",
        "{ccontract}:from",
        "{ccontract}:to",
        "ccontract:del:1",
        "ccontract:del:2",
        "ccontract:del:3",
        "ccontract:console",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let page = page_keys();
    delete.extend(page.iter().cloned());
    let mset = page.iter().map(|key| format!("{key} v")).collect::<Vec<_>>().join(" ");
    vec![
        format!("DEL {}", delete.join(" ")),
        format!("MSET {mset}"),
        "SET ccontract:str hello".into(),
        r#"SET ccontract:json '{"name":"Ada","tags":["a","b"]}'"#.into(),
        "HSET ccontract:hash name Ada role admin".into(),
        "RPUSH ccontract:list a b c".into(),
        "SADD ccontract:set x y z".into(),
        "ZADD ccontract:zset 1 one 2 two 3 three".into(),
        "XADD ccontract:stream 1-1 field one".into(),
        "XADD ccontract:stream 2-1 field two".into(),
    ]
}

fn key_filter(_: i64) -> Filter {
    Filter::None
}

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        engine: "redis".into(),
        host: env("HOST", "127.0.0.1"),
        port: env("PORT", "7001").parse().unwrap_or(7001),
        options: [("topology".to_owned(), "cluster".to_owned())].into_iter().collect(),
        ..ConnectionSpec::default()
    }
}

fn secrets() -> ConnectSecrets {
    ConnectSecrets::password(&env("PASSWORD", "studio-redis"))
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "redis",
        spec: spec(),
        secrets: secrets(),
        table: ObjectRef::new(Some("0"), None, "ccontract:str", NodeKind::KEY),
        child: ObjectRef::new(Some("0"), None, "ccontract:hash", NodeKind::KEY),
        key_column: "",
        name_column: "",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("BLPOP ccontract:never 0"),
        syntax_error: "SET ccontract:k",
        constraint_violation: "HSET ccontract:str field value",
        multi_statement: "SET ccontract:multi 1\nGET ccontract:str",
        insert_probe: "SET ccontract:probe 1",
        count_probe: "GET ccontract:str",
        classify_samples: &[
            ("GET k", Category::Read),
            ("SET k v", Category::Write),
            ("FLUSHALL", Category::Destructive),
            ("CLUSTER FORGET abc", Category::Destructive),
            ("CLUSTER INFO", Category::Read),
        ],
        language: "redis",
        gaps: &[],
    }
}

/// The shared suites that apply to a key-value engine: connect, console queries, and Stop (here without a client id, so
/// the run stops waiting and the next run reopens the connection).
mod cluster_suites {
    use super::*;

    #[test]
    fn classify() {
        suites::suite_classify(&fixture());
    }

    crate::contract_tests!(@live fixture(); suite_connect suite_query suite_cancel);
}

async fn connect_direct(spec: ConnectionSpec) -> crate::error::AppResult<Arc<dyn crate::contract::Connection>> {
    let endpoint = Endpoint {
        host: spec.host.clone(),
        port: spec.port,
    };
    REDIS
        .connect(ConnectRequest {
            spec,
            secrets: secrets(),
            endpoint,
            connect_timeout: Duration::from_secs(5),
        })
        .await
}

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

async fn edit(handle: &Handle, mut edit: Json) -> crate::error::AppResult<Json> {
    edit["db"] = json!(0);
    call(handle, "redis.edit", edit).await
}

fn set(name: &str, value: &str) -> Json {
    json!({ "op": "setString", "key": key(name), "value": key(value), "ttlMs": null, "keepTtl": false, "condition": "always" })
}

async fn value_of(handle: &Handle, name: &str) -> Json {
    call(handle, "redis.readValue", json!({ "db": 0, "key": key(name) }))
        .await
        .unwrap_or_else(|error| panic!("read {name}: {error}"))
}

async fn run_with(
    handle: &Handle,
    session: &Arc<dyn Session>,
    text: &str,
    database: Option<&str>,
    options: RunOptions,
) -> crate::error::AppResult<CollectSink> {
    let mut sink = CollectSink::default();
    let request = RunRequest {
        text: text.to_owned(),
        database: database.map(str::to_owned),
        offset: 0,
    };
    handle.run(Arc::clone(session), request, options, &mut sink).await.map(|_| sink)
}

async fn run(handle: &Handle, session: &Arc<dyn Session>, text: &str) -> CollectSink {
    run_with(handle, session, text, None, RunOptions::default())
        .await
        .unwrap_or_else(|error| panic!("{text}: {error}"))
}

fn first_cell(sink: &CollectSink) -> Value {
    sink.sets
        .last()
        .and_then(|set| set.rows.first())
        .map(|row| row[0].clone())
        .expect("a reply")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-cluster.yml"]
async fn cluster_connects_with_its_flavor_capabilities_and_label() {
    let conn = connect_direct(spec()).await.expect("cluster");
    let info = conn.info();
    assert_eq!(info.flavor.as_deref(), Some("cluster"));
    assert!(info.label.ends_with("· cluster (3 shards)"), "{}", info.label);
    let capabilities = conn.capabilities();
    assert!(!capabilities.contains(Capability::MultipleDatabases));
    assert!(!capabilities.contains(Capability::Vector));
    assert!(capabilities.contains(Capability::Console) && capabilities.contains(Capability::KeyValue));
    conn.ping().await.expect("ping every master");
    // The same node as Standalone is recognised as a cluster node.
    let mut standalone = spec();
    standalone.options.clear();
    let node = connect_direct(standalone).await.expect("one node");
    assert_eq!(node.info().flavor.as_deref(), Some("clusterNode"));
    assert!(node.info().label.ends_with("· cluster node"), "{}", node.info().label);
    // A seed nobody listens on: a connection error, not a hang or an internal error.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut unreachable = spec();
    unreachable.port = free;
    let Err(error) = connect_direct(unreachable).await else {
        panic!("nothing listens on {free}");
    };
    assert_eq!(error.code(), Some("CONNECTION_ERROR"), "{error:?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-cluster.yml"]
async fn cluster_tree_has_database_0_only_and_pages_every_key_once() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let children = |parent, cursor, filter: Option<&str>| {
        let handle = handle.clone();
        let request = TreeRequest {
            parent,
            cursor,
            limit: Some(100),
            filter: filter.map(str::to_owned),
        };
        async move {
            handle
                .call("listed children", |conn| async move {
                    conn.children(&request, &CallContext::default()).await
                })
                .await
                .expect("children")
        }
    };
    let top = children(None, None, None).await;
    assert_eq!(top.nodes.len(), 1);
    assert_eq!(top.nodes[0].label, "db0");
    assert!(top.nodes[0].detail.as_deref().unwrap_or_default().ends_with("keys"));
    let db0 = top.nodes[0].key.clone();
    let mut ids = std::collections::HashSet::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = children(Some(db0.clone()), cursor, Some("ccontract:page:*")).await;
        pages += 1;
        for node in &page.nodes {
            assert!(ids.insert(node.key.id.clone()), "{} came twice", node.key.id);
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
        assert!(cursor.as_deref().unwrap().contains('@'), "{cursor:?}");
        assert!(pages < 30, "the cursor never finishes");
    }
    assert_eq!(ids.len(), PAGE_KEYS);
    assert!(pages >= 2, "{pages} pages");
    let error = call(&handle, "redis.scan", json!({ "db": 1 })).await.unwrap_err();
    assert_eq!(error.to_string(), "Redis Cluster has only database 0.");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-cluster.yml"]
async fn cluster_scan_pages_with_type_ttl_and_memory() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let mut seen = std::collections::HashSet::new();
    let mut cursor = String::new();
    let mut pages = 0;
    loop {
        let page = call(
            &handle,
            "redis.scan",
            json!({ "db": 0, "cursor": cursor, "pattern": "ccontract:page:*", "pageSize": 100, "count": 50, "withMemory": true }),
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
        // DBSIZE is summed over the masters.
        assert!(page["dbSize"].as_u64().unwrap() >= PAGE_KEYS as u64);
        if page["done"].as_bool().unwrap() {
            assert_eq!(page["cursor"], "0");
            break;
        }
        cursor = page["cursor"].as_str().unwrap().to_owned();
        assert!(pages < 50, "the cursor never finishes");
    }
    assert_eq!(seen.len(), PAGE_KEYS, "every key exactly once across {pages} pages");
    assert!(pages >= 3, "100 keys per page needs several pages");
    let typed = call(
        &handle,
        "redis.scan",
        json!({ "db": 0, "pattern": "ccontract:*", "typeFilter": "zset", "pageSize": 1000 }),
    )
    .await
    .unwrap();
    assert_eq!(typed["keys"].as_array().unwrap().len(), 1);
    let error = call(&handle, "redis.scan", json!({ "db": 0, "cursor": "17" })).await.unwrap_err();
    assert!(error.to_string().contains("Start the listing again"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-cluster.yml"]
async fn cluster_values_of_every_type() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let string = value_of(&handle, "ccontract:str").await;
    assert_eq!(text(&string["value"]["data"]["value"]), "hello");
    assert_eq!(string["info"]["length"], 5);
    assert!(string["info"]["memoryBytes"].as_u64().unwrap() > 0);
    let json_string = value_of(&handle, "ccontract:json").await;
    assert_eq!(json_string["value"]["data"]["json"]["tags"][1], "b");
    let hash = value_of(&handle, "ccontract:hash").await;
    assert_eq!(hash["value"]["data"]["total"], 2);
    let list = value_of(&handle, "ccontract:list").await;
    assert_eq!(list["value"]["data"]["items"].as_array().unwrap().len(), 3);
    let set = value_of(&handle, "ccontract:set").await;
    assert_eq!(set["value"]["data"]["total"], 3);
    let zset = value_of(&handle, "ccontract:zset").await;
    assert_eq!(text(&zset["value"]["data"]["items"][2]["member"]), "three");
    let stream = value_of(&handle, "ccontract:stream").await;
    assert_eq!(stream["value"]["data"]["info"]["lastId"], "2-1");
    assert_eq!(value_of(&handle, "ccontract:nothing").await["value"]["type"], "none");
    let info = call(&handle, "redis.keyInfo", json!({ "db": 0, "key": key("ccontract:hash") }))
        .await
        .unwrap();
    assert_eq!((info["keyType"].as_str(), info["length"].as_u64()), (Some("hash"), Some(2)));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-cluster.yml"]
async fn cluster_edits_rename_within_a_slot_and_delete_across_slots() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    edit(&handle, set("{ccontract}:from", "v")).await.unwrap();
    let renamed = edit(
        &handle,
        json!({ "op": "rename", "key": key("{ccontract}:from"), "newKey": key("{ccontract}:to"), "overwrite": true }),
    )
    .await
    .unwrap();
    assert_eq!(renamed["applied"], true);
    assert_eq!(text(&value_of(&handle, "{ccontract}:to").await["value"]["data"]["value"]), "v");
    // Two keys in different slots: the server refuses, the message says why.
    edit(&handle, set("ccontract:edit:from", "v")).await.unwrap();
    let error = edit(
        &handle,
        json!({ "op": "rename", "key": key("ccontract:edit:from"), "newKey": key("ccontract:edit:to"), "overwrite": true }),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("hash slots"), "{error}");
    for name in ["ccontract:del:1", "ccontract:del:2", "ccontract:del:3"] {
        edit(&handle, set(name, "v")).await.unwrap();
    }
    let deleted = edit(
        &handle,
        json!({ "op": "delete", "keys": [key("ccontract:del:1"), key("ccontract:del:2"), key("ccontract:del:3")] }),
    )
    .await
    .unwrap();
    assert_eq!(deleted["count"], 3, "{deleted}");
    assert_eq!(value_of(&handle, "ccontract:del:2").await["value"]["type"], "none");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-cluster.yml"]
async fn cluster_guarded_edits_detect_conflicts() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let name = "ccontract:edit:str";
    let guarded = |value: &str, expect: Json| {
        let mut edit = set(name, value);
        edit["expect"] = expect;
        edit
    };
    edit(&handle, set(name, "one")).await.unwrap();
    edit(&handle, guarded("two", json!({ "kind": "string", "value": key("one") })))
        .await
        .unwrap();
    edit(&handle, set(name, "three")).await.unwrap();
    let error = edit(&handle, guarded("lost", json!({ "kind": "string", "value": key("two") })))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(CONFLICT), "{error:?}");
    assert_eq!(text(&value_of(&handle, name).await["value"]["data"]["value"]), "three");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-cluster.yml"]
async fn cluster_console_routes_commands_and_refuses_single_node_ones() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let session = handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("session")
        .session;
    run(&handle, &session, "SET ccontract:console routed").await;
    assert_eq!(
        first_cell(&run(&handle, &session, "GET ccontract:console").await),
        Value::Text("routed".into())
    );
    match first_cell(&run(&handle, &session, "DBSIZE").await) {
        Value::Int(count) => assert!(count >= PAGE_KEYS as i64, "{count}"),
        other => panic!("DBSIZE: {other:?}"),
    }
    // INFO goes to every master: one row per node.
    let info = run(&handle, &session, "INFO server").await;
    assert_eq!(info.sets.last().unwrap().rows.len(), 3, "one INFO per master");
    for refused in ["MULTI", "EXEC", "WATCH ccontract:str", "SCAN 0"] {
        let error = run_with(&handle, &session, refused, None, RunOptions::default()).await.unwrap_err();
        assert!(error.to_string().contains("Redis Cluster console"), "{refused}: {error}");
    }
    let read_only = RunOptions {
        read_only: true,
        ..RunOptions::default()
    };
    let error = run_with(&handle, &session, "SET ccontract:console no", None, read_only)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only"), "{error}");
    assert!(run_with(&handle, &session, "PING", Some("1"), RunOptions::default()).await.is_err());
    assert_eq!(
        first_cell(&run(&handle, &session, "GET ccontract:console").await),
        Value::Text("routed".into())
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-cluster.yml"]
async fn cluster_info_shows_the_shards_one_node_and_the_merged_slowlog() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    let info = call(&handle, "redis.info", Json::Null).await.unwrap();
    assert_eq!(info["topology"], "cluster");
    assert_eq!(info["mode"], "cluster");
    let cluster = &info["cluster"];
    assert_eq!(cluster["state"], "ok");
    assert_eq!(cluster["slotsAssigned"], 16384);
    let shards = cluster["shards"].as_array().unwrap();
    assert_eq!(shards.len(), 3);
    let slots: u64 = shards.iter().map(|shard| shard["slotCount"].as_u64().unwrap()).sum();
    assert_eq!(slots, 16384);
    for shard in shards {
        assert_eq!(shard["replicas"].as_array().unwrap().len(), 1, "{shard}");
        assert_eq!(shard["master"]["role"], "master");
    }
    let keys: u64 = shards.iter().map(|shard| shard["keys"].as_u64().unwrap_or(0)).sum();
    assert!(keys >= PAGE_KEYS as u64);
    assert_eq!(info["keyspace"][0]["keys"].as_u64(), Some(keys));
    // One replica's own INFO.
    let replica = shards[0]["replicas"][0]["addr"].as_str().unwrap().to_owned();
    let node = call(&handle, "redis.info", json!({ "node": replica })).await.unwrap();
    assert_eq!(node["node"], replica.as_str());
    assert_eq!(node["role"], "slave");
    assert!(node["cluster"].is_null());
    let missing = call(&handle, "redis.info", json!({ "node": "127.0.0.1:1" })).await.unwrap_err();
    assert!(missing.to_string().contains("not a node"), "{missing}");
    // Log every command for a moment, on every node, then read the merged log.
    let session = handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("session")
        .session;
    run(&handle, &session, "CONFIG SET slowlog-log-slower-than 0").await;
    for i in 0..30 {
        run(&handle, &session, &format!("GET ccontract:page:{i}")).await;
    }
    run(&handle, &session, "CONFIG SET slowlog-log-slower-than 10000").await;
    let slow = call(&handle, "redis.slowlog", json!({ "count": 100 })).await.unwrap();
    let entries = slow["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    let nodes: std::collections::HashSet<&str> = entries.iter().filter_map(|entry| entry["node"].as_str()).collect();
    assert!(nodes.len() >= 2, "entries from several nodes: {nodes:?}");
    let times: Vec<i64> = entries.iter().map(|entry| entry["timestamp"].as_i64().unwrap()).collect();
    assert!(times.windows(2).all(|pair| pair[0] >= pair[1]), "newest first");
    assert!(slow["length"].as_i64().unwrap() >= entries.len() as i64);
}
