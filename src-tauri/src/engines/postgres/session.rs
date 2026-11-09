//! A PostgreSQL query session.
//!
//! - Plain queries (SELECT / WITH / VALUES / TABLE) run through a server-side cursor (`DECLARE … NO SCROLL CURSOR`, `FETCH n`):
//!   rows arrive in batches, and stopping at the row limit closes the cursor, so the server never computes or sends the rest.
//!   Outside an explicit transaction the cursor lives in a short transaction of its own.
//! - Other statements run through the extended protocol; rows they return (`RETURNING`, `SHOW`, `EXPLAIN`) beyond the limit are
//!   read and dropped, since stopping a data change half way is not an option.
//! - Cancel sends PostgreSQL's CancelRequest on a separate connection (the session's CancelToken).
//! - The statement timeout is the server's `statement_timeout`, set when it changes.
//! - The transaction state is asked from the server after each run: in a transaction block `now()` (the transaction start)
//!   differs from `statement_timestamp()`; a failed block rejects the probe with SQLSTATE 25P02.

use super::{map_err, types};
use crate::engines::common::split::{self, Dialect};
use crate::engines::common::{
    classify, Batcher, ColumnInfo, EngineError, ErrorKind, Flow, Result, ResultSink, RunOptions, StatementKind, StreamEvent, TxState,
};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio_postgres::{CancelToken, Client, SimpleQueryMessage};
use tokio_postgres_rustls::MakeRustlsConnect;

const CURSOR: &str = "fosta_cursor";

pub struct PgSession {
    client: Client,
    cancel: PgCancel,
    state: Mutex<TxState>,
    timeout: Mutex<Option<Duration>>,
    /// The read-only default last applied (`None` = not yet).
    read_only: Mutex<Option<bool>>,
}

/// A statement of a script failed: the error, which statement it was and where it starts in the script.
#[derive(Debug)]
pub struct RunFailure {
    pub error: crate::engines::common::EngineError,
    pub index: usize,
    pub text: String,
    /// Byte offset of the statement in the script.
    pub start: usize,
}

/// Cancels the statement a session is running.
#[derive(Clone)]
pub struct PgCancel {
    token: CancelToken,
    tls: MakeRustlsConnect,
}

impl PgCancel {
    pub async fn cancel(&self) -> Result<()> {
        self.token.cancel_query(self.tls.clone()).await.map_err(map_err)
    }
}

impl PgSession {
    pub(super) fn new(client: Client, tls: MakeRustlsConnect) -> Self {
        let token = client.cancel_token();
        Self {
            client,
            cancel: PgCancel { token, tls },
            state: Mutex::new(TxState::Idle),
            timeout: Mutex::new(None),
            read_only: Mutex::new(None),
        }
    }

    pub fn canceller(&self) -> PgCancel {
        self.cancel.clone()
    }

    pub fn transaction_state(&self) -> TxState {
        *self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub async fn begin(&self) -> Result<()> {
        let r = self.client.batch_execute("BEGIN").await.map_err(map_err);
        self.refresh_state().await;
        r
    }
    pub async fn commit(&self) -> Result<()> {
        let r = self.client.batch_execute("COMMIT").await.map_err(map_err);
        self.refresh_state().await;
        r
    }
    pub async fn rollback(&self) -> Result<()> {
        let r = self.client.batch_execute("ROLLBACK").await.map_err(map_err);
        self.refresh_state().await;
        r
    }

    /// Asks the server whether a transaction block is open (see the module docs).
    pub async fn refresh_state(&self) -> TxState {
        let state = match self.client.simple_query("SELECT now() <> statement_timestamp()").await {
            Ok(msgs) => msgs
                .iter()
                .find_map(|m| match m {
                    SimpleQueryMessage::Row(r) => r.get(0).map(|v| if v == "t" { TxState::Active } else { TxState::Idle }),
                    _ => None,
                })
                .unwrap_or(TxState::Idle),
            Err(e) if e.code().map(|c| c.code()) == Some("25P02") => TxState::Failed,
            Err(_) => TxState::Idle,
        };
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = state;
        state
    }

    async fn apply_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        let current = *self.timeout.lock().unwrap_or_else(|p| p.into_inner());
        if current != timeout {
            let ms = timeout.map_or(0, |d| d.as_millis().max(1));
            self.client
                .batch_execute(&format!("SET statement_timeout = {ms}"))
                .await
                .map_err(map_err)?;
            *self.timeout.lock().unwrap_or_else(|p| p.into_inner()) = timeout;
        }
        Ok(())
    }

