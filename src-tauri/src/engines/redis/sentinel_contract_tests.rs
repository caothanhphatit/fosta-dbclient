//! Contract tests for Redis Sentinel against `dev/compose/redis-sentinel.yml` (sentinels 127.0.0.1:26390-26392 with
//! password `studio-sentinel`, master name `fosta`, data nodes 6390/6391 with password `studio-redis`; override with
//! `FOSTA_CONTRACT_REDIS_SENTINEL_HOST` / `_PORT` / `_PASSWORD` / `_SENTINEL_PASSWORD` / `_MASTER`).
//! Run: `cargo test redis::sentinel_contract_tests:: -- --ignored --test-threads=1`.
//!
//! The suite works in database 14 on keys that start with `scontract:`. It never assumes which data node is the master: a
//! previous run failed over. The failover test is named so that it sorts (and runs) last, and leaves the setup settled.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use redis::aio::MultiplexedConnection;
use serde_json::{json, Value as Json};

use super::{adapter::REDIS, connection::HostPort, sentinel::master_address};
use crate::{
    contract::{
        errors::CONFLICT,
        testing::suites::{self, Fixture},
        CallContext, Capability, Category, CollectSink, ConnectRequest, ConnectSecrets, ConnectionSpec, Endpoint, Engine, Filter, NodeKind,
        ObjectRef, RunOptions, RunRequest, SecretSlot, Session, SessionOptions, TreeRequest, Value,
    },
    runtime::Handle,
};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_REDIS_SENTINEL_{name}")).unwrap_or_else(|_| default.to_owned())
}

fn host() -> String {
    env("HOST", "127.0.0.1")
}

fn port() -> u16 {
    env("PORT", "26390").parse().unwrap_or(26390)
}

fn master_name() -> String {
    env("MASTER", "fosta")
}

fn data_password() -> String {
    env("PASSWORD", "studio-redis")
}

fn sentinel_password() -> String {
    env("SENTINEL_PASSWORD", "studio-sentinel")
}

/// The three sentinels of the compose file: the profile's, then the two in `options.nodes`.
fn sentinels() -> Vec<HostPort> {
    (0..3).map(|i| HostPort::new(host(), port() + i)).collect()
}

const PAGE_KEYS: usize = 120;

