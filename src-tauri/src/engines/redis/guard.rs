//! Optimistic concurrency for edits. An edit may carry `expect`: what the user saw of the part of the key it changes. The
//! guarded apply runs on its own connection (WATCH state belongs to a connection, so it must not be shared):
//!
//! 1. `WATCH key`, then read the same part of the key and compare it with `expect`;
//! 2. different: `UNWATCH` and fail with `CONFLICT` carrying the current value, nothing is written;
//! 3. equal: `MULTI`, the edit, `EXEC`. Redis aborts the transaction when the key changed after the WATCH (the reply is
//!    nil), which is also a `CONFLICT`.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    connection::{with_timeout, RedisEngine},
    edit::{EditResult, RedisEdit},
    types::{as_bytes, as_f64, as_i64, as_items, check, Blob},
};
use crate::{
    contract::errors::CONFLICT,
    error::{AppError, AppResult},
};

/// A hash field as the user saw it; `value: None` means the field did not exist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldExpect {
    pub field: Blob,
    #[serde(default)]
    pub value: Option<Blob>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberExpect {
    pub member: Blob,
    pub present: bool,
}

/// A sorted set member's score as the user saw it; `score: None` means the member did not exist.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreExpect {
    pub member: Blob,
    #[serde(default)]
    pub score: Option<f64>,
}

/// The part of a key an edit depends on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Expect {
    /// The whole string value; `None` means the key did not exist.
    String {
        value: Option<Blob>,
    },
    Hash {
        fields: Vec<FieldExpect>,
    },
    /// The element at `index`; `None` means the list was shorter.
    ListItem {
        index: i64,
        value: Option<Blob>,
    },
    Set {
        members: Vec<MemberExpect>,
    },
    Zset {
        members: Vec<ScoreExpect>,
    },
    /// The key did not exist (creating a key must not overwrite one made meanwhile).
    Missing,
    /// What a `Missing` expectation finds when the key exists now (never sent by the UI).
    Exists,
}

fn blob(value: &redis::Value) -> Option<Blob> {
    as_bytes(value.clone()).map(Blob)
}

impl Expect {
    /// Reads the same part of the key as it is now, in the same shape.
    async fn observe(&self, engine: &RedisEngine, conn: &mut redis::aio::MultiplexedConnection, key: &Blob) -> AppResult<Expect> {
        let mut pipe = redis::pipe();
        match self {
            Expect::String { .. } => {
                pipe.cmd("GET").arg(key);
            }
            Expect::Hash { fields } => {
                for item in fields {
                    pipe.cmd("HGET").arg(key).arg(&item.field);
                }
            }
            Expect::ListItem { index, .. } => {
                pipe.cmd("LINDEX").arg(key).arg(*index);
            }
            Expect::Set { members } => {
                for item in members {
                    pipe.cmd("SISMEMBER").arg(key).arg(&item.member);
                }
            }
            Expect::Zset { members } => {
                for item in members {
                    pipe.cmd("ZSCORE").arg(key).arg(&item.member);
                }
            }
            Expect::Missing | Expect::Exists => {
                pipe.cmd("EXISTS").arg(key);
            }
        }
        pipe.ignore_errors();
        let replies: Vec<redis::Value> = with_timeout(engine.command_timeout(), pipe.query_async(conn)).await?;
        let replies = replies.into_iter().map(check).collect::<AppResult<Vec<_>>>()?;
        let first = replies.first().cloned().unwrap_or(redis::Value::Nil);
        Ok(match self {
            Expect::String { .. } => Expect::String { value: blob(&first) },
            Expect::Hash { fields } => Expect::Hash {
                fields: fields
                    .iter()
                    .zip(&replies)
                    .map(|(item, reply)| FieldExpect {
                        field: item.field.clone(),
                        value: blob(reply),
                    })
                    .collect(),
            },
            Expect::ListItem { index, .. } => Expect::ListItem {
                index: *index,
                value: blob(&first),
            },
            Expect::Set { members } => Expect::Set {
                members: members
                    .iter()
                    .zip(&replies)
                    .map(|(item, reply)| MemberExpect {
                        member: item.member.clone(),
                        present: as_i64(reply) == Some(1) || matches!(reply, redis::Value::Boolean(true)),
                    })
                    .collect(),
            },
            Expect::Zset { members } => Expect::Zset {
                members: members
                    .iter()
                    .zip(&replies)
                    .map(|(item, reply)| ScoreExpect {
                        member: item.member.clone(),
                        score: as_f64(reply),
                    })
                    .collect(),
            },
            Expect::Missing | Expect::Exists => {
                if as_i64(&first).unwrap_or(0) == 0 {
                    Expect::Missing
                } else {
                    Expect::Exists
                }
            }
        })
    }
}

