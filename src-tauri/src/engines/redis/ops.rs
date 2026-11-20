//! The operations engine views call through `Connection::invoke` (`redis.scan`, `redis.keyInfo`, `redis.readValue`,
//! `redis.edit`, `redis.info`, `redis.slowlog`). Each takes the engine of the database the arguments name (`db`), parses its
//! own arguments and returns JSON the UI types mirror (`src/engines/redis/model.ts`). `redis.scan` only ever uses `SCAN`.
//! On a Redis Cluster `redis.info` adds the cluster overview (or shows one node with `{ node: "host:port" }`) and
//! `redis.slowlog` merges every node's log.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use super::{
    connection::RedisEngine,
    edit::RedisEdit,
    guard::{apply_guarded, Expect},
    scan::{KeyInfo, ScanRequest},
    types::{as_bytes, as_i64, as_items, as_string, server_error, Blob, RedisType},
    values::{CursorPage, FieldValue, JsonValue, ListPage, StreamInfo, StreamPage, StreamRangeRequest, StringValue, ZsetPage, ZsetRange},
};
use crate::error::{AppError, AppResult};

/// Samples for `MEMORY USAGE` in the key list: the server's default (5) estimates big collections without reading them all.
const MEMORY_SAMPLES: u32 = 5;
const MAX_SLOWLOG: u32 = 1000;

fn parse<T: serde::de::DeserializeOwned>(args: &Json, what: &str) -> AppResult<T> {
    serde_json::from_value(args.clone()).map_err(|error| AppError::new(format!("Invalid arguments for {what}: {error}")))
}

