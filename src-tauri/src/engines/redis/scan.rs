//! Key browsing with `SCAN` (never `KEYS`): cursor paging with MATCH, TYPE and COUNT, plus per-key metadata.

use std::time::{Duration, Instant};

use redis::Value;
use serde::{Deserialize, Serialize};

use super::{
    connection::RedisEngine,
    types::{as_i64, as_string, scan_reply, server_error, Blob, RedisType},
};
use crate::error::AppResult;

const DEFAULT_COUNT: u32 = 500;
const DEFAULT_PAGE: usize = 500;
const MAX_PAGE: usize = 10_000;
/// A sparse pattern can need many SCAN calls per page; stop after this long and hand back the cursor.
const PAGE_TIME_BUDGET: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanRequest {
    /// `"0"` (or empty) starts a new iteration; otherwise the cursor a previous page returned.
    pub cursor: String,
    /// Glob pattern for `MATCH` (`users:*`). `None` scans every key.
    pub pattern: Option<String>,
    /// `TYPE` filter (Redis 6+).
    pub type_filter: Option<RedisType>,
    /// `COUNT` hint per SCAN call (default 500). Larger is faster on big databases, smaller keeps each call short.
    pub count: Option<u32>,
    /// How many keys to collect before returning (default 500, at most 10,000). Fewer come back when the time budget or
    /// the iteration ends first.
    pub page_size: Option<usize>,
    /// Also fetch `TYPE` and `PTTL` for every key (one pipelined round trip).
    pub with_meta: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanPage {
    /// Pass back for the next page; `"0"` when the iteration is complete.
    pub cursor: String,
    pub done: bool,
    pub keys: Vec<KeySummary>,
    /// SCAN calls made for this page.
    pub calls: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySummary {
    pub key: Blob,
    pub key_type: Option<RedisType>,
    /// Milliseconds to live; `-1` without expiry, `-2` when the key vanished meanwhile.
    pub ttl_ms: Option<i64>,
}

/// Details for one key (the key header in the value view).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    pub key: Blob,
    pub key_type: RedisType,
    pub ttl_ms: i64,
    /// `MEMORY USAGE` in bytes; `None` when the command is not allowed or not supported.
    pub memory_bytes: Option<u64>,
    /// `OBJECT ENCODING` (`listpack`, `hashtable`, `quicklist`, `embstr`...).
    pub encoding: Option<String>,
    /// Elements (or bytes for strings): STRLEN / HLEN / LLEN / SCARD / ZCARD / XLEN / JSON.DEBUG-free length.
    pub length: Option<u64>,
}

impl RedisEngine {
    /// One page of keys. Keeps calling SCAN until `page_size` keys are found, the iteration ends, or the time budget
    /// runs out, so sparse patterns do not return a stream of empty pages.
    pub async fn scan_keys(&self, request: &ScanRequest) -> AppResult<ScanPage> {
        let page_size = request.page_size.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        let count = request.count.unwrap_or(DEFAULT_COUNT).max(1);
        let mut cursor = if request.cursor.is_empty() {
            "0".to_string()
        } else {
            request.cursor.clone()
        };
        let mut keys: Vec<Blob> = Vec::new();
        let mut calls = 0;
        let started = Instant::now();
        loop {
            let mut cmd = redis::cmd("SCAN");
            cmd.arg(&cursor);
            if let Some(pattern) = request.pattern.as_deref().filter(|pattern| !pattern.is_empty() && *pattern != "*") {
                cmd.arg("MATCH").arg(pattern);
            }
            // Ask for no more than what is still missing, but never below a useful batch.
            cmd.arg("COUNT").arg(count);
            if let Some(kind) = &request.type_filter {
                cmd.arg("TYPE").arg(kind.server_name());
            }
            let (next, batch) = scan_reply(self.query(&cmd).await?)?;
            calls += 1;
            keys.extend(
                batch
                    .into_iter()
                    .map(|value| Blob(super::types::as_bytes(value).unwrap_or_default())),
            );
            cursor = next;
            if cursor == "0" || keys.len() >= page_size || started.elapsed() > PAGE_TIME_BUDGET {
                break;
            }
        }
        // SCAN may return a key twice across calls (rehashing); keep the first.
        let mut seen = std::collections::HashSet::new();
        keys.retain(|key| seen.insert(key.clone()));
        let summaries = if request.with_meta {
            self.key_summaries(keys).await?
        } else {
            keys.into_iter()
                .map(|key| KeySummary {
                    key,
                    key_type: None,
                    ttl_ms: None,
                })
                .collect()
        };
        Ok(ScanPage {
            done: cursor == "0",
            cursor,
            keys: summaries,
            calls,
        })
    }

    /// `TYPE` and `PTTL` for many keys in one pipelined round trip.
    pub async fn key_summaries(&self, keys: Vec<Blob>) -> AppResult<Vec<KeySummary>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::pipe();
        for key in &keys {
            pipe.cmd("TYPE").arg(key).cmd("PTTL").arg(key);
        }
        let mut replies = self.pipeline(&mut pipe).await?.into_iter();
        Ok(keys
            .into_iter()
            .map(|key| {
                let key_type = replies.next().and_then(as_string).map(|name| RedisType::parse(&name));
                let ttl_ms = replies.next().as_ref().and_then(as_i64);
                KeySummary { key, key_type, ttl_ms }
            })
            .collect())
    }

    /// Type, TTL, memory, encoding and length of one key. Commands the ACL forbids just leave their field empty.
    pub async fn key_info(&self, key: &Blob) -> AppResult<KeyInfo> {
        let key_type = match self.query(redis::cmd("TYPE").arg(key)).await.map(as_string)? {
            Some(name) => RedisType::parse(&name),
            None => RedisType::None,
        };
        let mut pipe = redis::pipe();
        pipe.cmd("PTTL").arg(key);
        pipe.cmd("MEMORY").arg("USAGE").arg(key).arg("SAMPLES").arg(0);
        pipe.cmd("OBJECT").arg("ENCODING").arg(key);
        let length_command = match key_type {
            RedisType::String => Some("STRLEN"),
            RedisType::Hash => Some("HLEN"),
            RedisType::List => Some("LLEN"),
            RedisType::Set => Some("SCARD"),
            RedisType::Zset => Some("ZCARD"),
            RedisType::Stream => Some("XLEN"),
            RedisType::Vectorset => Some("VCARD"),
            _ => None,
        };
        if let Some(command) = length_command {
            pipe.cmd(command).arg(key);
        }
        let replies = self.pipeline(&mut pipe).await?;
        let ok = |index: usize| {
            replies
                .get(index)
                .filter(|value| server_error(value).is_none() && **value != Value::Nil)
        };
        Ok(KeyInfo {
            key: key.clone(),
            key_type,
            ttl_ms: ok(0).and_then(as_i64).unwrap_or(-2),
            memory_bytes: ok(1).and_then(as_i64).map(|bytes| bytes.max(0) as u64),
            encoding: ok(2).cloned().and_then(as_string),
            length: if length_command.is_some() {
                ok(3).and_then(as_i64).map(|n| n.max(0) as u64)
            } else {
                None
            },
        })
    }

    /// `DBSIZE` of the selected database.
    pub async fn db_size(&self) -> AppResult<u64> {
        Ok(as_i64(&self.query(&redis::cmd("DBSIZE")).await?).unwrap_or(0).max(0) as u64)
    }
}
