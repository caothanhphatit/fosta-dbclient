//! Console sessions (language `redis`): one command per line, as in redis-cli. A session owns a connection, so `SELECT`,
//! `MULTI` and `CLIENT SETNAME` stay in its console tab. Each line is one result set:
//!
//! - an array or set reply: columns `#` and `value`, one row per element;
//! - a map reply (`HGETALL`, `CONFIG GET` on RESP3): columns `field` and `value`;
//! - anything else: one `reply` cell (`Nil` is NULL, integers are `Int`, binary bulk strings are `Bytes`).
//!
//! Nested replies are shown as redis-cli text in their cell. A server error reply fails the run with `ENGINE_ERROR`, the
//! server's error name (`WRONGTYPE`, `ERR`) as the vendor code and the failing line as position and statement; the lines
//! before it keep their results. Stop cancels with `CLIENT UNBLOCK` (blocking commands), falling back to killing the
//! session's connection, which the next run replaces.

use std::sync::{
    atomic::{AtomicBool, AtomicI64, Ordering},
    Arc,
};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::sync::Mutex;

use super::{
    connection::RedisEngine,
    console::{split_lines, RedisConsole, RedisReply},
    types::as_i64,
};
use crate::{
    contract::{Batcher, Canceller, ColumnInfo, EngineError, ResultSink, RunOptions, RunRequest, Session, StreamEvent, Value, ValueKind},
    error::{AppError, AppResult, ErrorPosition},
};

pub const LANGUAGE: &str = "redis";

/// State the session and its canceller share.
#[derive(Default)]
struct Shared {
    /// `CLIENT ID` of the session's connection (0 until known).
    client_id: AtomicI64,
    /// The cancel killed the connection: the next run opens a new one.
    killed: AtomicBool,
    /// A cancel was requested during the current run, so its failure reads as "cancelled".
    cancelled: AtomicBool,
}

pub struct RedisSession {
    engine: RedisEngine,
    console: Mutex<RedisConsole>,
    shared: Arc<Shared>,
}

impl RedisSession {
    pub async fn open(engine: RedisEngine) -> AppResult<(Self, Arc<dyn Canceller>)> {
        let console = engine.open_console().await?;
        let shared = Arc::new(Shared::default());
        shared.client_id.store(console.client_id().unwrap_or(0), Ordering::SeqCst);
        let canceller = Arc::new(RedisCanceller {
            control: engine.clone(),
            shared: Arc::clone(&shared),
        });
        Ok((
            Self {
                engine,
                console: Mutex::new(console),
                shared,
            },
            canceller,
        ))
    }
}

struct RedisCanceller {
    /// The shared connection of the database, never the session's own (the run is using that one).
    control: RedisEngine,
    shared: Arc<Shared>,
}

impl Canceller for RedisCanceller {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>> {
        let control = self.control.clone();
        let shared = Arc::clone(&self.shared);
        Box::pin(async move {
            let id = shared.client_id.load(Ordering::SeqCst);
            if id == 0 {
                return Err(AppError::new(
                    "This connection cannot be stopped: the server did not report its client id.",
                ));
            }
            shared.cancelled.store(true, Ordering::SeqCst);
            let unblocked = control
                .query(redis::cmd("CLIENT").arg("UNBLOCK").arg(id).arg("ERROR"))
                .await
                .ok()
                .and_then(|reply| as_i64(&reply))
                .unwrap_or(0);
            if unblocked == 0 {
                // Not blocked in a blocking command (a long script, DEBUG SLEEP): drop the connection.
                shared.killed.store(true, Ordering::SeqCst);
                control.query(redis::cmd("CLIENT").arg("KILL").arg("ID").arg(id)).await?;
            }
            Ok(())
        })
    }
}

fn cell(reply: &RedisReply) -> Value {
    match reply {
        RedisReply::Nil => Value::Null,
        RedisReply::Integer { value } => Value::Int(*value),
        RedisReply::Bulk { value } => match value.as_str() {
            Some(text) => Value::Text(text.to_owned()),
            None => Value::Bytes(value.0.clone()),
        },
        RedisReply::Simple { value } => Value::Text(value.clone()),
        RedisReply::Ok => Value::Text("OK".into()),
        RedisReply::Double { value } => Value::Float(*value),
        RedisReply::Boolean { value } => Value::Bool(*value),
        RedisReply::BigNumber { value } => Value::Decimal(value.clone()),
        RedisReply::Verbatim { text, .. } => Value::Text(text.clone()),
        nested => Value::Text(nested.to_cli_text()),
    }
}

/// The columns, rows and whether the reply was cut by the console's own element limit.
fn tabulate(reply: &RedisReply) -> (Vec<ColumnInfo>, Vec<Vec<Value>>, bool) {
    let column = |name: &str, kind| ColumnInfo::result(name, "reply", kind);
    match reply {
        RedisReply::Array { items } | RedisReply::Set { items } | RedisReply::Push { items, .. } => {
            let cut = matches!(items.last(), Some(RedisReply::Truncated { .. }));
            let rows = items
                .iter()
                .filter(|item| !matches!(item, RedisReply::Truncated { .. }))
                .enumerate()
                .map(|(index, item)| vec![Value::Int(index as i64 + 1), cell(item)])
                .collect();
            (vec![column("#", ValueKind::Int), column("value", ValueKind::Other)], rows, cut)
        }
        RedisReply::Map { entries } => (
            vec![column("field", ValueKind::Other), column("value", ValueKind::Other)],
            entries.iter().map(|(key, value)| vec![cell(key), cell(value)]).collect(),
            false,
        ),
        scalar => (vec![column("reply", ValueKind::Other)], vec![vec![cell(scalar)]], false),
    }
}

