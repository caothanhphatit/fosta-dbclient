//! A SQLite query session: its own connection, streaming results, interrupt-based cancel and timeout, transaction state from
//! `sqlite3_get_autocommit`.

use super::{lock, map_err, row_values, types};
use crate::engines::common::split::{self, Dialect};
use crate::engines::common::ColumnInfo;
use crate::engines::common::{
    run_blocking, Batcher, EngineError, ErrorKind, Flow, Result, ResultSink, RunOptions, StreamEvent, TxState, ValueKind,
};
use rusqlite::{Connection, InterruptHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct SqliteSession {
    conn: Arc<Mutex<Connection>>,
    cancel: SqliteCancel,
}

/// A statement of a script failed: the error, which statement it was and where it starts in the script.
#[derive(Debug)]
pub struct RunFailure {
    pub error: EngineError,
    pub index: usize,
    pub text: String,
    /// Byte offset of the statement in the script.
    pub start: usize,
}

/// Cancels the statement a session is running, from any thread.
#[derive(Clone)]
pub struct SqliteCancel {
    handle: Arc<InterruptHandle>,
    cancelled: Arc<AtomicBool>,
}

impl SqliteCancel {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.handle.interrupt();
    }
}

impl SqliteSession {
    pub(super) fn new(conn: Connection) -> Self {
        let handle = Arc::new(conn.get_interrupt_handle());
        Self {
            conn: Arc::new(Mutex::new(conn)),
            cancel: SqliteCancel {
                handle,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
        }
    }

    pub fn canceller(&self) -> SqliteCancel {
        self.cancel.clone()
    }

    /// Runs a script statement by statement, streaming each result set. Stops at the first error.
    pub async fn run(&self, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
        self.run_detailed(sql, opts, sink).await.map_err(|failure| failure.error)
    }

    /// [`run`](Self::run) that says which statement failed (index, text and its start in the script).
    pub async fn run_detailed(&self, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> std::result::Result<(), Box<RunFailure>> {
        self.cancel.cancelled.store(false, Ordering::SeqCst);
        let failed: Arc<Mutex<Option<(usize, String, usize)>>> = Arc::new(Mutex::new(None));
        let failed_w = failed.clone();
        let statements = split::split(Dialect::Sqlite, sql);
        let conn = self.conn.clone();
        let opts2 = opts.clone();
        // Watchdog: the worker records when each statement started; past the timeout the statement is interrupted.
        let started: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let timed_out = Arc::new(AtomicBool::new(false));
        let watchdog = opts.timeout.map(|limit| {
            let (started, timed_out, handle) = (started.clone(), timed_out.clone(), self.cancel.handle.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let over = started.lock().ok().and_then(|s| *s).is_some_and(|t| t.elapsed() >= limit);
                    if over {
                        timed_out.store(true, Ordering::SeqCst);
                        handle.interrupt();
                        return;
                    }
                }
            })
        });
        let started_w = started.clone();
        let result = run_blocking(sink, move |sink| {
            let conn = lock(&conn);
            // The rules' read-only backstop: writes fail with SQLITE_READONLY.
            conn.pragma_update(None, "query_only", opts2.read_only).map_err(map_err)?;
            for (index, st) in statements.iter().enumerate() {
                if let Ok(mut s) = started_w.lock() {
                    *s = Some(Instant::now());
                }
                let r = run_statement(&conn, index, &st.text, &opts2, sink);
                if let Ok(mut s) = started_w.lock() {
                    *s = None;
                }
                if r.is_err() {
                    if let Ok(mut failure) = failed_w.lock() {
                        *failure = Some((index, st.text.clone(), st.start));
                    }
                }
                r?;
            }
            Ok(())
        })
        .await;
        if let Some(w) = watchdog {
            w.abort();
        }
        let error = match result {
            Ok(()) => return Ok(()),
            Err(e) if e.kind == ErrorKind::Cancelled && timed_out.load(Ordering::SeqCst) => {
                EngineError::timeout("The statement", opts.timeout.unwrap_or_default())
            }
            Err(e) => e,
        };
        let (index, text, start) = failed.lock().ok().and_then(|failure| failure.clone()).unwrap_or_default();
        Err(Box::new(RunFailure { error, index, text, start }))
    }

    pub async fn transaction_state(&self) -> TxState {
        let conn = self.conn.clone();
        crate::engines::common::blocking(move || {
            Ok(if lock(&conn).is_autocommit() {
                TxState::Idle
            } else {
                TxState::Active
            })
        })
        .await
        .unwrap_or(TxState::Idle)
    }

    async fn exec(&self, sql: &'static str) -> Result<()> {
        let conn = self.conn.clone();
        crate::engines::common::blocking(move || lock(&conn).execute_batch(sql).map_err(map_err)).await
    }

    pub async fn begin(&self) -> Result<()> {
        self.exec("BEGIN").await
    }
    pub async fn commit(&self) -> Result<()> {
        self.exec("COMMIT").await
    }
    pub async fn rollback(&self) -> Result<()> {
        self.exec("ROLLBACK").await
    }
}

fn run_statement(conn: &Connection, index: usize, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
    let started = Instant::now();
    sink.event(StreamEvent::Statement {
        index,
        sql: sql.to_owned(),
    });
    let mut st = conn.prepare(sql).map_err(map_err)?;
    if st.column_count() == 0 {
        st.raw_execute().map_err(map_err)?;
        let affected = conn.changes();
        sink.event(StreamEvent::Done {
            index,
            affected: Some(affected),
            rows: 0,
            truncated: false,
            elapsed_ms: elapsed(started),
        });
        return Ok(());
    }
    let columns: Vec<ColumnInfo> = st
        .columns()
        .map(|c| {
            let decl = c.decl_type().unwrap_or("");
            ColumnInfo::result(
                c.name(),
                decl,
                if decl.is_empty() { ValueKind::Other } else { types::kind_of(decl) },
            )
        })
        .collect();
    let kinds: Vec<ValueKind> = columns.iter().map(|c| c.kind.clone()).collect();
    if sink.event(StreamEvent::Columns { index, columns }) == Flow::Stop {
        return Ok(());
    }
    let mut batcher = Batcher::new(index, opts);
    let mut rows = st.raw_query();
    let mut truncated = false;
    while let Some(r) = rows.next().map_err(map_err)? {
        if !batcher.push(row_values(r, &kinds)?, sink) {
            // Finalizing the statement stops SQLite from computing the rest; one more step tells whether rows were left.
            truncated = rows.next().map_err(map_err)?.is_some();
            break;
        }
    }
    batcher.flush(sink);
    drop(rows);
    // INSERT … RETURNING changes rows too.
    let affected = (!st.readonly()).then(|| conn.changes());
    sink.event(StreamEvent::Done {
        index,
        affected,
        rows: batcher.kept,
        truncated,
        elapsed_ms: elapsed(started),
    });
    Ok(())
}

fn elapsed(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}