/// The database a call is about (`db`, default 0).
pub fn db_of(args: &Json) -> i64 {
    args.get("db").and_then(Json::as_i64).unwrap_or(0)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanArgs {
    #[serde(flatten)]
    scan: ScanRequest,
    /// `MEMORY USAGE` per key of the page, one pipelined round trip.
    #[serde(default)]
    with_memory: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyRow {
    pub key: Blob,
    pub key_type: Option<RedisType>,
    pub ttl_ms: Option<i64>,
    pub memory_bytes: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyPage {
    pub cursor: String,
    pub done: bool,
    pub keys: Vec<KeyRow>,
    pub calls: u32,
    /// `DBSIZE`, so the UI can say "loaded 500 of 20,000".
    pub db_size: Option<u64>,
}

/// `MEMORY USAGE` of each key; a key that vanished or a forbidden command leaves `None`.
pub async fn memory_of(engine: &RedisEngine, keys: &[Blob]) -> Vec<Option<u64>> {
    if keys.is_empty() {
        return Vec::new();
    }
    let mut pipe = redis::pipe();
    for key in keys {
        pipe.cmd("MEMORY").arg("USAGE").arg(key).arg("SAMPLES").arg(MEMORY_SAMPLES);
    }
    match engine.pipeline(&mut pipe).await {
        Ok(replies) => replies
            .iter()
            .map(|reply| {
                if server_error(reply).is_some() {
                    None
                } else {
                    as_i64(reply).map(|bytes| bytes.max(0) as u64)
                }
            })
            .collect(),
        Err(_) => vec![None; keys.len()],
    }
}

pub async fn scan(engine: &RedisEngine, args: &Json) -> AppResult<Json> {
    let mut args: ScanArgs = parse(args, "redis.scan")?;
    // The list always shows the type and the TTL; they come in the same pass.
    args.scan.with_meta = true;
    let page = engine.scan_keys(&args.scan).await?;
    let keys: Vec<Blob> = page.keys.iter().map(|summary| summary.key.clone()).collect();
    let memory = if args.with_memory {
        memory_of(engine, &keys).await
    } else {
        Vec::new()
    };
    let rows = page
        .keys
        .into_iter()
        .enumerate()
        .map(|(index, summary)| KeyRow {
            key: summary.key,
            key_type: summary.key_type,
            ttl_ms: summary.ttl_ms,
            memory_bytes: memory.get(index).copied().flatten(),
        })
        .collect();
    let db_size = engine.db_size().await.ok();
    Ok(serde_json::to_value(KeyPage {
        cursor: page.cursor,
        done: page.done,
        keys: rows,
        calls: page.calls,
        db_size,
    })?)
}

#[derive(Deserialize)]
struct KeyArgs {
    key: Blob,
}

/// A key's header: type, TTL, encoding, length, and `MEMORY USAGE` (default sampling, so it stays cheap on big keys).
pub async fn key_info(engine: &RedisEngine, args: &Json) -> AppResult<KeyInfo> {
    let KeyArgs { key } = parse(args, "redis.keyInfo")?;
    let mut info = engine.key_info(&key).await?;
    // `key_info` asks for exact memory (SAMPLES 0), which walks every element: use the sampled estimate instead.
    info.memory_bytes = memory_of(engine, std::slice::from_ref(&key)).await.into_iter().next().flatten();
    Ok(info)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadArgs {
    key: Blob,
    /// SCAN-family cursor (hash, set).
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    count: Option<u32>,
    /// First index of a list window.
    #[serde(default)]
    start: Option<i64>,
    #[serde(default)]
    zset: Option<ZsetRange>,
    #[serde(default)]
    stream: Option<StreamRangeRequest>,
    #[serde(default)]
    json_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "type", content = "data")]
pub enum ValuePayload {
    String(StringValue),
    Hash(CursorPage<FieldValue>),
    List(ListPage),
    Set(CursorPage<Blob>),
    Zset(ZsetPage),
    Stream {
        page: StreamPage,
        info: StreamInfo,
    },
    Json(JsonValue),
    /// The key does not exist (any more).
    None,
    /// A type with no editor (time series, vector sets, other modules): the console can still work on it.
    #[serde(rename_all = "camelCase")]
    Unsupported {
        type_name: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueRead {
    pub info: KeyInfo,
    pub value: ValuePayload,
}

pub async fn read_value(engine: &RedisEngine, args: &Json) -> AppResult<ValueRead> {
    let read: ReadArgs = parse(args, "redis.readValue")?;
    let info = key_info(engine, args).await?;
    let key = &read.key;
    let pattern = read.pattern.as_deref().filter(|pattern| !pattern.is_empty());
    let cursor = read.cursor.as_deref().unwrap_or("0");
    let value = match &info.key_type {
        RedisType::None => ValuePayload::None,
        RedisType::String => ValuePayload::String(engine.read_string(key).await?),
        RedisType::Hash => ValuePayload::Hash(engine.read_hash(key, cursor, pattern, read.count).await?),
        RedisType::Set => ValuePayload::Set(engine.read_set(key, cursor, pattern, read.count).await?),
        RedisType::List => ValuePayload::List(engine.read_list(key, read.start.unwrap_or(0), read.count).await?),
        RedisType::Zset => {
            let range = read.zset.clone().unwrap_or(ZsetRange::Rank {
                start: 0,
                count: read.count,
                rev: false,
            });
            ValuePayload::Zset(engine.read_zset(key, &range).await?)
        }
        RedisType::Stream => {
            let mut request = read.stream.clone().unwrap_or_default();
            if request.count.is_none() {
                request.count = read.count;
            }
            ValuePayload::Stream {
                page: engine.read_stream(key, &request).await?,
                info: engine.stream_info(key).await?,
            }
        }
        RedisType::Json => ValuePayload::Json(engine.read_json(key, read.json_path.as_deref()).await?),
        other => ValuePayload::Unsupported {
            type_name: other.server_name().to_owned(),
        },
    };
    Ok(ValueRead { info, value })
}

/// `redis.edit`: a [`RedisEdit`] plus an optional `expect` (see [`super::guard`]). With `expect` the write fails with `CONFLICT`
/// when the value changed since it was read; without, it is unconditional (the UI's "overwrite anyway").
pub async fn edit(engine: &RedisEngine, args: &Json) -> AppResult<Json> {
    let edit: RedisEdit = parse(args, "redis.edit")?;
    let expect = match args.get("expect").filter(|expect| !expect.is_null()) {
        Some(expect) => Some(parse::<Expect>(expect, "redis.edit expect")?),
        None => None,
    };
    let result = match &expect {
        Some(expect) => apply_guarded(engine, &edit, expect).await?,
        None => engine.apply_edit(&edit).await?,
    };
    Ok(serde_json::to_value(result)?)
}

#[derive(Default, Deserialize)]
struct InfoArgs {
    /// `host:port` of one cluster node; the whole cluster (or the single server) when absent.
    #[serde(default)]
    node: Option<String>,
}

/// Server information: `INFO` sections plus the headline figures; on a cluster also the cluster overview, or one node's
/// own `INFO` with `{ node: "host:port" }`.
pub async fn info(engine: &RedisEngine, args: &Json) -> AppResult<Json> {
    let InfoArgs { node } = if args.is_null() {
        InfoArgs::default()
    } else {
        parse(args, "redis.info")?
    };
    let info = match node.as_deref().map(str::trim).filter(|node| !node.is_empty()) {
        Some(node) if engine.is_cluster() => engine.node_server_info(node).await?,
        Some(node) if node != engine.address() => {
            return Err(AppError::new(format!(
                "{node} is not this server: only a Redis Cluster connection shows other nodes."
            )))
        }
        _ => engine.server_info().await?,
    };
    Ok(serde_json::to_value(info)?)
}

#[derive(Deserialize)]
struct SlowlogArgs {
    #[serde(default)]
    count: Option<u32>,
}

/// `SLOWLOG GET` entries; `node` names the cluster node they came from.
fn slowlog_entries(reply: redis::Value, node: Option<&str>) -> Vec<Json> {
    as_items(reply)
        .into_iter()
        .filter_map(|entry| {
            let mut parts = as_items(entry).into_iter();
            let id = as_i64(&parts.next()?)?;
            let timestamp = as_i64(&parts.next()?)?;
            let duration_us = as_i64(&parts.next()?)?;
            let command: Vec<String> = as_items(parts.next()?)
                .into_iter()
                .map(|arg| String::from_utf8_lossy(&as_bytes(arg).unwrap_or_default()).into_owned())
                .collect();
            let client = parts.next().and_then(as_string).filter(|text| !text.is_empty());
            let name = parts.next().and_then(as_string).filter(|text| !text.is_empty());
            let mut entry = json!({
                "id": id, "timestamp": timestamp, "durationUs": duration_us, "command": command,
                "client": client, "clientName": name,
            });
            if let Some(node) = node {
                entry["node"] = json!(node);
            }
            Some(entry)
        })
        .collect()
}

/// The newest first: by timestamp, then by id (entries of one second on one node).
fn merge_slowlogs(mut entries: Vec<Json>, count: usize) -> Vec<Json> {
    let key = |entry: &Json| (entry["timestamp"].as_i64().unwrap_or(0), entry["id"].as_i64().unwrap_or(0));
    entries.sort_by_key(|entry| std::cmp::Reverse(key(entry)));
    entries.truncate(count);
    entries
}

/// The newest slow commands (`SLOWLOG GET`), the log length and the two settings that decide what is logged. On a cluster
/// every node's log is read and merged by time, each entry naming its `node`; a node that does not answer is left out.
pub async fn slowlog(engine: &RedisEngine, args: &Json) -> AppResult<Json> {
    let SlowlogArgs { count } = parse(args, "redis.slowlog")?;
    let count = count.unwrap_or(128).clamp(1, MAX_SLOWLOG);
    let get = redis::cmd("SLOWLOG").arg("GET").arg(count).clone();
    let len = redis::cmd("SLOWLOG").arg("LEN").clone();
    let (entries, length) = if engine.is_cluster() {
        let nodes = engine.all_nodes().await?;
        let logs = futures_util::future::join_all(nodes.iter().map(|node| async {
            let entries = engine.query_node(&get, node).await;
            let length = engine.query_node(&len, node).await;
            (entries, length)
        }))
        .await;
        let mut entries = Vec::new();
        let mut length = None;
        for (node, (log, size)) in nodes.iter().zip(logs) {
            if let Ok(log) = log {
                entries.extend(slowlog_entries(log, Some(&node.addr)));
            }
            if let Some(size) = size.ok().as_ref().and_then(as_i64) {
                length = Some(length.unwrap_or(0) + size);
            }
        }
        (merge_slowlogs(entries, count as usize), length)
    } else {
        let entries = slowlog_entries(engine.query(&get).await?, None);
        let length = engine.query(&len).await.ok().and_then(|reply| as_i64(&reply));
        (entries, length)
    };
    let threshold = engine.config_get("slowlog-log-slower-than").await.ok().flatten();
    let max_len = engine.config_get("slowlog-max-len").await.ok().flatten();
    Ok(json!({ "length": length, "entries": entries, "slowerThanUs": threshold, "maxLen": max_len }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_arguments_flatten_the_core_request() {
        let args: ScanArgs = serde_json::from_value(json!({
            "db": 2, "cursor": "17", "pattern": "user:*", "typeFilter": "hash", "pageSize": 50, "withMemory": true
        }))
        .unwrap();
        assert_eq!(args.scan.cursor, "17");
        assert_eq!(args.scan.pattern.as_deref(), Some("user:*"));
        assert_eq!(args.scan.type_filter, Some(RedisType::Hash));
        assert_eq!(args.scan.page_size, Some(50));
        assert!(args.with_memory);
        let defaults: ScanArgs = serde_json::from_value(json!({})).unwrap();
        assert!(defaults.scan.cursor.is_empty() && !defaults.with_memory);
        assert_eq!(db_of(&json!({ "db": 5 })), 5);
        assert_eq!(db_of(&json!({})), 0);
    }

    #[test]
    fn value_payloads_are_tagged_by_type() {
        let payload = serde_json::to_value(ValuePayload::Set(CursorPage {
            items: vec![Blob::text("a")],
            cursor: "0".into(),
            done: true,
            total: 1,
        }))
        .unwrap();
        assert_eq!(payload["type"], "set");
        assert_eq!(payload["data"]["items"][0]["text"], "a");
        assert_eq!(serde_json::to_value(ValuePayload::None).unwrap(), json!({ "type": "none" }));
        let unsupported = serde_json::to_value(ValuePayload::Unsupported {
            type_name: "TSDB-TYPE".into(),
        })
        .unwrap();
        assert_eq!(unsupported["data"]["typeName"], "TSDB-TYPE");
    }

    #[test]
    fn cluster_slowlogs_merge_newest_first_with_their_node() {
        let entry = |id: i64, timestamp: i64| {
            redis::Value::Array(vec![
                redis::Value::Int(id),
                redis::Value::Int(timestamp),
                redis::Value::Int(15_000),
                redis::Value::Array(vec![redis::Value::BulkString(b"DEBUG".to_vec())]),
            ])
        };
        let mut entries = slowlog_entries(redis::Value::Array(vec![entry(1, 100), entry(2, 300)]), Some("127.0.0.1:7001"));
        entries.extend(slowlog_entries(redis::Value::Array(vec![entry(9, 200)]), Some("127.0.0.1:7002")));
        let merged = merge_slowlogs(entries, 2);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            (merged[0]["id"].as_i64(), merged[0]["node"].as_str()),
            (Some(2), Some("127.0.0.1:7001"))
        );
        assert_eq!(
            (merged[1]["id"].as_i64(), merged[1]["node"].as_str()),
            (Some(9), Some("127.0.0.1:7002"))
        );
        let single = slowlog_entries(redis::Value::Array(vec![entry(1, 100)]), None);
        assert!(single[0].get("node").is_none());
        assert_eq!(single[0]["command"][0], "DEBUG");
    }

    #[test]
    fn edits_with_an_expectation_parse_both_parts() {
        let args = json!({
            "op": "setString", "key": { "text": "k" }, "value": { "text": "new" }, "ttlMs": null, "keepTtl": true, "condition": "always",
            "db": 1, "expect": { "kind": "string", "value": { "text": "old" } }
        });
        assert!(parse::<RedisEdit>(&args, "t").is_ok());
        assert!(parse::<Expect>(&args["expect"], "t").is_ok());
        assert!(parse::<RedisEdit>(&json!({ "op": "teleport" }), "t")
            .unwrap_err()
            .to_string()
            .contains("Invalid arguments"));
    }
}
