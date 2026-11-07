//! Shared Redis shapes: binary-safe bytes for IPC, data types, and helpers that read RESP2 and RESP3 replies alike.

use base64::{engine::general_purpose::STANDARD, Engine};
use redis::Value;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// Binary-safe bytes (keys, members, values). Serialised as `{ "text": "..." }` when the bytes are valid UTF-8 and as
/// `{ "base64": "..." }` otherwise, so any key survives a round trip through the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "BlobWire", into = "BlobWire")]
pub struct Blob(pub Vec<u8>);

#[derive(Serialize, Deserialize)]
struct BlobWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base64: Option<String>,
}

impl From<Blob> for BlobWire {
    fn from(blob: Blob) -> Self {
        match String::from_utf8(blob.0) {
            Ok(text) => BlobWire {
                text: Some(text),
                base64: None,
            },
            Err(error) => BlobWire {
                text: None,
                base64: Some(STANDARD.encode(error.into_bytes())),
            },
        }
    }
}

impl TryFrom<BlobWire> for Blob {
    type Error = String;

    fn try_from(wire: BlobWire) -> Result<Self, Self::Error> {
        match (wire.text, wire.base64) {
            (Some(text), None) => Ok(Blob(text.into_bytes())),
            (None, Some(encoded)) => STANDARD
                .decode(encoded)
                .map(Blob)
                .map_err(|error| format!("Invalid base64 value: {error}")),
            _ => Err("A binary value needs exactly one of `text` or `base64`.".into()),
        }
    }
}

impl Blob {
    pub fn text(value: impl Into<String>) -> Self {
        Blob(value.into().into_bytes())
    }

    /// Lossy text for display and for the prefix tree.
    pub fn display(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }

    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
}

impl From<&str> for Blob {
    fn from(value: &str) -> Self {
        Blob(value.as_bytes().to_vec())
    }
}

impl redis::ToRedisArgs for Blob {
    fn write_redis_args<W: ?Sized + redis::RedisWrite>(&self, out: &mut W) {
        out.write_arg(&self.0);
    }
}

impl redis::ToSingleRedisArg for Blob {}

/// The data type of a key, as `TYPE` reports it. Module types are mapped where the app has a view for them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RedisType {
    String,
    List,
    Set,
    Zset,
    Hash,
    Stream,
    /// RedisJSON (`ReJSON-RL`).
    Json,
    /// RedisTimeSeries (`TSDB-TYPE`).
    TimeSeries,
    /// Redis 8 vector sets.
    Vectorset,
    /// The key does not exist (`none`).
    None,
    /// Any other module type, with the name `TYPE` returned.
    Other(String),
}

impl RedisType {
    pub fn parse(name: &str) -> Self {
        match name {
            "string" => Self::String,
            "list" => Self::List,
            "set" => Self::Set,
            "zset" => Self::Zset,
            "hash" => Self::Hash,
            "stream" => Self::Stream,
            "ReJSON-RL" => Self::Json,
            "TSDB-TYPE" => Self::TimeSeries,
            "vectorset" => Self::Vectorset,
            "none" => Self::None,
            other => Self::Other(other.to_string()),
        }
    }

    /// The name `SCAN ... TYPE` expects.
    pub fn server_name(&self) -> &str {
        match self {
            Self::String => "string",
            Self::List => "list",
            Self::Set => "set",
            Self::Zset => "zset",
            Self::Hash => "hash",
            Self::Stream => "stream",
            Self::Json => "ReJSON-RL",
            Self::TimeSeries => "TSDB-TYPE",
            Self::Vectorset => "vectorset",
            Self::None => "none",
            Self::Other(name) => name,
        }
    }
}

// ---- Reply helpers (RESP2 and RESP3 shapes) ----

pub(crate) fn server_error(value: &Value) -> Option<String> {
    match value {
        Value::ServerError(error) => Some(match error.details() {
            Some(details) => format!("{} {details}", error.code()),
            None => error.code().to_string(),
        }),
        _ => None,
    }
}

pub(crate) fn as_bytes(value: Value) -> Option<Vec<u8>> {
    match value {
        Value::BulkString(bytes) => Some(bytes),
        Value::SimpleString(text) => Some(text.into_bytes()),
        Value::VerbatimString { text, .. } => Some(text.into_bytes()),
        Value::Okay => Some(b"OK".to_vec()),
        Value::Int(number) => Some(number.to_string().into_bytes()),
        Value::Double(number) => Some(number.to_string().into_bytes()),
        _ => None,
    }
}

