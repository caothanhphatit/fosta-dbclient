//! An Oracle query session: its own connection with autocommit off, streaming fetches, `break_execution` for cancel and the
//! statement timeout, implicit result sets from PL/SQL (`DBMS_SQL.RETURN_RESULT`), and the transaction state from
//! `DBMS_TRANSACTION.LOCAL_TRANSACTION_ID`.

use super::{map_err, row_values, types};
use crate::engines::common::split::{self, Dialect};
use crate::engines::common::{
    blocking, run_blocking, Batcher, ColumnInfo, EngineError, ErrorKind, Flow, Result, ResultSink, RunOptions, StreamEvent, TxState,
};
use oracle::{Connection, ResultSet, Row};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct OraSession {
    conn: Arc<Connection>,
}

/// Cancels the statement a session is running (OCIBreak on the session's connection).
#[derive(Clone)]
pub struct OraCancel {
    conn: Arc<Connection>,
}

impl OraCancel {
    pub fn cancel(&self) -> Result<()> {
        self.conn.break_execution().map_err(map_err)
    }
}

impl OraSession {
    pub(super) fn new(conn: Connection) -> Self {
        Self { conn: Arc::new(conn) }
    }

    pub fn canceller(&self) -> OraCancel {
        OraCancel { conn: self.conn.clone() }
    }

    /// Runs a script statement by statement (PL/SQL blocks end at a `/` line). Stops at the first error.
    pub async fn run(&self, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
        let statements = split::split(Dialect::Oracle, sql);
        let conn = self.conn.clone();
        let opts2 = opts.clone();
        let started: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let timed_out = Arc::new(AtomicBool::new(false));
        // Watchdog: break the running call once it exceeds the timeout. (OCI's call timeout would leave the connection unusable.)
        let watchdog = opts.timeout.map(|limit| {
            let (started, timed_out, conn) = (started.clone(), timed_out.clone(), self.conn.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    if started.lock().ok().and_then(|s| *s).is_some_and(|t| t.elapsed() >= limit) {
                        timed_out.store(true, Ordering::SeqCst);
                        let c = conn.clone();
                        let _ = tokio::task::spawn_blocking(move || c.break_execution()).await;
                        return;
                    }
                }
            })
        });
        let started_w = started.clone();
        let result = run_blocking(sink, move |sink| {
            for (index, st) in statements.iter().enumerate() {
                if let Ok(mut s) = started_w.lock() {
                    *s = Some(Instant::now());
                }
                let r = run_statement(&conn, index, &st.text, &opts2, sink);
                if let Ok(mut s) = started_w.lock() {
                    *s = None;
                }
                r?;
            }
            Ok(())
        })
        .await;
        if let Some(w) = watchdog {
            w.abort();
        }
        match result {
            Err(e) if e.kind == ErrorKind::Cancelled && timed_out.load(Ordering::SeqCst) => {
                Err(EngineError::timeout("The statement", opts.timeout.unwrap_or_default()))
            }
            other => other,
        }
    }

    pub async fn transaction_state(&self) -> TxState {
        let conn = self.conn.clone();
        blocking(move || {
            let row = conn
                .query_row("SELECT DBMS_TRANSACTION.LOCAL_TRANSACTION_ID FROM dual", &[])
                .map_err(map_err)?;
            let id: Option<String> = row.get(0).map_err(map_err)?;
            Ok(if id.is_some() { TxState::Active } else { TxState::Idle })
        })
        .await
        .unwrap_or(TxState::Idle)
    }

    /// Oracle starts a transaction with the first change; there is nothing to send.
    pub async fn begin(&self) -> Result<()> {
        Ok(())
    }
    pub async fn commit(&self) -> Result<()> {
        let conn = self.conn.clone();
        blocking(move || conn.commit().map_err(map_err)).await
    }
    pub async fn rollback(&self) -> Result<()> {
        let conn = self.conn.clone();
        blocking(move || conn.rollback().map_err(map_err)).await
    }
}

fn elapsed(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// Streams one result set; returns (rows kept, truncated).
fn stream_rows(rows: ResultSet<'_, Row>, index: usize, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<(usize, bool)> {
    let columns: Vec<ColumnInfo> = rows
        .column_info()
        .iter()
        .map(|c| {
            let mut col = ColumnInfo::result(c.name(), &c.oracle_type().to_string(), types::kind_of(c.oracle_type()));
            col.nullable = c.nullable();
            col
        })
        .collect();
    if sink.event(StreamEvent::Columns { index, columns }) == Flow::Stop {
        return Ok((0, true));
    }
    let mut batcher = Batcher::new(index, opts);
    let mut truncated = false;
    let mut rows = rows;
    while let Some(row) = rows.next() {
        let row = row.map_err(map_err)?;
        if !batcher.push(row_values(&row), sink) {
            // Dropping the result set closes the cursor; the server stops there.
            truncated = rows.next().is_some();
            break;
        }
    }
    batcher.flush(sink);
    Ok((batcher.kept, truncated))
}

/// [`map_err`] for opening a result set, with a way out for native JSON columns, which the `oracle` crate cannot fetch.
fn query_err(e: oracle::Error) -> EngineError {
    let mut err = map_err(e);
    if err.message.contains("unsupported Oracle type JSON") {
        err.kind = ErrorKind::Input;
        err.message = "This result has a JSON column, which Fosta Studio cannot read directly yet. Select it as text with \
                       JSON_SERIALIZE(column), e.g. SELECT JSON_SERIALIZE(doc PRETTY) AS doc FROM ..."
            .into();
    }
    err
}

fn run_statement(conn: &Connection, index: usize, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
    let started = Instant::now();
    sink.event(StreamEvent::Statement {
        index,
        sql: sql.to_owned(),
    });
    let batch = opts.batch_size.clamp(1, 10_000) as u32;
    let mut stmt = conn
        .statement(sql)
        .fetch_array_size(batch)
        .prefetch_rows(batch.min(opts.row_limit.max(1) as u32))
        .build()
        .map_err(map_err)?;
    if stmt.is_query() {
        let rows = stmt.query(&[]).map_err(query_err)?;
        let (kept, truncated) = stream_rows(rows, index, opts, sink)?;
        sink.event(StreamEvent::Done {
            index,
            affected: None,
            rows: kept,
            truncated,
            elapsed_ms: elapsed(started),
        });
        return Ok(());
    }
    stmt.execute(&[]).map_err(map_err)?;
    let affected = if stmt.is_dml() {
        Some(stmt.row_count().map_err(map_err)?)
    } else {
        None
    };
    sink.event(StreamEvent::Done {
        index,
        affected,
        rows: 0,
        truncated: false,
        elapsed_ms: elapsed(started),
    });
    if stmt.is_plsql() {
        // Result sets returned with DBMS_SQL.RETURN_RESULT, each announced as a statement of its own (same index).
        let mut n = 0;
        while let Some(mut cursor) = stmt.implicit_result().map_err(map_err)? {
            n += 1;
            sink.event(StreamEvent::Statement {
                index,
                sql: format!("-- implicit result {n}"),
            });
            let rows = cursor.query().map_err(query_err)?;
            let (kept, truncated) = stream_rows(rows, index, opts, sink)?;
            sink.event(StreamEvent::Done {
                index,
                affected: None,
                rows: kept,
                truncated,
                elapsed_ms: elapsed(started),
            });
        }
    }
    Ok(())
}
