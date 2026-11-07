//! Typed write operations. Each one builds a single command (or a short atomic pipeline) and has a fixed class for the
//! statement gateway, so the UI never has to send raw command text to change data.

use serde::{Deserialize, Serialize};

use super::{
    classify::CommandClass,
    connection::RedisEngine,
    types::{as_i64, check, Blob},
    values::{FieldValue, ScoredMember},
};
use crate::error::{AppError, AppResult};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetCondition {
    #[default]
    Always,
    /// Only when the key does not exist (`NX`).
    IfMissing,
    /// Only when the key exists (`XX`).
    IfExists,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ListEnd {
    Head,
    #[default]
    Tail,
}

/// A change to one key. `op` names the operation in JSON (`{ "op": "hashSet", "key": {...}, "fields": [...] }`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "op")]
pub enum RedisEdit {
    /// `SET key value [NX|XX] [PX ttl | KEEPTTL]`.
    #[serde(rename_all = "camelCase")]
    SetString {
        key: Blob,
        value: Blob,
        ttl_ms: Option<u64>,
        keep_ttl: bool,
        condition: SetCondition,
    },
    /// `PEXPIRE key ttl`.
    #[serde(rename_all = "camelCase")]
    Expire {
        key: Blob,
        ttl_ms: u64,
    },
    Persist {
        key: Blob,
    },
    /// `RENAME` (or `RENAMENX` when `overwrite` is false; fails if the new name exists).
    #[serde(rename_all = "camelCase")]
    Rename {
        key: Blob,
        new_key: Blob,
        overwrite: bool,
    },
    /// `UNLINK` (non-blocking delete) of explicit keys.
    Delete {
        keys: Vec<Blob>,
    },
    HashSet {
        key: Blob,
        fields: Vec<FieldValue>,
    },
    HashDelete {
        key: Blob,
        fields: Vec<Blob>,
    },
    ListPush {
        key: Blob,
        values: Vec<Blob>,
        end: ListEnd,
    },
    ListSet {
        key: Blob,
        index: i64,
        value: Blob,
    },
    /// `LREM key count value`: count > 0 from the head, < 0 from the tail, 0 all.
    ListRemove {
        key: Blob,
        value: Blob,
        count: i64,
    },
    SetAdd {
        key: Blob,
        members: Vec<Blob>,
    },
    SetRemove {
        key: Blob,
        members: Vec<Blob>,
    },
    /// `ZADD key score member ...` (adds or updates scores).
    ZsetAdd {
        key: Blob,
        members: Vec<ScoredMember>,
    },
    ZsetRemove {
        key: Blob,
        members: Vec<Blob>,
    },
    ZsetIncrement {
        key: Blob,
        member: Blob,
        delta: f64,
    },
    /// `XADD key [MAXLEN ~ n] id field value ...`; `id` defaults to `*`.
    #[serde(rename_all = "camelCase")]
    StreamAdd {
        key: Blob,
        id: Option<String>,
        fields: Vec<FieldValue>,
        max_len: Option<u64>,
    },
    StreamDelete {
        key: Blob,
        ids: Vec<String>,
    },
    /// `JSON.SET key path value` (RedisJSON).
    JsonSet {
        key: Blob,
        path: String,
        value: serde_json::Value,
    },
    JsonDelete {
        key: Blob,
        path: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditResult {
    /// Integer reply when the command has one (fields added, members removed, new length...).
    pub count: Option<i64>,
    /// False when a conditional write did nothing (`SET NX` on an existing key, `RENAMENX` onto an existing name).
    pub applied: bool,
}

impl RedisEdit {
    pub fn key(&self) -> Option<&Blob> {
        match self {
            Self::Delete { .. } => None,
            Self::SetString { key, .. }
            | Self::Expire { key, .. }
            | Self::Persist { key }
            | Self::Rename { key, .. }
            | Self::HashSet { key, .. }
            | Self::HashDelete { key, .. }
            | Self::ListPush { key, .. }
            | Self::ListSet { key, .. }
            | Self::ListRemove { key, .. }
            | Self::SetAdd { key, .. }
            | Self::SetRemove { key, .. }
            | Self::ZsetAdd { key, .. }
            | Self::ZsetRemove { key, .. }
            | Self::ZsetIncrement { key, .. }
            | Self::StreamAdd { key, .. }
            | Self::StreamDelete { key, .. }
            | Self::JsonSet { key, .. }
            | Self::JsonDelete { key, .. } => Some(key),
        }
    }

    /// Deleting whole keys is destructive; everything else changes data in place.
    pub fn class(&self) -> CommandClass {
        match self {
            Self::Delete { .. } => CommandClass::Destructive,
            Self::Rename { overwrite: true, .. } => CommandClass::Destructive,
            _ => CommandClass::Write,
        }
    }

    /// The command this edit runs, for logs and the gateway preview.
    pub fn command(&self) -> AppResult<redis::Cmd> {
        let empty = |items: usize, what: &str| {
            if items == 0 {
                Err(AppError::new(format!("Give at least one {what}.")))
            } else {
                Ok(())
            }
        };
        let cmd = match self {
            Self::SetString {
                key,
                value,
                ttl_ms,
                keep_ttl,
                condition,
            } => {
                let mut cmd = redis::cmd("SET");
                cmd.arg(key).arg(value);
                match condition {
                    SetCondition::Always => {}
                    SetCondition::IfMissing => {
                        cmd.arg("NX");
                    }
                    SetCondition::IfExists => {
                        cmd.arg("XX");
                    }
                }
                match (ttl_ms, keep_ttl) {
                    (Some(ttl), _) => {
                        cmd.arg("PX").arg((*ttl).max(1));
                    }
                    (None, true) => {
                        cmd.arg("KEEPTTL");
                    }
                    (None, false) => {}
                }
                cmd
            }
            Self::Expire { key, ttl_ms } => redis::cmd("PEXPIRE").arg(key).arg((*ttl_ms).max(1)).to_owned(),
            Self::Persist { key } => redis::cmd("PERSIST").arg(key).to_owned(),
            Self::Rename { key, new_key, overwrite } => {
                if new_key.0.is_empty() {
                    return Err(AppError::new("Enter the new key name."));
                }
                redis::cmd(if *overwrite { "RENAME" } else { "RENAMENX" })
                    .arg(key)
                    .arg(new_key)
                    .to_owned()
            }
            Self::Delete { keys } => {
                empty(keys.len(), "key")?;
                redis::cmd("UNLINK").arg(keys).to_owned()
            }
            Self::HashSet { key, fields } => {
                empty(fields.len(), "field")?;
                let mut cmd = redis::cmd("HSET");
                cmd.arg(key);
                for field in fields {
                    cmd.arg(&field.field).arg(&field.value);
                }
                cmd
            }
            Self::HashDelete { key, fields } => {
                empty(fields.len(), "field")?;
                redis::cmd("HDEL").arg(key).arg(fields).to_owned()
            }
            Self::ListPush { key, values, end } => {
                empty(values.len(), "value")?;
                redis::cmd(if *end == ListEnd::Head { "LPUSH" } else { "RPUSH" })
                    .arg(key)
                    .arg(values)
                    .to_owned()
            }
            Self::ListSet { key, index, value } => redis::cmd("LSET").arg(key).arg(*index).arg(value).to_owned(),
            Self::ListRemove { key, value, count } => redis::cmd("LREM").arg(key).arg(*count).arg(value).to_owned(),
            Self::SetAdd { key, members } => {
                empty(members.len(), "member")?;
                redis::cmd("SADD").arg(key).arg(members).to_owned()
            }
            Self::SetRemove { key, members } => {
                empty(members.len(), "member")?;
                redis::cmd("SREM").arg(key).arg(members).to_owned()
            }
            Self::ZsetAdd { key, members } => {
                empty(members.len(), "member")?;
                let mut cmd = redis::cmd("ZADD");
                cmd.arg(key);
                for member in members {
                    if member.score.is_nan() {
                        return Err(AppError::new("A sorted-set score must be a number."));
                    }
                    cmd.arg(format_score(member.score)).arg(&member.member);
                }
                cmd
            }
            Self::ZsetRemove { key, members } => {
                empty(members.len(), "member")?;
                redis::cmd("ZREM").arg(key).arg(members).to_owned()
            }
            Self::ZsetIncrement { key, member, delta } => redis::cmd("ZINCRBY").arg(key).arg(format_score(*delta)).arg(member).to_owned(),
            Self::StreamAdd { key, id, fields, max_len } => {
                empty(fields.len(), "field")?;
                let mut cmd = redis::cmd("XADD");
                cmd.arg(key);
                if let Some(max_len) = max_len {
                    cmd.arg("MAXLEN").arg("~").arg(*max_len);
                }
                cmd.arg(id.as_deref().filter(|id| !id.is_empty()).unwrap_or("*"));
                for field in fields {
                    cmd.arg(&field.field).arg(&field.value);
                }
                cmd
            }
            Self::StreamDelete { key, ids } => {
                empty(ids.len(), "entry id")?;
                redis::cmd("XDEL").arg(key).arg(ids).to_owned()
            }
            Self::JsonSet { key, path, value } => redis::cmd("JSON.SET")
                .arg(key)
                .arg(if path.is_empty() { "$" } else { path })
                .arg(serde_json::to_string(value)?)
                .to_owned(),
            Self::JsonDelete { key, path } => redis::cmd("JSON.DEL")
                .arg(key)
                .arg(if path.is_empty() { "$" } else { path })
                .to_owned(),
        };
        Ok(cmd)
    }
}

/// Scores as Redis expects them (`inf`, `-inf`, shortest decimal).
fn format_score(score: f64) -> String {
    if score == f64::INFINITY {
        "+inf".into()
    } else if score == f64::NEG_INFINITY {
        "-inf".into()
    } else {
        score.to_string()
    }
}

impl RedisEngine {
    pub async fn apply_edit(&self, edit: &RedisEdit) -> AppResult<EditResult> {
        let reply = check(self.query(&edit.command()?).await?)?;
        let count = as_i64(&reply);
        let applied = match edit {
            RedisEdit::SetString { .. } => reply != redis::Value::Nil,
            RedisEdit::Rename { overwrite: false, .. } | RedisEdit::Expire { .. } | RedisEdit::Persist { .. } => count != Some(0),
            _ => true,
        };
        Ok(EditResult { count, applied })
    }
}
