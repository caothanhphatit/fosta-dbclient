//! Typed, paged value reads. Collections are always read in windows (HSCAN / SSCAN / ZSCAN / LRANGE / ZRANGE / XRANGE)
//! and strings above a size limit are cut, so opening a huge key never loads it whole.

use redis::Value;
use serde::{Deserialize, Serialize};

use super::{
    connection::RedisEngine,
    types::{as_blob, as_bytes, as_f64, as_i64, as_items, as_pairs, as_string, check, scan_reply, Blob},
};
use crate::error::{AppError, AppResult};

/// Strings longer than this are read with `GETRANGE` and flagged as truncated.
pub const STRING_PREVIEW_LIMIT: u64 = 1024 * 1024;
const DEFAULT_WINDOW: u32 = 200;
const MAX_WINDOW: u32 = 5_000;

fn window(count: Option<u32>) -> u32 {
    count.unwrap_or(DEFAULT_WINDOW).clamp(1, MAX_WINDOW)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StringFormat {
    /// Valid UTF-8 that is not JSON.
    Text,
    /// Valid JSON object or array (numbers and quoted strings stay `Text`).
    Json,
    /// Not valid UTF-8.
    Binary,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StringValue {
    pub value: Blob,
    /// Full length in bytes (`STRLEN`).
    pub length: u64,
    /// True when only the first [`STRING_PREVIEW_LIMIT`] bytes were read.
    pub truncated: bool,
    pub format: StringFormat,
    /// Parsed JSON when `format` is `Json`.
    pub json: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldValue {
    pub field: Blob,
    pub value: Blob,
}

/// A `SCAN`-family page (hash, set, sorted set).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPage<T> {
    pub items: Vec<T>,
    /// Next cursor; `"0"` when done.
    pub cursor: String,
    pub done: bool,
    /// Collection size (HLEN / SCARD / ZCARD).
    pub total: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListPage {
    /// Index of the first item.
    pub start: i64,
    pub items: Vec<Blob>,
    pub total: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoredMember {
    pub member: Blob,
    pub score: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZsetPage {
    pub items: Vec<ScoredMember>,
    /// Offset of the first item within the requested range (rank for `Rank`, LIMIT offset for `Score`).
    pub offset: u64,
    pub total: u64,
}

/// How to window a sorted set.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "by")]
pub enum ZsetRange {
    /// By rank: `ZRANGE key start start+count-1 [REV] WITHSCORES`.
    #[serde(rename_all = "camelCase")]
    Rank { start: u64, count: Option<u32>, rev: bool },
    /// By score: `ZRANGE key min max BYSCORE [REV] LIMIT offset count WITHSCORES`. Bounds use Redis syntax
    /// (`-inf`, `+inf`, `(5` for exclusive).
    #[serde(rename_all = "camelCase")]
    Score {
        min: String,
        max: String,
        offset: u64,
        count: Option<u32>,
        rev: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<FieldValue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamPage {
    pub entries: Vec<StreamEntry>,
    /// Pass as `after` (or `before` when reversed) to read the next page; `None` at the end.
    pub next: Option<String>,
    pub length: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StreamRangeRequest {
    /// Exclusive lower bound (the last id of the previous page) or `None` to start at the beginning (`-`).
    pub after: Option<String>,
    /// Exclusive upper bound for reverse paging, or `None` to start at the newest entry (`+`).
    pub before: Option<String>,
    pub count: Option<u32>,
    /// Newest first (`XREVRANGE`).
    pub reverse: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfo {
    pub length: u64,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
    pub groups: Vec<ConsumerGroup>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumerGroup {
    pub name: String,
    pub consumers: u64,
    pub pending: u64,
    pub last_delivered_id: String,
    /// Redis 7+: entries not yet delivered to the group.
    pub lag: Option<u64>,
    pub consumer_list: Vec<Consumer>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Consumer {
    pub name: String,
    pub pending: u64,
    pub idle_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonValue {
    /// The JSON at `path` (for `$` paths RedisJSON wraps matches in an array; this is unwrapped for a single match).
    pub value: serde_json::Value,
    /// `JSON.TYPE` at the root: object, array, string, integer, number, boolean, null.
    pub json_type: Option<String>,
    /// Bytes in memory (`JSON.DEBUG MEMORY`), when available.
    pub memory_bytes: Option<u64>,
}

impl RedisEngine {
    /// A string value: whole when small, the first megabyte otherwise; with text / JSON / binary detection.
    pub async fn read_string(&self, key: &Blob) -> AppResult<StringValue> {
        let length = as_i64(&self.query(redis::cmd("STRLEN").arg(key)).await?).unwrap_or(0).max(0) as u64;
        let truncated = length > STRING_PREVIEW_LIMIT;
        let reply = if truncated {
            self.query(redis::cmd("GETRANGE").arg(key).arg(0).arg(STRING_PREVIEW_LIMIT - 1))
                .await?
        } else {
            self.query(redis::cmd("GET").arg(key)).await?
        };
        if reply == Value::Nil {
            return Err(AppError::new("The key no longer exists."));
        }
        let bytes = as_bytes(reply).unwrap_or_default();
        let (format, json) = detect_format(&bytes, truncated);
        Ok(StringValue {
            value: Blob(bytes),
            length,
            truncated,
            format,
            json,
        })
    }

    /// One HSCAN page of a hash.
    pub async fn read_hash(
        &self,
        key: &Blob,
        cursor: &str,
        pattern: Option<&str>,
        count: Option<u32>,
    ) -> AppResult<CursorPage<FieldValue>> {
        let (cursor, items) = self.scan_collection("HSCAN", key, cursor, pattern, count).await?;
        let mut items = items.into_iter();
        let mut fields = Vec::new();
        while let (Some(field), Some(value)) = (items.next(), items.next()) {
            fields.push(FieldValue {
                field: as_blob(field),
                value: as_blob(value),
            });
        }
        let total = self.length("HLEN", key).await?;
        Ok(CursorPage {
            done: cursor == "0",
            cursor,
            items: fields,
            total,
        })
    }

    /// One SSCAN page of a set.
    pub async fn read_set(&self, key: &Blob, cursor: &str, pattern: Option<&str>, count: Option<u32>) -> AppResult<CursorPage<Blob>> {
        let (cursor, items) = self.scan_collection("SSCAN", key, cursor, pattern, count).await?;
        let total = self.length("SCARD", key).await?;
        Ok(CursorPage {
            done: cursor == "0",
            cursor,
            items: items.into_iter().map(as_blob).collect(),
            total,
        })
    }

    /// One ZSCAN page of a sorted set (unordered; use [`Self::read_zset`] for ordered windows).
    pub async fn scan_zset(
        &self,
        key: &Blob,
        cursor: &str,
        pattern: Option<&str>,
        count: Option<u32>,
    ) -> AppResult<CursorPage<ScoredMember>> {
        let (cursor, items) = self.scan_collection("ZSCAN", key, cursor, pattern, count).await?;
        let total = self.length("ZCARD", key).await?;
        Ok(CursorPage {
            done: cursor == "0",
            cursor,
            items: scored_members(items),
            total,
        })
    }

    /// A window of a list: `LRANGE key start start+count-1`. Negative `start` counts from the end.
    pub async fn read_list(&self, key: &Blob, start: i64, count: Option<u32>) -> AppResult<ListPage> {
        let count = window(count) as i64;
        let total = self.length("LLEN", key).await?;
        let start = if start < 0 { (total as i64 + start).max(0) } else { start };
        let reply = self.query(redis::cmd("LRANGE").arg(key).arg(start).arg(start + count - 1)).await?;
        Ok(ListPage {
            start,
            items: as_items(reply).into_iter().map(as_blob).collect(),
            total,
        })
    }

    /// A window of a sorted set by rank or by score, with scores.
    pub async fn read_zset(&self, key: &Blob, range: &ZsetRange) -> AppResult<ZsetPage> {
        let total = self.length("ZCARD", key).await?;
        let mut cmd = redis::cmd("ZRANGE");
        cmd.arg(key);
        let offset = match range {
            ZsetRange::Rank { start, count, rev } => {
                let count = window(*count) as u64;
                cmd.arg(*start).arg(start + count - 1);
                if *rev {
                    cmd.arg("REV");
                }
                *start
            }
            ZsetRange::Score {
                min,
                max,
                offset,
                count,
                rev,
            } => {
                // With REV, ZRANGE expects the bounds as max then min.
                if *rev {
                    cmd.arg(max).arg(min).arg("BYSCORE").arg("REV");
                } else {
                    cmd.arg(min).arg(max).arg("BYSCORE");
                }
                cmd.arg("LIMIT").arg(*offset).arg(window(*count));
                *offset
            }
        };
        cmd.arg("WITHSCORES");
        let reply = check(self.query(&cmd).await?)?;
        Ok(ZsetPage {
            items: scored_members(as_items(reply)),
            offset,
            total,
        })
    }

    /// One page of a stream (XRANGE / XREVRANGE with exclusive bounds, Redis 6.2+).
    pub async fn read_stream(&self, key: &Blob, request: &StreamRangeRequest) -> AppResult<StreamPage> {
        let count = window(request.count);
        let exclusive = |id: &Option<String>, open: &str| id.as_ref().map(|id| format!("({id}")).unwrap_or_else(|| open.to_string());
        let (low, high) = (exclusive(&request.after, "-"), exclusive(&request.before, "+"));
        let mut cmd = if request.reverse {
            redis::cmd("XREVRANGE")
        } else {
            redis::cmd("XRANGE")
        };
        cmd.arg(key);
        if request.reverse {
            cmd.arg(&high).arg(&low);
        } else {
            cmd.arg(&low).arg(&high);
        }
        cmd.arg("COUNT").arg(count);
        let entries = parse_stream_entries(check(self.query(&cmd).await?)?);
        let length = self.length("XLEN", key).await?;
        let next = if entries.len() as u32 == count {
            entries.last().map(|entry| entry.id.clone())
        } else {
            None
        };
        Ok(StreamPage { entries, next, length })
    }

    /// Stream summary with consumer groups and their consumers.
    pub async fn stream_info(&self, key: &Blob) -> AppResult<StreamInfo> {
        let info = fields_of(check(self.query(redis::cmd("XINFO").arg("STREAM").arg(key)).await?)?);
        let entry_id = |name: &str| {
            info.iter()
                .find(|(field, _)| field == name)
                .and_then(|(_, value)| as_items(value.clone()).into_iter().next())
                .and_then(as_string)
        };
        let length = info
            .iter()
            .find(|(field, _)| field == "length")
            .and_then(|(_, value)| as_i64(value))
            .unwrap_or(0)
            .max(0) as u64;
        let (first_id, last_id) = (entry_id("first-entry"), entry_id("last-entry"));
        let groups_reply = check(self.query(redis::cmd("XINFO").arg("GROUPS").arg(key)).await?)?;
        let mut groups = Vec::new();
        for group in as_items(groups_reply) {
            let fields = fields_of(group);
            let text = |name: &str| {
                fields
                    .iter()
                    .find(|(field, _)| field == name)
                    .and_then(|(_, value)| as_string(value.clone()))
            };
            let number = |name: &str| {
                fields
                    .iter()
                    .find(|(field, _)| field == name)
                    .and_then(|(_, value)| as_i64(value))
                    .map(|n| n.max(0) as u64)
            };
            let name = text("name").unwrap_or_default();
            let consumers_reply = self
                .query(redis::cmd("XINFO").arg("CONSUMERS").arg(key).arg(&name))
                .await
                .unwrap_or(Value::Nil);
            let consumer_list = as_items(consumers_reply)
                .into_iter()
                .map(|consumer| {
                    let fields = fields_of(consumer);
                    let find = |name: &str| fields.iter().find(|(field, _)| field == name).map(|(_, value)| value.clone());
                    Consumer {
                        name: find("name").and_then(as_string).unwrap_or_default(),
                        pending: find("pending").as_ref().and_then(as_i64).unwrap_or(0).max(0) as u64,
                        idle_ms: find("idle").as_ref().and_then(as_i64).unwrap_or(0).max(0) as u64,
                    }
                })
                .collect();
            groups.push(ConsumerGroup {
                consumers: number("consumers").unwrap_or(0),
                pending: number("pending").unwrap_or(0),
                last_delivered_id: text("last-delivered-id").unwrap_or_default(),
                lag: number("lag"),
                consumer_list,
                name,
            });
        }
        Ok(StreamInfo {
            length,
            first_id,
            last_id,
            groups,
        })
    }

    /// A RedisJSON document (or the part at `path`, JSONPath like `$.address`). Refuses documents above the string limit.
    pub async fn read_json(&self, key: &Blob, path: Option<&str>) -> AppResult<JsonValue> {
        if !self.has_json() {
            return Err(AppError::new("This server has no RedisJSON module, so JSON keys cannot be read."));
        }
        let path = path.filter(|path| !path.is_empty()).unwrap_or("$");
        let memory_bytes = self
            .query(redis::cmd("JSON.DEBUG").arg("MEMORY").arg(key))
            .await
            .ok()
            .and_then(|value| as_i64(&value))
            .map(|n| n.max(0) as u64);
        if memory_bytes.is_some_and(|bytes| bytes > STRING_PREVIEW_LIMIT * 8) {
            return Err(AppError::new(
                "This JSON document is too large to open whole. Read a smaller path, e.g. `$.items[0:100]`.",
            ));
        }
        let text = as_string(check(self.query(redis::cmd("JSON.GET").arg(key).arg(path)).await?)?)
            .ok_or_else(|| AppError::new("The key no longer exists."))?;
        let mut value: serde_json::Value = serde_json::from_str(&text)?;
        if path.starts_with('$') {
            if let serde_json::Value::Array(items) = &mut value {
                if items.len() == 1 {
                    value = items.remove(0);
                }
            }
        }
        let json_type = self.query(redis::cmd("JSON.TYPE").arg(key)).await.ok().and_then(|value| {
            // RESP3 / `$` paths return a nested array; the legacy root path returns a plain string.
            let first = as_items(value.clone()).into_iter().next().unwrap_or(value);
            as_string(as_items(first.clone()).into_iter().next().unwrap_or(first))
        });
        Ok(JsonValue {
            value,
            json_type,
            memory_bytes,
        })
    }

    async fn scan_collection(
        &self,
        command: &str,
        key: &Blob,
        cursor: &str,
        pattern: Option<&str>,
        count: Option<u32>,
    ) -> AppResult<(String, Vec<Value>)> {
        let mut cmd = redis::cmd(command);
        cmd.arg(key).arg(if cursor.is_empty() { "0" } else { cursor });
        if let Some(pattern) = pattern.filter(|pattern| !pattern.is_empty() && *pattern != "*") {
            cmd.arg("MATCH").arg(pattern);
        }
        cmd.arg("COUNT").arg(window(count));
        scan_reply(self.query(&cmd).await?)
    }

    async fn length(&self, command: &str, key: &Blob) -> AppResult<u64> {
        Ok(as_i64(&check(self.query(redis::cmd(command).arg(key)).await?)?).unwrap_or(0).max(0) as u64)
    }
}

/// Text, JSON or binary. A truncated value is never parsed as JSON.
pub fn detect_format(bytes: &[u8], truncated: bool) -> (StringFormat, Option<serde_json::Value>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        // A cut in the middle of a multi-byte character is still text.
        if truncated && std::str::from_utf8(&bytes[..bytes.len().saturating_sub(3)]).is_ok() {
            return (StringFormat::Text, None);
        }
        return (StringFormat::Binary, None);
    };
    let trimmed = text.trim_start();
    if !truncated && (trimmed.starts_with('{') || trimmed.starts_with('[')) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
            return (StringFormat::Json, Some(json));
        }
    }
    (StringFormat::Text, None)
}

/// Member/score pairs from a flat `WITHSCORES` / ZSCAN reply or RESP3 nested pairs.
fn scored_members(items: Vec<Value>) -> Vec<ScoredMember> {
    let nested = items.first().is_some_and(|item| matches!(item, Value::Array(_)));
    if nested {
        return items
            .into_iter()
            .filter_map(|pair| {
                let mut parts = as_items(pair).into_iter();
                let member = as_blob(parts.next()?);
                let score = as_f64(&parts.next()?)?;
                Some(ScoredMember { member, score })
            })
            .collect();
    }
    let mut members = Vec::new();
    let mut items = items.into_iter();
    while let (Some(member), Some(score)) = (items.next(), items.next()) {
        members.push(ScoredMember {
            member: as_blob(member),
            score: as_f64(&score).unwrap_or(f64::NAN),
        });
    }
    members
}

pub(crate) fn parse_stream_entries(reply: Value) -> Vec<StreamEntry> {
    as_items(reply)
        .into_iter()
        .filter_map(|entry| {
            let mut parts = as_items(entry).into_iter();
            let id = as_string(parts.next()?)?;
            let fields = as_pairs(parts.next().unwrap_or(Value::Nil))
                .into_iter()
                .map(|(field, value)| FieldValue {
                    field: as_blob(field),
                    value: as_blob(value),
                })
                .collect();
            Some(StreamEntry { id, fields })
        })
        .collect()
}

/// `XINFO` replies: field name (as text) and value.
fn fields_of(value: Value) -> Vec<(String, Value)> {
    as_pairs(value)
        .into_iter()
        .filter_map(|(field, value)| as_string(field).map(|field| (field, value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_string_formats() {
        assert_eq!(detect_format(br#"{"a":1}"#, false).0, StringFormat::Json);
        assert_eq!(detect_format(br#"{"a":1"#, false).0, StringFormat::Text);
        assert_eq!(detect_format(br#"{"a":1}"#, true).0, StringFormat::Text);
        assert_eq!(detect_format(b"42", false).0, StringFormat::Text);
        assert_eq!(detect_format(&[0xff, 0xfe, 0x00], false).0, StringFormat::Binary);
    }

    #[test]
    fn parses_scored_members_in_both_protocols() {
        let flat = vec![Value::BulkString(b"a".to_vec()), Value::BulkString(b"1.5".to_vec())];
        let parsed = scored_members(flat);
        assert_eq!((parsed[0].member.display(), parsed[0].score), ("a".to_string(), 1.5));
        let nested = vec![Value::Array(vec![Value::BulkString(b"b".to_vec()), Value::Double(2.0)])];
        assert_eq!(scored_members(nested)[0].score, 2.0);
    }
}