pub(crate) fn as_blob(value: Value) -> Blob {
    Blob(as_bytes(value).unwrap_or_default())
}

pub(crate) fn as_string(value: Value) -> Option<String> {
    as_bytes(value).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(number) => Some(*number),
        Value::BulkString(bytes) => std::str::from_utf8(bytes).ok()?.trim().parse().ok(),
        Value::SimpleString(text) => text.trim().parse().ok(),
        Value::Double(number) => Some(*number as i64),
        _ => None,
    }
}

pub(crate) fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Double(number) => Some(*number),
        Value::Int(number) => Some(*number as f64),
        Value::BulkString(bytes) => parse_score(std::str::from_utf8(bytes).ok()?),
        Value::SimpleString(text) => parse_score(text),
        _ => None,
    }
}

fn parse_score(text: &str) -> Option<f64> {
    match text.trim() {
        "inf" | "+inf" => Some(f64::INFINITY),
        "-inf" => Some(f64::NEG_INFINITY),
        other => other.parse().ok(),
    }
}

/// Elements of an array, set or push reply.
pub(crate) fn as_items(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) | Value::Set(items) => items,
        Value::Push { data, .. } => data,
        Value::Nil => Vec::new(),
        Value::Map(pairs) => pairs.into_iter().flat_map(|(key, value)| [key, value]).collect(),
        Value::Attribute { data, .. } => as_items(*data),
        other => vec![other],
    }
}

/// Key/value pairs from a RESP3 map or a flat RESP2 array (`HGETALL`, `XINFO`, `CONFIG GET`...).
pub(crate) fn as_pairs(value: Value) -> Vec<(Value, Value)> {
    match value {
        Value::Map(pairs) => pairs,
        Value::Attribute { data, .. } => as_pairs(*data),
        other => {
            let mut items = as_items(other).into_iter();
            let mut pairs = Vec::new();
            while let (Some(key), Some(value)) = (items.next(), items.next()) {
                pairs.push((key, value));
            }
            pairs
        }
    }
}

/// A reply turned into an error when the server sent one (inside a pipeline with `ignore_errors`).
pub(crate) fn check(value: Value) -> AppResult<Value> {
    match server_error(&value) {
        Some(message) => Err(AppError::new(message)),
        None => Ok(value),
    }
}

/// `(cursor, items)` from a `SCAN`-family reply.
pub(crate) fn scan_reply(value: Value) -> AppResult<(String, Vec<Value>)> {
    let mut parts = as_items(check(value)?).into_iter();
    let cursor = parts
        .next()
        .and_then(as_string)
        .ok_or_else(|| AppError::new("Unexpected SCAN reply from Redis."))?;
    let items = parts.next().map(as_items).unwrap_or_default();
    Ok((cursor, items))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_round_trips_text_and_binary() {
        let text = Blob::text("users:1");
        let json = serde_json::to_string(&text).unwrap();
        assert_eq!(json, r#"{"text":"users:1"}"#);
        assert_eq!(serde_json::from_str::<Blob>(&json).unwrap(), text);

        let binary = Blob(vec![0xff, 0x00, 0x41]);
        let json = serde_json::to_string(&binary).unwrap();
        assert_eq!(json, r#"{"base64":"/wBB"}"#);
        assert_eq!(serde_json::from_str::<Blob>(&json).unwrap(), binary);
        assert!(serde_json::from_str::<Blob>("{}").is_err());
    }

    #[test]
    fn pairs_accept_resp2_and_resp3() {
        let resp2 = Value::Array(vec![
            Value::BulkString(b"a".to_vec()),
            Value::Int(1),
            Value::BulkString(b"b".to_vec()),
            Value::Int(2),
        ]);
        assert_eq!(as_pairs(resp2).len(), 2);
        let resp3 = Value::Map(vec![(Value::SimpleString("a".into()), Value::Int(1))]);
        assert_eq!(as_pairs(resp3).len(), 1);
        assert_eq!(as_f64(&Value::BulkString(b"-inf".to_vec())), Some(f64::NEG_INFINITY));
        assert_eq!(RedisType::parse("ReJSON-RL"), RedisType::Json);
        assert_eq!(RedisType::Json.server_name(), "ReJSON-RL");
    }
}
