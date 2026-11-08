//! Live tests against `dev/compose/{redis,elasticsearch,opensearch}.yml` (Redis on 127.0.0.1:6379, password `studio-redis`).
//! Run with `cargo test live_engine_ -- --ignored --test-threads=1`. Each test writes its own keys under `fosta-test:`
//! so it does not depend on the seed script, and the seed keys (users:*, sessions:*, ...) are checked when present.

use std::time::Duration;

use super::{
    classify::CommandClass,
    connection::{RedisConnectOptions, RedisEngine},
    console::RedisReply,
    edit::{ListEnd, RedisEdit, SetCondition},
    scan::ScanRequest,
    tree::PrefixTree,
    types::{Blob, RedisType},
    values::{FieldValue, ScoredMember, StreamRangeRequest, StringFormat, ZsetRange},
};

fn options(resp3: bool) -> RedisConnectOptions {
    RedisConnectOptions {
        host: std::env::var("FOSTA_REDIS_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: Some(6379),
        password: Some("studio-redis".into()),
        db: 0,
        resp3,
        client_name: Some("fosta-live-test".into()),
        ..Default::default()
    }
}

async fn engine(resp3: bool) -> RedisEngine {
    RedisEngine::connect(options(resp3))
        .await
        .expect("Redis from dev/compose/{redis,elasticsearch,opensearch}.yml must be running")
}

fn key(name: &str) -> Blob {
    Blob::text(format!("fosta-test:{name}"))
}

async fn reset(engine: &RedisEngine, names: &[&str]) {
    let keys = names.iter().map(|name| key(name)).collect();
    engine.apply_edit(&RedisEdit::Delete { keys }).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn live_engine_redis_connect_and_info() {
    let engine = engine(false).await;
    engine.ping().await.unwrap();
    let info = engine.server_info().await.unwrap();
    assert!(!info.version.is_empty());
    assert_eq!(info.mode, "standalone");
    assert!(info.used_memory.unwrap_or(0) > 0);
    assert!(info.sections.contains_key("server"));
    assert!(engine.has_json(), "redis:8 ships RedisJSON: {:?}", engine.modules());

    let mut wrong = options(false);
    wrong.password = Some("nope".into());
    let error = RedisEngine::connect(wrong)
        .await
        .err()
        .expect("a wrong password must fail")
        .to_string();
    assert!(error.contains("rejected") || error.to_lowercase().contains("wrongpass"), "{error}");
}

#[tokio::test]
#[ignore]
async fn live_engine_redis_scan_and_prefix_tree() {
    let engine = engine(false).await;
    let names: Vec<String> = (0..300)
        .map(|n| format!("scan:{}:{n}", if n % 2 == 0 { "even" } else { "odd" }))
        .collect();
    let mut pipe = redis::pipe();
    for name in &names {
        pipe.cmd("SET").arg(key(name)).arg(1).ignore();
    }
    pipe.cmd("HSET").arg(key("scan:hash")).arg("f").arg("v").ignore();
    engine.pipeline(&mut pipe).await.unwrap();

    let mut tree = PrefixTree::new(":");
    let mut request = ScanRequest {
        pattern: Some("fosta-test:scan:*".into()),
        count: Some(100),
        page_size: Some(120),
        with_meta: true,
        ..Default::default()
    };
    let mut pages = 0;
    loop {
        let page = engine.scan_keys(&request).await.unwrap();
        pages += 1;
        tree.extend(page.keys);
        if page.done {
            break;
        }
        request.cursor = page.cursor;
    }
    assert!(pages >= 2, "120-key pages over 301 keys");
    assert_eq!(tree.len(), 301);
    let scan = tree.children("fosta-test:scan:");
    let folders: Vec<_> = scan
        .iter()
        .filter(|entry| entry.has_children)
        .map(|entry| (entry.name.as_str(), entry.key_count))
        .collect();
    assert_eq!(folders, vec![("even", 150), ("odd", 150)]);
    assert!(scan
        .iter()
        .any(|entry| entry.name == "hash" && entry.key_type == Some(RedisType::Hash)));

    let hashes = engine
        .scan_keys(&ScanRequest {
            pattern: Some("fosta-test:scan:*".into()),
            type_filter: Some(RedisType::Hash),
            page_size: Some(10_000),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(hashes.done);
    assert_eq!(hashes.keys.len(), 1);

    let info = engine.key_info(&key("scan:hash")).await.unwrap();
    assert_eq!(info.key_type, RedisType::Hash);
    assert_eq!(info.length, Some(1));
    assert_eq!(info.ttl_ms, -1);
    assert!(info.memory_bytes.unwrap_or(0) > 0);
    assert!(info.encoding.is_some());

    let mut all: Vec<Blob> = names.iter().map(|name| key(name)).collect();
    all.push(key("scan:hash"));
    engine.apply_edit(&RedisEdit::Delete { keys: all }).await.unwrap();

    // Seed data from dev/seed-nosql.sh, when loaded.
    let seeded = engine
        .scan_keys(&ScanRequest {
            pattern: Some("users:*".into()),
            page_size: Some(50),
            ..Default::default()
        })
        .await
        .unwrap();
    if !seeded.keys.is_empty() {
        let leaderboard = engine.key_info(&Blob::text("leaderboard")).await.unwrap();
        assert_eq!(leaderboard.key_type, RedisType::Zset);
    }
}

#[tokio::test]
#[ignore]
async fn live_engine_redis_types_read_write_page() {
    for resp3 in [false, true] {
        let engine = engine(resp3).await;
        let names = ["str", "json-str", "bin", "hash", "list", "set", "zset", "stream", "json"];
        reset(&engine, &names).await;

        // String: text, JSON detection, binary, TTL edits.
        let set = |name: &str, value: Blob| RedisEdit::SetString {
            key: key(name),
            value,
            ttl_ms: None,
            keep_ttl: false,
            condition: SetCondition::Always,
        };
        engine.apply_edit(&set("str", Blob::text("hello"))).await.unwrap();
        engine.apply_edit(&set("json-str", Blob::text(r#"{"a":[1,2]}"#))).await.unwrap();
        engine.apply_edit(&set("bin", Blob(vec![0, 159, 146, 150]))).await.unwrap();
        assert_eq!(engine.read_string(&key("str")).await.unwrap().format, StringFormat::Text);
        let json = engine.read_string(&key("json-str")).await.unwrap();
        assert_eq!(json.json.unwrap()["a"][1], 2);
        let binary = engine.read_string(&key("bin")).await.unwrap();
        assert_eq!((binary.format, binary.value.0.len()), (StringFormat::Binary, 4));
        let nx = RedisEdit::SetString {
            key: key("str"),
            value: Blob::text("x"),
            ttl_ms: None,
            keep_ttl: false,
            condition: SetCondition::IfMissing,
        };
        assert!(!engine.apply_edit(&nx).await.unwrap().applied);
        engine
            .apply_edit(&RedisEdit::Expire {
                key: key("str"),
                ttl_ms: 60_000,
            })
            .await
            .unwrap();
        assert!(engine.key_info(&key("str")).await.unwrap().ttl_ms > 0);
        engine.apply_edit(&RedisEdit::Persist { key: key("str") }).await.unwrap();
        assert_eq!(engine.key_info(&key("str")).await.unwrap().ttl_ms, -1);
        let rename = RedisEdit::Rename {
            key: key("str"),
            new_key: key("json-str"),
            overwrite: false,
        };
        assert!(!engine.apply_edit(&rename).await.unwrap().applied, "RENAMENX onto an existing key");

        // Hash: 1,000 fields read in HSCAN pages.
        let fields: Vec<FieldValue> = (0..1000)
            .map(|n| FieldValue {
                field: Blob::text(format!("f{n}")),
                value: Blob::text(n.to_string()),
            })
            .collect();
        engine.apply_edit(&RedisEdit::HashSet { key: key("hash"), fields }).await.unwrap();
        let (mut cursor, mut seen) = ("0".to_string(), std::collections::HashSet::new());
        loop {
            let page = engine.read_hash(&key("hash"), &cursor, None, Some(100)).await.unwrap();
            assert_eq!(page.total, 1000);
            seen.extend(page.items.into_iter().map(|item| item.field));
            if page.done {
                break;
            }
            cursor = page.cursor;
        }
        assert_eq!(seen.len(), 1000);
        engine
            .apply_edit(&RedisEdit::HashDelete {
                key: key("hash"),
                fields: vec![Blob::text("f0")],
            })
            .await
            .unwrap();
        let filtered = engine.read_hash(&key("hash"), "0", Some("f99*"), Some(5000)).await.unwrap();
        assert_eq!(filtered.items.len(), 11);

        // List: windows, set, remove.
        let values: Vec<Blob> = (0..500).map(|n| Blob::text(format!("item-{n}"))).collect();
        engine
            .apply_edit(&RedisEdit::ListPush {
                key: key("list"),
                values,
                end: ListEnd::Tail,
            })
            .await
            .unwrap();
        let page = engine.read_list(&key("list"), 100, Some(50)).await.unwrap();
        assert_eq!((page.total, page.start, page.items.len()), (500, 100, 50));
        assert_eq!(page.items[0], Blob::text("item-100"));
        let tail = engine.read_list(&key("list"), -10, Some(50)).await.unwrap();
        assert_eq!((tail.start, tail.items.len()), (490, 10));
        engine
            .apply_edit(&RedisEdit::ListSet {
                key: key("list"),
                index: 0,
                value: Blob::text("first"),
            })
            .await
            .unwrap();
        engine
            .apply_edit(&RedisEdit::ListRemove {
                key: key("list"),
                value: Blob::text("first"),
                count: 0,
            })
            .await
            .unwrap();
        assert_eq!(engine.read_list(&key("list"), 0, Some(1)).await.unwrap().total, 499);

        // Set.
        let members: Vec<Blob> = (0..300).map(|n| Blob::text(format!("m{n}"))).collect();
        engine.apply_edit(&RedisEdit::SetAdd { key: key("set"), members }).await.unwrap();
        engine
            .apply_edit(&RedisEdit::SetRemove {
                key: key("set"),
                members: vec![Blob::text("m0")],
            })
            .await
            .unwrap();
        let page = engine.read_set(&key("set"), "0", None, Some(50)).await.unwrap();
        assert_eq!(page.total, 299);
        assert!(!page.items.is_empty());

        // Sorted set: rank and score windows, increment, ZSCAN.
        let members: Vec<ScoredMember> = (0..100)
            .map(|n| ScoredMember {
                member: Blob::text(format!("p{n}")),
                score: n as f64,
            })
            .collect();
        engine.apply_edit(&RedisEdit::ZsetAdd { key: key("zset"), members }).await.unwrap();
        let top = engine
            .read_zset(
                &key("zset"),
                &ZsetRange::Rank {
                    start: 0,
                    count: Some(3),
                    rev: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(top.items.iter().map(|item| item.score).collect::<Vec<_>>(), vec![99.0, 98.0, 97.0]);
        let range = ZsetRange::Score {
            min: "(10".into(),
            max: "20".into(),
            offset: 2,
            count: Some(5),
            rev: false,
        };
        let by_score = engine.read_zset(&key("zset"), &range).await.unwrap();
        assert_eq!(by_score.items.first().map(|item| item.score), Some(13.0));
        assert_eq!(by_score.items.len(), 5);
        engine
            .apply_edit(&RedisEdit::ZsetIncrement {
                key: key("zset"),
                member: Blob::text("p0"),
                delta: 1000.5,
            })
            .await
            .unwrap();
        let top = engine
            .read_zset(
                &key("zset"),
                &ZsetRange::Rank {
                    start: 0,
                    count: Some(1),
                    rev: true,
                },
            )
            .await
            .unwrap();
        assert_eq!((top.items[0].member.display(), top.items[0].score), ("p0".to_string(), 1000.5));
        engine
            .apply_edit(&RedisEdit::ZsetRemove {
                key: key("zset"),
                members: vec![Blob::text("p0")],
            })
            .await
            .unwrap();
        assert_eq!(engine.scan_zset(&key("zset"), "0", None, Some(1000)).await.unwrap().total, 99);

        // Stream: paging forwards and backwards, delete, consumer groups.
        for n in 0..25 {
            let fields = vec![FieldValue {
                field: Blob::text("n"),
                value: Blob::text(n.to_string()),
            }];
            engine
                .apply_edit(&RedisEdit::StreamAdd {
                    key: key("stream"),
                    id: None,
                    fields,
                    max_len: None,
                })
                .await
                .unwrap();
        }
        let first = engine
            .read_stream(
                &key("stream"),
                &StreamRangeRequest {
                    count: Some(10),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!((first.entries.len(), first.length), (10, 25));
        let second = engine
            .read_stream(
                &key("stream"),
                &StreamRangeRequest {
                    after: first.next.clone(),
                    count: Some(10),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(second.entries[0].fields[0].value, Blob::text("10"));
        let newest = engine
            .read_stream(
                &key("stream"),
                &StreamRangeRequest {
                    reverse: true,
                    count: Some(5),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(newest.entries[0].fields[0].value, Blob::text("24"));
        engine
            .query(redis::cmd("XGROUP").arg("CREATE").arg(key("stream")).arg("workers").arg("0"))
            .await
            .unwrap();
        engine
            .query(
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg("workers")
                    .arg("w1")
                    .arg("COUNT")
                    .arg(3)
                    .arg("STREAMS")
                    .arg(key("stream"))
                    .arg(">"),
            )
            .await
            .unwrap();
        let info = engine.stream_info(&key("stream")).await.unwrap();
        assert_eq!(info.groups.len(), 1);
        assert_eq!((info.groups[0].pending, info.groups[0].consumer_list.len()), (3, 1));
        let delete = RedisEdit::StreamDelete {
            key: key("stream"),
            ids: vec![first.entries[0].id.clone()],
        };
        assert_eq!(engine.apply_edit(&delete).await.unwrap().count, Some(1));

        // RedisJSON.
        let doc = serde_json::json!({ "name": "Ada", "tags": ["x", "y"], "address": { "city": "London" } });
        engine
            .apply_edit(&RedisEdit::JsonSet {
                key: key("json"),
                path: "$".into(),
                value: doc.clone(),
            })
            .await
            .unwrap();
        let read = engine.read_json(&key("json"), None).await.unwrap();
        assert_eq!(read.value, doc);
        assert_eq!(read.json_type.as_deref(), Some("object"));
        assert_eq!(
            engine.read_json(&key("json"), Some("$.address.city")).await.unwrap().value,
            "London"
        );
        engine
            .apply_edit(&RedisEdit::JsonDelete {
                key: key("json"),
                path: "$.tags".into(),
            })
            .await
            .unwrap();
        assert!(engine.read_json(&key("json"), None).await.unwrap().value.get("tags").is_none());
        assert_eq!(engine.key_info(&key("json")).await.unwrap().key_type, RedisType::Json);

        reset(&engine, &names).await;
    }
}

#[tokio::test]
#[ignore]
async fn live_engine_redis_console() {
    let engine = engine(true).await;
    let mut console = engine.open_console().await.unwrap();
    let results = console
        .run("# console test\nSET fosta-test:console \"hello world\"\nGET fosta-test:console\nHSET fosta-test:console-h a 1 b 2\nHGETALL fosta-test:console-h\nNOSUCHCOMMAND\nDEL fosta-test:console fosta-test:console-h")
        .await;
    assert_eq!(results.len(), 6);
    assert_eq!(results[0].reply, Some(RedisReply::Ok));
    assert_eq!(results[0].line, 2);
    assert_eq!(
        results[1].reply,
        Some(RedisReply::Bulk {
            value: Blob::text("hello world")
        })
    );
    assert!(
        matches!(results[3].reply, Some(RedisReply::Map { ref entries }) if entries.len() == 2),
        "RESP3 map: {:?}",
        results[3].reply
    );
    assert!(
        matches!(results[4].reply, Some(RedisReply::Error { ref code, .. }) if code == "ERR"),
        "{:?}",
        results[4].reply
    );
    assert_eq!(results[5].reply, Some(RedisReply::Integer { value: 2 }));
    assert_eq!(results[5].classification.class, CommandClass::Write);

    // SELECT stays inside the console connection.
    let selected = console.run("SELECT 1\nSET fosta-test:db1 x\nDBSIZE").await;
    assert_eq!(selected[0].reply, Some(RedisReply::Ok));
    assert!(engine
        .scan_keys(&ScanRequest {
            pattern: Some("fosta-test:db1".into()),
            ..Default::default()
        })
        .await
        .unwrap()
        .keys
        .is_empty());
    console.run("DEL fosta-test:db1\nSELECT 0").await;

    // MULTI/EXEC takes the highest class of the queued commands.
    let transaction = console.run("MULTI\nSET fosta-test:tx 1\nFLUSHDB").await;
    assert_eq!(transaction.len(), 3);
    assert_eq!(console.classify_line("EXEC").unwrap().class, CommandClass::Destructive);
    let discard = console.run("DISCARD").await;
    assert_eq!(discard[0].reply, Some(RedisReply::Ok));

    // Refused and timed-out commands.
    let monitor = console.run("MONITOR").await;
    assert_eq!(monitor[0].classification.class, CommandClass::Unsupported);
    assert!(monitor[0].error.is_some());
    console.set_timeout(Duration::from_millis(300));
    let blocked = console.run("BLPOP fosta-test:never 5").await;
    assert!(
        blocked[0].error.as_deref().unwrap_or("").contains("did not answer"),
        "{:?}",
        blocked[0]
    );
    let unbalanced = console.run("GET \"open").await;
    assert!(unbalanced[0].error.as_deref().unwrap_or("").contains("unbalanced"));
}