/// The commands that (re)create the `scontract:*` keys of database 14.
fn seed() -> Vec<String> {
    let page: Vec<String> = (0..PAGE_KEYS).map(|i| format!("scontract:page:{i}")).collect();
    let mut delete: Vec<String> = [
        "scontract:str",
        "scontract:hash",
        "scontract:list",
        "scontract:zset",
        "scontract:edit:str",
        "scontract:console",
        "scontract:failover",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    delete.extend(page.iter().cloned());
    let mset = page.iter().map(|key| format!("{key} v")).collect::<Vec<_>>().join(" ");
    vec![
        format!("DEL {}", delete.join(" ")),
        format!("MSET {mset}"),
        "SET scontract:str hello".into(),
        "HSET scontract:hash name Ada role admin".into(),
        "RPUSH scontract:list a b c".into(),
        "ZADD scontract:zset 1 one 2 two".into(),
    ]
}

fn key_filter(_: i64) -> Filter {
    Filter::None
}

fn spec_with(options: &[(&str, &str)]) -> ConnectionSpec {
    let mut all: std::collections::HashMap<String, String> = [
        ("topology", "sentinel".to_owned()),
        ("masterName", master_name()),
        ("nodes", format!("{h}:{}, {h}:{}", port() + 1, port() + 2, h = host())),
        ("db", "14".to_owned()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    for (key, value) in options {
        all.insert((*key).to_owned(), (*value).to_owned());
    }
    ConnectionSpec {
        engine: "redis".into(),
        host: host(),
        port: port(),
        options: all,
        ..ConnectionSpec::default()
    }
}

fn spec() -> ConnectionSpec {
    spec_with(&[])
}

fn secrets_with(data: &str, sentinel: &str) -> ConnectSecrets {
    let mut secrets = ConnectSecrets::password(data);
    if !sentinel.is_empty() {
        secrets.values.insert(SecretSlot::SecondaryPassword, sentinel.to_owned());
    }
    secrets
}

fn secrets() -> ConnectSecrets {
    secrets_with(&data_password(), &sentinel_password())
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "redis",
        spec: spec(),
        secrets: secrets(),
        table: ObjectRef::new(Some("14"), None, "scontract:str", NodeKind::KEY),
        child: ObjectRef::new(Some("14"), None, "scontract:hash", NodeKind::KEY),
        key_column: "",
        name_column: "",
        seed: seed(),
        seed_database: None,
        key_filter,
        sleep_statement: Some("BLPOP scontract:never 30"),
        syntax_error: "SET scontract:k",
        constraint_violation: "HSET scontract:str field value",
        multi_statement: "SET scontract:multi 1\nGET scontract:str",
        insert_probe: "SET scontract:probe 1",
        count_probe: "GET scontract:str",
        classify_samples: &[
            ("GET k", Category::Read),
            ("SET k v", Category::Write),
            ("FLUSHALL", Category::Destructive),
            ("SENTINEL MASTERS", Category::Read),
        ],
        language: "redis",
        gaps: &[],
    }
}

/// The shared suites that apply to a key-value engine: connect, console queries, and Stop (CLIENT UNBLOCK on the master).
mod sentinel_suites {
    use super::*;

    #[test]
    fn classify() {
        suites::suite_classify(&fixture());
    }

    crate::contract_tests!(@live fixture(); suite_connect suite_query suite_cancel);
}

// --- Helpers -------------------------------------------------------------------------------------------------------------

async fn connect_with(spec: ConnectionSpec, secrets: ConnectSecrets) -> crate::error::AppResult<Arc<dyn crate::contract::Connection>> {
    let endpoint = Endpoint {
        host: spec.host.clone(),
        port: spec.port,
    };
    REDIS
        .connect(ConnectRequest {
            spec,
            secrets,
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

fn set(name: &str, value: &str) -> Json {
    json!({ "op": "setString", "key": key(name), "value": key(value), "ttlMs": null, "keepTtl": false, "condition": "always", "db": 14 })
}

async fn read_text(handle: &Handle, db: i64, name: &str) -> crate::error::AppResult<String> {
    let value = call(handle, "redis.readValue", json!({ "db": db, "key": key(name) })).await?;
    Ok(text(&value["value"]["data"]["value"]).to_owned())
}

async fn open_session(handle: &Handle) -> Arc<dyn Session> {
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
    handle
        .run(Arc::clone(session), request, RunOptions::default(), &mut sink)
        .await
        .map(|_| sink)
}

fn first_cell(sink: &CollectSink) -> Value {
    sink.sets
        .last()
        .and_then(|set| set.rows.first())
        .map(|row| row[0].clone())
        .expect("a reply")
}

/// The role in a console's `ROLE` reply (an array: one row per element, the value in the second column).
fn role_cell(sink: &CollectSink) -> Value {
    sink.sets
        .last()
        .and_then(|set| set.rows.first())
        .map(|row| row[1].clone())
        .expect("a ROLE reply")
}

/// A direct connection (outside the engine) to a sentinel or a data node, for the test's own checks.
async fn direct(address: &HostPort, password: &str) -> MultiplexedConnection {
    let url = format!("redis://:{password}@{address}/");
    redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap_or_else(|error| panic!("{address}: {error}"))
}

async fn ask(conn: &mut MultiplexedConnection, args: &[&str]) -> redis::RedisResult<redis::Value> {
    let mut cmd = redis::cmd(args[0]);
    for arg in &args[1..] {
        cmd.arg(*arg);
    }
    cmd.query_async(conn).await
}

/// `CLIENT ID` of a new direct connection to a data node (each call takes the node's next id).
async fn client_id(node: &HostPort) -> i64 {
    let mut conn = direct(node, &data_password()).await;
    let reply = ask(&mut conn, &["CLIENT", "ID"]).await;
    reply.ok().as_ref().and_then(super::types::as_i64).unwrap_or(0)
}

/// The master `sentinel` names now.
async fn named_master(sentinel: &HostPort) -> Option<HostPort> {
    let mut conn = direct(sentinel, &sentinel_password()).await;
    master_address(
        ask(&mut conn, &["SENTINEL", "GET-MASTER-ADDR-BY-NAME", &master_name()])
            .await
            .ok()?,
    )
}

/// `ROLE` of a data node: `master` or `slave`, with the replica's master and link state.
async fn role(node: &HostPort) -> Vec<String> {
    let mut conn = direct(node, &data_password()).await;
    let reply = ask(&mut conn, &["ROLE"]).await.unwrap_or(redis::Value::Nil);
    super::types::as_items(reply)
        .into_iter()
        .map(|item| match item {
            redis::Value::Int(number) => number.to_string(),
            other => super::types::as_string(other).unwrap_or_default(),
        })
        .collect()
}

/// `sentinel` lists `replica` as a replica of the master, not down nor disconnected, with its link up.
async fn sentinel_sees_replica_up(sentinel: &HostPort, replica: &HostPort) -> bool {
    let mut conn = direct(sentinel, &sentinel_password()).await;
    let Ok(replicas) = ask(&mut conn, &["SENTINEL", "REPLICAS", &master_name()]).await else {
        return false;
    };
    super::types::as_items(replicas).into_iter().any(|entry| {
        let fields: std::collections::HashMap<String, String> = super::types::as_pairs(entry)
            .into_iter()
            .filter_map(|(key, value)| Some((super::types::as_string(key)?, super::types::as_string(value)?)))
            .collect();
        let field = |name: &str| fields.get(name).map(String::as_str).unwrap_or_default();
        field("port") == replica.port.to_string() && field("flags") == "slave" && field("master-link-status") == "ok"
    })
}

/// Waits until the setup is settled: every sentinel names the same master, which says `master`; the other data node is
/// its connected replica; and the sentinels see that replica up. Returns the master and the replica.
async fn wait_settled(within: Duration) -> (HostPort, HostPort) {
    let deadline = Instant::now() + within;
    let mut last = String::new();
    loop {
        let mut named = Vec::new();
        for sentinel in sentinels() {
            named.push(named_master(&sentinel).await);
        }
        if let Some(Some(master)) = named.first().cloned() {
            if named.iter().all(|other| other.as_ref() == Some(&master)) {
                let replica = if master.port == 6390 {
                    HostPort::new(master.host.clone(), 6391)
                } else {
                    HostPort::new(master.host.clone(), 6390)
                };
                let master_role = role(&master).await;
                let replica_role = role(&replica).await;
                let mut up = true;
                for sentinel in sentinels() {
                    up &= sentinel_sees_replica_up(&sentinel, &replica).await;
                }
                if master_role.first().map(String::as_str) == Some("master")
                    && replica_role.first().map(String::as_str) == Some("slave")
                    && replica_role.get(2).map(String::as_str) == Some(&master.port.to_string())
                    && replica_role.get(3).map(String::as_str) == Some("connected")
                    && up
                {
                    return (master, replica);
                }
                last = format!("master {master} role {master_role:?}, replica {replica} role {replica_role:?}, sentinels see it up: {up}");
            } else {
                last = format!("the sentinels disagree: {named:?}");
            }
        }
        assert!(Instant::now() < deadline, "the Sentinel setup did not settle: {last}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Asks a sentinel to fail over (retrying while a previous failover or the replica's sync is still going on) and waits
/// until the sentinels name another master than `from`.
async fn fail_over(from: &HostPort, within: Duration) -> HostPort {
    let deadline = Instant::now() + within;
    let sentinel = sentinels()[0].clone();
    loop {
        let mut conn = direct(&sentinel, &sentinel_password()).await;
        match ask(&mut conn, &["SENTINEL", "FAILOVER", &master_name()]).await {
            Ok(_) => break,
            Err(error) => assert!(Instant::now() < deadline, "SENTINEL FAILOVER kept failing: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    loop {
        if let Some(master) = named_master(&sentinel).await.filter(|master| master != from) {
            return master;
        }
        assert!(Instant::now() < deadline, "the sentinels still name {from} after the failover");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// --- Tests ---------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-sentinel.yml"]
async fn sentinel_connects_to_the_master_with_its_flavor_label_and_overview() {
    let (master, replica) = wait_settled(Duration::from_secs(60)).await;
    let conn = connect_with(spec(), secrets()).await.expect("sentinel");
    let info = conn.info();
    assert_eq!(info.flavor.as_deref(), Some("sentinel"));
    assert!(info.label.ends_with(&format!("· sentinel {}", master_name())), "{}", info.label);
    let capabilities = conn.capabilities();
    assert!(capabilities.contains(Capability::MultipleDatabases));
    assert!(capabilities.contains(Capability::Console) && capabilities.contains(Capability::KeyValue));
    conn.ping().await.expect("ping the master");
    let overview = conn.invoke("redis.info", Json::Null, &CallContext::default()).await.unwrap();
    assert_eq!(overview["topology"], "sentinel");
    assert_eq!(overview["role"], "master");
    assert_eq!(overview["node"], master.to_string());
    assert_eq!(overview["currentDb"], 14);
    let sentinel = &overview["sentinel"];
    assert_eq!(sentinel["masterName"], master_name());
    assert_eq!(sentinel["master"], master.to_string());
    assert_eq!(sentinel["quorum"], 2);
    let replicas = sentinel["replicas"].as_array().unwrap();
    assert_eq!(replicas.len(), 1, "{sentinel}");
    assert_eq!(replicas[0]["address"], replica.to_string());
    assert_eq!(replicas[0]["link"], "ok");
    let watching = sentinel["sentinels"].as_array().unwrap();
    assert_eq!(watching.len(), 3, "the answering sentinel and the two others: {sentinel}");
    // The node's own INFO: only this server.
    let one = conn
        .invoke("redis.info", json!({ "node": master.to_string() }), &CallContext::default())
        .await
        .unwrap();
    assert_eq!(one["node"], master.to_string());
    assert!(conn
        .invoke("redis.info", json!({ "node": replica.to_string() }), &CallContext::default())
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-sentinel.yml"]
async fn sentinel_skips_a_sentinel_that_is_down() {
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut spec = spec();
    spec.port = free;
    let conn = connect_with(spec, secrets()).await.expect("the other sentinels answer");
    assert_eq!(conn.info().flavor.as_deref(), Some("sentinel"));
    // No sentinel at all: a connection error, not a hang.
    let mut none = spec_with(&[("nodes", "")]);
    none.port = free;
    let Err(error) = connect_with(none, secrets()).await else {
        panic!("nothing listens on {free}");
    };
    assert_eq!(error.code(), Some("CONNECTION_ERROR"), "{error:?}");
    assert!(error.to_string().contains("sentinel"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-sentinel.yml"]
async fn sentinel_explains_wrong_names_and_passwords() {
    let unknown = connect_with(spec_with(&[("masterName", "no-such-master")]), secrets())
        .await
        .err()
        .expect("unknown master");
    assert!(
        unknown.to_string().contains("do not monitor a master named `no-such-master`"),
        "{unknown}"
    );
    let missing = connect_with(spec_with(&[("masterName", "")]), secrets())
        .await
        .err()
        .expect("no master name");
    assert!(missing.to_string().contains("master name"), "{missing}");
    // A wrong sentinel password.
    let wrong = connect_with(spec(), secrets_with(&data_password(), "nope"))
        .await
        .err()
        .expect("wrong sentinel password");
    assert_eq!(wrong.code(), Some("CONNECTION_ERROR"));
    assert!(wrong.to_string().contains("sentinels rejected"), "{wrong}");
    // No sentinel password: the sentinels ask for one, and the data password (different here) does not open them.
    let fallback = connect_with(spec(), secrets_with(&data_password(), ""))
        .await
        .err()
        .expect("fallback refused");
    assert!(fallback.to_string().contains("sentinels rejected"), "{fallback}");
    // No sentinel password and the sentinels' password as the data password: the fallback opens the sentinels, and the
    // master then refuses the login, which is reported as the data login.
    let data = connect_with(spec(), secrets_with(&sentinel_password(), ""))
        .await
        .err()
        .expect("data login refused");
    assert_eq!(data.code(), Some("CONNECTION_ERROR"));
    assert!(!data.to_string().contains("sentinels rejected"), "{data}");
    assert!(data.to_string().contains("password"), "{data}");
    // A wrong data password with the right sentinel password.
    let login = connect_with(spec(), secrets_with("nope", &sentinel_password()))
        .await
        .err()
        .expect("wrong data password");
    assert_eq!(login.code(), Some("CONNECTION_ERROR"));
    assert!(login.to_string().contains("rejected the user name or password"), "{login}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-sentinel.yml"]
async fn sentinel_browses_and_edits_keys_in_several_databases() {
    let (_manager, handle) = suites::connect(&fixture()).await;
    // The tree pages the keys of database 14 (the profile's).
    let top = handle
        .call("listed children", |conn| async move {
            conn.children(&TreeRequest::default(), &CallContext::default()).await
        })
        .await
        .expect("databases");
    assert!(top.nodes.len() >= 16, "every database: {}", top.nodes.len());
    let db14 = top.nodes.iter().find(|node| node.label == "db14").expect("db14").key.clone();
    let mut ids = std::collections::HashSet::new();
    let mut cursor = None;
    loop {
        let request = TreeRequest {
            parent: Some(db14.clone()),
            cursor,
            limit: Some(50),
            filter: Some("scontract:page:*".into()),
        };
        let page = handle
            .call("listed children", |conn| async move {
                conn.children(&request, &CallContext::default()).await
            })
            .await
            .expect("children");
        for node in &page.nodes {
            assert!(ids.insert(node.key.id.clone()), "{} came twice", node.key.id);
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), PAGE_KEYS);
    let hash = call(&handle, "redis.readValue", json!({ "db": 14, "key": key("scontract:hash") }))
        .await
        .unwrap();
    assert_eq!(hash["info"]["keyType"], "hash", "{hash}");
    // Edits, and a guarded edit against a stale value.
    let name = "scontract:edit:str";
    call(&handle, "redis.edit", set(name, "one")).await.unwrap();
    let mut guarded = set(name, "lost");
    guarded["expect"] = json!({ "kind": "string", "value": key("zero") });
    let error = call(&handle, "redis.edit", guarded).await.unwrap_err();
    assert_eq!(error.code(), Some(CONFLICT), "{error:?}");
    assert_eq!(read_text(&handle, 14, name).await.unwrap(), "one");
    // Another database gets an engine of its own, resolved through the sentinels too.
    let mut other = set(name, "thirteen");
    other["db"] = json!(13);
    call(&handle, "redis.edit", other).await.unwrap();
    assert_eq!(read_text(&handle, 13, name).await.unwrap(), "thirteen");
    assert_eq!(read_text(&handle, 14, name).await.unwrap(), "one");
    // The console runs on the master too.
    let session = open_session(&handle).await;
    run(&handle, &session, "SET scontract:console here").await.unwrap();
    assert_eq!(
        first_cell(&run(&handle, &session, "GET scontract:console").await.unwrap()),
        Value::Text("here".into())
    );
    assert_eq!(
        role_cell(&run(&handle, &session, "ROLE").await.unwrap()),
        Value::Text("master".into())
    );
}

/// Runs last (the name sorts after the others): fails over with `SENTINEL FAILOVER`, then checks that the open connection
/// follows the new master for reads and writes, and that a console reconnects. Fails back afterwards when it can, and
/// leaves the setup settled for the next run either way.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs dev/compose/redis-sentinel.yml"]
async fn z_failover_moves_the_master_and_the_open_connection_follows() {
    let (before, promoted) = wait_settled(Duration::from_secs(60)).await;
    let (_manager, handle) = suites::connect(&fixture()).await;
    let session = open_session(&handle).await;
    // A console left on the old master: Stop after the failover must not reach the client with its id on the new one.
    // Client ids count up per server; push the master's past the replica's (promoted below) so that the new master can
    // still hand out the stale console's id afterwards.
    let replica_id = client_id(&promoted).await;
    for _ in 0..5000 {
        if client_id(&before).await > replica_id + 200 {
            break;
        }
    }
    let stale = handle
        .call("opened a session", |conn| async move {
            conn.open_session(&SessionOptions::default(), &CallContext::default()).await
        })
        .await
        .expect("session");
    let stale_id = match first_cell(&run(&handle, &stale.session, "CLIENT ID").await.unwrap()) {
        Value::Int(id) => id,
        other => panic!("CLIENT ID: {other:?}"),
    };
    call(&handle, "redis.edit", set("scontract:failover", "before")).await.unwrap();
    assert_eq!(read_text(&handle, 14, "scontract:failover").await.unwrap(), "before");
    // Wait for the replica to have the value, so the promoted node holds it.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (_, replica) = wait_settled(Duration::from_secs(30)).await;
    loop {
        let mut conn = direct(&replica, &data_password()).await;
        let value = ask(&mut conn, &["SELECT", "14"])
            .await
            .and(ask(&mut conn, &["GET", "scontract:failover"]).await);
        if value.ok().and_then(super::types::as_string).as_deref() == Some("before") {
            break;
        }
        assert!(Instant::now() < deadline, "the replica never got the value");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let after = fail_over(&before, Duration::from_secs(30)).await;
    assert_ne!(after, before);

    // Reads through the connection opened before the failover: a dropped connection may fail one call, then the engine
    // is on the new master.
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut failures = Vec::new();
    loop {
        match read_text(&handle, 14, "scontract:failover").await {
            Ok(value) if value == "before" => break,
            Ok(value) => failures.push(format!("read {value:?}")),
            Err(error) => failures.push(error.to_string()),
        }
        assert!(Instant::now() < deadline, "reads never recovered: {failures:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    eprintln!("calls that failed while the master moved: {failures:?}");
    // Writes land on the new master (until the old one is demoted it may still accept them, so check the new one).
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let value = format!("after-{attempt}");
        let written = call(&handle, "redis.edit", set("scontract:failover", &value)).await;
        let mut conn = direct(&after, &data_password()).await;
        let on_new = ask(&mut conn, &["SELECT", "14"])
            .await
            .and(ask(&mut conn, &["GET", "scontract:failover"]).await);
        if let Err(error) = &written {
            eprintln!("write {attempt}: {error}");
        }
        if written.is_ok() && on_new.ok().and_then(super::types::as_string).as_deref() == Some(value.as_str()) {
            break;
        }
        assert!(Instant::now() < deadline, "writes never reached the new master {after}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let info = call(&handle, "redis.info", Json::Null).await.unwrap();
    assert_eq!(info["node"], after.to_string());
    assert_eq!(info["role"], "master");
    // The engine's shared connection is on the new master now. Give a connection there the stale console's client id
    // (ids count up per server), then Stop the stale console: the new master's client must survive.
    let mut victim = None;
    for _ in 0..5000 {
        let mut conn = direct(&after, &data_password()).await;
        let id = ask(&mut conn, &["CLIENT", "ID"])
            .await
            .ok()
            .as_ref()
            .and_then(super::types::as_i64)
            .unwrap_or(0);
        if id == stale_id {
            victim = Some(conn);
            break;
        }
        if id > stale_id {
            break;
        }
    }
    let _ = stale.canceller.expect("a canceller").cancel().await;
    match victim {
        Some(mut victim) => assert!(
            ask(&mut victim, &["PING"]).await.is_ok(),
            "Stop killed client {stale_id} of the new master"
        ),
        None => eprintln!("the new master is past client id {stale_id}; the stale Stop check did not apply"),
    }
    drop(stale.session);
    // The console opened before the failover reconnects (one run may fail) and lands on the new master.
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut outcomes = Vec::new();
    loop {
        match run(&handle, &session, "ROLE").await {
            Ok(sink) if role_cell(&sink) == Value::Text("master".into()) => break,
            Ok(sink) => outcomes.push(format!("{:?}", role_cell(&sink))),
            Err(error) => outcomes.push(error.to_string()),
        }
        assert!(Instant::now() < deadline, "the console never reached the new master: {outcomes:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    drop(session);
    drop(handle);

    // Fail back (best effort), then leave the setup settled for whoever runs next.
    let (current, _) = wait_settled(Duration::from_secs(60)).await;
    if current != before {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut conn = direct(&sentinels()[0], &sentinel_password()).await;
        while ask(&mut conn, &["SENTINEL", "FAILOVER", &master_name()]).await.is_err() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    wait_settled(Duration::from_secs(60)).await;
}