fn conflict(key: &Blob, expected: &Expect, current: Option<&Expect>, why: &str) -> AppError {
    AppError::structured(
        format!("Nothing was saved: {} {why} since you loaded it.", describe_key(key)),
        json!({ "code": CONFLICT, "key": key, "expected": expected, "current": current }),
    )
}

fn describe_key(key: &Blob) -> String {
    format!("`{}`", key.display())
}

/// Applies `edit` only when `expect` still holds. `CONFLICT` (nothing written) otherwise.
pub async fn apply_guarded(engine: &RedisEngine, edit: &RedisEdit, expect: &Expect) -> AppResult<EditResult> {
    apply_guarded_after(engine, edit, expect, async {}).await
}

/// [`apply_guarded`], running `between` after the comparison and before `EXEC`: tests change the key there to prove that
/// WATCH aborts the transaction.
pub(crate) async fn apply_guarded_after(
    engine: &RedisEngine,
    edit: &RedisEdit,
    expect: &Expect,
    between: impl std::future::Future<Output = ()>,
) -> AppResult<EditResult> {
    let key = edit
        .key()
        .ok_or_else(|| AppError::new("A guarded edit changes one key; deleting several keys cannot carry `expect`."))?;
    let command = edit.command()?;
    let mut conn = engine.new_connection().await?;
    with_timeout(
        engine.command_timeout(),
        redis::cmd("WATCH").arg(key).query_async::<redis::Value>(&mut conn),
    )
    .await?;
    let outcome = async {
        let observed = expect.observe(engine, &mut conn, key).await?;
        if observed != *expect {
            return Err(conflict(key, expect, Some(&observed), "was changed by someone else"));
        }
        between.await;
        let mut pipe = redis::pipe();
        pipe.atomic().add_command(command);
        let reply = with_timeout(engine.command_timeout(), pipe.query_async::<redis::Value>(&mut conn)).await?;
        match reply {
            redis::Value::Nil => Err(conflict(key, expect, None, "was changed by someone else while it was being saved")),
            reply => Ok(reply),
        }
    }
    .await;
    if outcome.is_err() {
        // The connection is dropped right after, which also clears the WATCH; this keeps it tidy for a reused pool later.
        let _ = redis::cmd("UNWATCH").query_async::<redis::Value>(&mut conn).await;
    }
    let reply = outcome?;
    let first = as_items(reply).into_iter().next().unwrap_or(redis::Value::Nil);
    let first = check(first)?;
    let count = as_i64(&first);
    let applied = match edit {
        RedisEdit::SetString { .. } => first != redis::Value::Nil,
        RedisEdit::Rename { overwrite: false, .. } | RedisEdit::Expire { .. } | RedisEdit::Persist { .. } => count != Some(0),
        _ => true,
    };
    Ok(EditResult { count, applied })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expectations_have_a_tagged_wire_form() {
        let expect: Expect = serde_json::from_value(json!({
            "kind": "hash", "fields": [ { "field": { "text": "name" }, "value": { "text": "Ada" } }, { "field": { "text": "new" } } ]
        }))
        .unwrap();
        let Expect::Hash { fields } = &expect else { panic!("hash") };
        assert_eq!(fields[0].value, Some(Blob::text("Ada")));
        assert_eq!(fields[1].value, None);
        let string: Expect = serde_json::from_value(json!({ "kind": "string", "value": null })).unwrap();
        assert_eq!(string, Expect::String { value: None });
        assert!(serde_json::from_value::<Expect>(json!({ "kind": "teleport" })).is_err());
    }

    #[test]
    fn a_conflict_is_structured_and_names_the_key() {
        let expected = Expect::String {
            value: Some(Blob::text("a")),
        };
        let current = Expect::String {
            value: Some(Blob::text("b")),
        };
        let error = conflict(&Blob::text("user:1"), &expected, Some(&current), "was changed by someone else");
        assert_eq!(error.code(), Some(CONFLICT));
        assert!(error.to_string().contains("`user:1`"), "{error}");
        let detail = error.payload().unwrap();
        assert_eq!(detail["current"]["value"]["text"], "b");
        assert_eq!(detail["expected"]["value"]["text"], "a");
    }
}