    /// Runs a script statement by statement, streaming each result set. Stops at the first error.
    pub async fn run(&self, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
        self.run_detailed(sql, opts, sink).await.map_err(|failure| failure.error)
    }

    /// [`run`](Self::run) that says which statement failed (index, text and its start in the script).
    pub async fn run_detailed(&self, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> std::result::Result<(), Box<RunFailure>> {
        let before = |error| {
            Box::new(RunFailure {
                error,
                index: 0,
                text: String::new(),
                start: 0,
            })
        };
        self.apply_limits(opts).await.map_err(before)?;
        let mut result = Ok(());
        for (index, st) in split::split(Dialect::Postgres, sql).into_iter().enumerate() {
            sink.event(StreamEvent::Statement {
                index,
                sql: st.text.clone(),
            });
            let started = Instant::now();
            let r = if is_cursor_query(&st.text) {
                self.run_cursor(index, &st.text, opts, sink, started).await
            } else {
                self.run_direct(index, &st.text, opts, sink, started).await
            };
            if let Err(error) = r {
                result = Err(Box::new(RunFailure {
                    error,
                    index,
                    text: st.text.clone(),
                    start: st.start,
                }));
                break;
            }
        }
        self.refresh_state().await;
        result
    }

    /// Applies the rules' statement timeout and read-only backstop (a failed transaction block rejects every `SET`).
    pub async fn apply_limits(&self, opts: &RunOptions) -> Result<()> {
        if self.transaction_state() != TxState::Failed {
            self.apply_timeout(opts.timeout).await?;
            self.apply_read_only(opts.read_only).await?;
        }
        Ok(())
    }

    /// Sets the session's default for new transactions (the rules' read-only backstop).
    ///
    /// A read-only run never trusts what was applied before: a statement of an earlier run may have changed the default
    /// (`SET default_transaction_read_only`, `RESET ALL`, `DISCARD ALL`, `SET SESSION CHARACTERISTICS`), so the default is set
    /// again before every read-only run, and a read-write transaction left open (`BEGIN READ WRITE`, or one begun under
    /// other rules) refuses the run until it is committed or rolled back.
    async fn apply_read_only(&self, read_only: bool) -> Result<()> {
        if read_only {
            // Two round trips: in one query string both statements would share an implicit transaction begun before the SET.
            self.client
                .batch_execute("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
                .await
                .map_err(map_err)?;
            *self.read_only.lock().unwrap_or_else(|p| p.into_inner()) = Some(true);
            let messages = self.client.simple_query("SHOW transaction_read_only").await.map_err(map_err)?;
            let current = messages.iter().find_map(|m| match m {
                SimpleQueryMessage::Row(r) => r.get(0).map(str::to_owned),
                _ => None,
            });
            if current.as_deref() != Some("on") {
                return Err(EngineError::new(
                    ErrorKind::Input,
                    "A read-write transaction is open, and this connection's rules only allow reading. Commit or roll it back first.",
                ));
            }
            return Ok(());
        }
        let current = *self.read_only.lock().unwrap_or_else(|p| p.into_inner());
        if current != Some(read_only) {
            let mode = if read_only { "READ ONLY" } else { "READ WRITE" };
            self.client
                .batch_execute(&format!("SET SESSION CHARACTERISTICS AS TRANSACTION {mode}"))
                .await
                .map_err(map_err)?;
            *self.read_only.lock().unwrap_or_else(|p| p.into_inner()) = Some(read_only);
        }
        Ok(())
    }

    async fn run_cursor(&self, index: usize, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink, started: Instant) -> Result<()> {
        let own_tx = self.refresh_state().await == TxState::Idle;
        if own_tx {
            self.client.batch_execute("BEGIN").await.map_err(map_err)?;
        }
        let r = self.fetch_cursor(index, sql, opts, sink, started).await;
        match (&r, own_tx) {
            (Ok(_), true) => self.client.batch_execute("COMMIT").await.map_err(map_err)?,
            (Err(_), true) => {
                let _ = self.client.batch_execute("ROLLBACK").await;
            }
            _ => {}
        }
        r
    }

    async fn fetch_cursor(&self, index: usize, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink, started: Instant) -> Result<()> {
        let declare = format!("DECLARE {CURSOR} NO SCROLL CURSOR FOR {sql}");
        // The server reports a position in the whole DECLARE text; the statement starts after the prefix.
        let prefix = (declare.chars().count() - sql.chars().count()) as u32;
        self.client.batch_execute(&declare).await.map_err(|error| {
            let mut error = map_err(error);
            error.position = error.position.map(|position| position.saturating_sub(prefix));
            error
        })?;
        let batch = opts.batch_size.max(1);
        let fetch = self
            .client
            .prepare(&format!("FETCH FORWARD {batch} FROM {CURSOR}"))
            .await
            .map_err(map_err)?;
        let columns = result_columns(fetch.columns());
        let mut batcher = Batcher::new(index, opts);
        let mut truncated = false;
        if sink.event(StreamEvent::Columns { index, columns }) == Flow::Continue {
            loop {
                let rows = self.client.query(&fetch, &[]).await.map_err(map_err)?;
                let full = rows.len() == batch;
                let mut stopped = false;
                for (i, row) in rows.iter().enumerate() {
                    if !batcher.push(types::row_values(row), sink) {
                        stopped = true;
                        truncated = i + 1 < rows.len() || full;
                        break;
                    }
                }
                if stopped || !full {
                    break;
                }
            }
            batcher.flush(sink);
        }
        self.client.batch_execute(&format!("CLOSE {CURSOR}")).await.map_err(map_err)?;
        sink.event(StreamEvent::Done {
            index,
            affected: None,
            rows: batcher.kept,
            truncated,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
        Ok(())
    }

    async fn run_direct(&self, index: usize, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink, started: Instant) -> Result<()> {
        use futures_util::{pin_mut, StreamExt};
        let stmt = self.client.prepare(sql).await.map_err(map_err)?;
        if stmt.columns().is_empty() {
            let n = self.client.execute(&stmt, &[]).await.map_err(map_err)?;
            sink.event(StreamEvent::Done {
                index,
                affected: Some(n),
                rows: 0,
                truncated: false,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
            return Ok(());
        }
        let columns = result_columns(stmt.columns());
        let wants_rows = sink.event(StreamEvent::Columns { index, columns }) == Flow::Continue;
        let stream = self
            .client
            .query_raw(&stmt, std::iter::empty::<&(dyn tokio_postgres::types::ToSql + Sync)>())
            .await
            .map_err(map_err)?;
        pin_mut!(stream);
        let mut batcher = Batcher::new(index, opts);
        let mut truncated = false;
        let mut reading = wants_rows;
        while let Some(row) = stream.next().await {
            let row = row.map_err(map_err)?;
            if reading {
                reading = batcher.push(types::row_values(&row), sink);
            } else {
                truncated = true; // drained: the statement must finish (it may be changing rows)
            }
        }
        batcher.flush(sink);
        let affected = stream.rows_affected();
        let is_read = classify(Dialect::Postgres, sql).kind == StatementKind::Read;
        sink.event(StreamEvent::Done {
            index,
            affected: if is_read { None } else { affected },
            rows: batcher.kept,
            truncated,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
        Ok(())
    }
}

fn result_columns(cols: &[tokio_postgres::Column]) -> Vec<ColumnInfo> {
    cols.iter()
        .map(|c| ColumnInfo::result(c.name(), c.type_().name(), types::kind_of(c.type_())))
        .collect()
}

/// A read-only query a cursor can run: SELECT/WITH/VALUES/TABLE that the classifier calls Read (so no data-modifying CTE and
/// no SELECT INTO).
pub(crate) fn is_cursor_query(sql: &str) -> bool {
    let c = classify(Dialect::Postgres, sql);
    let first = split::words(Dialect::Postgres, sql).into_iter().next().unwrap_or_default();
    c.kind == StatementKind::Read && matches!(first.as_str(), "SELECT" | "WITH" | "VALUES" | "TABLE")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_eligibility() {
        assert!(is_cursor_query("SELECT * FROM t"));
        assert!(is_cursor_query("with x as (select 1) select * from x"));
        assert!(is_cursor_query("VALUES (1), (2)"));
        assert!(!is_cursor_query("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d"));
        assert!(!is_cursor_query("SELECT * INTO t2 FROM t"));
        assert!(!is_cursor_query("SHOW search_path"));
        assert!(!is_cursor_query("INSERT INTO t VALUES (1) RETURNING id"));
    }
}