/// A server error reply as the structured error: the server's error name is the vendor code.
fn reply_error(code: &str, message: Option<&str>) -> AppError {
    let message = message.filter(|text| !text.is_empty()).unwrap_or("The server returned an error");
    AppError::from(EngineError::server(message, Some(code.to_owned()))).with_engine(super::descriptor::ID)
}

#[async_trait]
impl Session for RedisSession {
    fn language(&self) -> &str {
        LANGUAGE
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        let lines = split_lines(&request.text);
        if lines.is_empty() {
            return Err(AppError::new("Type a command, for example PING or GET user:1."));
        }
        let mut console = self.console.lock().await;
        self.shared.cancelled.store(false, Ordering::SeqCst);
        if self.shared.killed.swap(false, Ordering::SeqCst) {
            *console = self.engine.open_console().await?;
            self.shared.client_id.store(console.client_id().unwrap_or(0), Ordering::SeqCst);
        }
        if let Some(timeout) = options.timeout {
            console.set_timeout(timeout);
        }
        for (index, (line_number, line)) in lines.into_iter().enumerate() {
            sink.event(StreamEvent::Statement {
                index,
                sql: line.to_owned(),
            });
            let started = std::time::Instant::now();
            let failed = |error: AppError, cancelled: bool| {
                let error = if cancelled {
                    AppError::from(EngineError::cancelled())
                } else {
                    error
                };
                error
                    .with_statement(index, line)
                    .with_position(ErrorPosition::line(line_number as u32))
            };
            let reply = match console.exec_line(line).await {
                Ok((_, RedisReply::Error { code, message })) => {
                    let cancelled = code == "UNBLOCKED" && self.shared.cancelled.load(Ordering::SeqCst);
                    return Err(failed(reply_error(&code, message.as_deref()), cancelled));
                }
                Ok((_, reply)) => reply,
                Err(error) => {
                    let cancelled = self.shared.cancelled.load(Ordering::SeqCst);
                    return Err(failed(error, cancelled));
                }
            };
            let (columns, rows, cut) = tabulate(&reply);
            sink.event(StreamEvent::Columns { index, columns });
            let mut batcher = Batcher::new(index, options);
            let total = rows.len();
            for row in rows {
                if !batcher.push(row, sink) {
                    break;
                }
            }
            batcher.flush(sink);
            let sink_stopped = batcher.stopped && batcher.kept < options.row_limit;
            sink.event(StreamEvent::Done {
                index,
                affected: None,
                rows: batcher.kept,
                truncated: cut || batcher.kept < total,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
            if sink_stopped {
                break;
            }
        }
        Ok(())
    }

    async fn close(&self) {
        // The connection closes when the session is dropped; nothing is held on the server beyond it.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::redis::types::Blob;

    #[test]
    fn replies_become_typed_cells() {
        assert_eq!(cell(&RedisReply::Nil), Value::Null);
        assert_eq!(cell(&RedisReply::Integer { value: 7 }), Value::Int(7));
        assert_eq!(cell(&RedisReply::Bulk { value: Blob::text("a") }), Value::Text("a".into()));
        assert_eq!(
            cell(&RedisReply::Bulk {
                value: Blob(vec![0xff, 0x00])
            }),
            Value::Bytes(vec![0xff, 0x00])
        );
        assert_eq!(cell(&RedisReply::Ok), Value::Text("OK".into()));
        assert_eq!(cell(&RedisReply::Double { value: 1.5 }), Value::Float(1.5));
        let nested = RedisReply::Array {
            items: vec![RedisReply::Integer { value: 1 }, RedisReply::Nil],
        };
        assert_eq!(cell(&nested), Value::Text("1) (integer) 1\n2) (nil)".into()));
    }

    #[test]
    fn arrays_maps_and_scalars_have_their_own_shape() {
        let array = RedisReply::Array {
            items: vec![
                RedisReply::Bulk { value: Blob::text("a") },
                RedisReply::Bulk { value: Blob::text("b") },
                RedisReply::Truncated { omitted: 5 },
            ],
        };
        let (columns, rows, cut) = tabulate(&array);
        assert_eq!(columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["#", "value"]);
        assert_eq!(
            rows,
            vec![
                vec![Value::Int(1), Value::Text("a".into())],
                vec![Value::Int(2), Value::Text("b".into())]
            ]
        );
        assert!(cut);
        let map = RedisReply::Map {
            entries: vec![(RedisReply::Simple { value: "k".into() }, RedisReply::Integer { value: 1 })],
        };
        let (columns, rows, cut) = tabulate(&map);
        assert_eq!((columns[0].name.as_str(), rows.len(), cut), ("field", 1, false));
        let (columns, rows, _) = tabulate(&RedisReply::Integer { value: 3 });
        assert_eq!((columns[0].name.as_str(), rows), ("reply", vec![vec![Value::Int(3)]]));
    }

    #[test]
    fn server_errors_keep_their_name_as_the_vendor_code() {
        let error = reply_error("WRONGTYPE", Some("Operation against a key holding the wrong kind of value"));
        assert_eq!(error.code(), Some("ENGINE_ERROR"));
        let detail = error.payload().unwrap();
        assert_eq!(detail["vendorCode"], "WRONGTYPE");
        assert_eq!(detail["engine"], "redis");
        assert_eq!(reply_error("ERR", None).message(), "The server returned an error (ERR)");
    }
}
