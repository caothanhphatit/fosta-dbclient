//! A PostgreSQL query session.
//!
//! - Plain queries (SELECT / WITH / VALUES / TABLE) run through a server-side cursor (`DECLARE … NO SCROLL CURSOR`, then
//!   `FETCH` of one row past the limit): rows stream as they arrive, and the server never computes or sends the rest.
//!   Outside an explicit transaction the cursor lives in a short transaction of its own.
//! - Other statements run through the extended protocol; rows they return (`RETURNING`, `SHOW`, `EXPLAIN`) beyond the limit are
//!   read and dropped, since stopping a data change half way is not an option.
//! - The requests of one statement are pipelined (sent together, answers read in order), so a long script of small
//!   statements costs about one round trip per statement.
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
/// In a transaction block `now()` is the block's start, so it differs from the statement's own timestamp.
const STATE_PROBE: &str = "SELECT now() <> statement_timestamp()";

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
        self.store_state(self.client.simple_query(STATE_PROBE).await)
    }

    /// Keeps the transaction state the probe's answer tells.
    fn store_state(&self, probe: std::result::Result<Vec<SimpleQueryMessage>, tokio_postgres::Error>) -> TxState {
        let state = match probe {
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
        // A failed transaction block rejects the read-only SET, so nothing would be re-asserted before what follows a
        // ROLLBACK in the same script: a read-only run there may only roll back.
        if opts.read_only && self.transaction_state() == TxState::Failed && !is_lone_rollback(sql) {
            return Err(before(EngineError::new(
                ErrorKind::Input,
                "The transaction failed. Roll it back on its own before running anything else.",
            )));
        }
        self.apply_limits(opts).await.map_err(before)?;
        let mut result = Ok(());
        // A cursor query leaves the transaction state as it was, and any other statement asks for it along with its
        // own requests, so the server is asked on its own only before the first cursor query.
        let mut state_known = false;
        for (index, st) in split::split(Dialect::Postgres, sql).into_iter().enumerate() {
            sink.event(StreamEvent::Statement {
                index,
                sql: st.text.clone(),
            });
            let started = Instant::now();
            let r = if is_cursor_query(&st.text) {
                if !state_known {
                    self.refresh_state().await;
                    state_known = true;
                }
                self.run_cursor(index, &st.text, opts, sink, started).await
            } else {
                state_known = true;
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

    /// A cursor query in one round trip: `[BEGIN;] DECLARE`, `FETCH FORWARD limit+1` and `COMMIT` (or `CLOSE` inside the
    /// user's transaction) are sent together and their answers read in order. The FETCH streams, so rows reach the sink as
    /// they arrive, and the server computes at most one row past the cap. The columns come with the first row; an empty
    /// result describes the query afterwards. After a failure the later requests fail too (their errors are ignored), and
    /// `COMMIT` of the failed block of our own rolls it back.
    async fn run_cursor(&self, index: usize, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink, started: Instant) -> Result<()> {
        use futures_util::{future::maybe_done, pin_mut, poll, TryStreamExt};
        let own_tx = self.transaction_state() == TxState::Idle;
        let declare = format!("{}DECLARE {CURSOR} NO SCROLL CURSOR FOR {sql}", if own_tx { "BEGIN; " } else { "" });
        // The server reports a position in the whole query text; the statement starts after the prefix.
        let prefix = (declare.chars().count() - sql.chars().count()) as u32;
        let fetch = match u32::try_from(opts.row_limit.saturating_add(1)) {
            Ok(count) if count <= i32::MAX as u32 => format!("FETCH FORWARD {count} FROM {CURSOR}"),
            _ => format!("FETCH ALL FROM {CURSOR}"),
        };
        let end = if own_tx { "COMMIT".to_owned() } else { format!("CLOSE {CURSOR}") };
        let no_params = std::iter::empty::<(&(dyn tokio_postgres::types::ToSql + Sync), tokio_postgres::types::Type)>();
        let declared = maybe_done(self.client.batch_execute(&declare));
        let fetched = maybe_done(self.client.query_typed_raw(&fetch, no_params));
        let ended = maybe_done(self.client.batch_execute(&end));
        pin_mut!(declared, fetched, ended);
        // Each request is sent when its future is first polled: poll all three before waiting for any answer.
        let _ = poll!(declared.as_mut());
        let _ = poll!(fetched.as_mut());
        let _ = poll!(ended.as_mut());

        let mut columns_sent = false;
        let outcome: Result<(usize, bool)> = async {
            declared.as_mut().await;
            declared.as_mut().take_output().unwrap_or(Ok(())).map_err(|error| {
                let mut error = map_err(error);
                error.position = error.position.map(|position| position.saturating_sub(prefix));
                error
            })?;
            fetched.as_mut().await;
            let stream = fetched.as_mut().take_output().expect("polled to completion").map_err(map_err)?;
            pin_mut!(stream);
            let mut batcher = Batcher::new(index, opts);
            let mut wants_rows = true;
            let mut reading = true;
            let mut received = 0usize;
            while let Some(row) = stream.try_next().await.map_err(map_err)? {
                if !columns_sent {
                    columns_sent = true;
                    wants_rows = sink.event(StreamEvent::Columns {
                        index,
                        columns: result_columns(row.columns()),
                    }) == Flow::Continue;
                    reading = wants_rows;
                }
                received += 1;
                if reading {
                    reading = batcher.push(types::row_values(&row), sink);
                }
            }
            batcher.flush(sink);
            Ok((batcher.kept, wants_rows && received > batcher.kept))
        }
        .await;
        // The block of our own ends (or the cursor closes) whatever happened above.
        ended.as_mut().await;
        let ended = ended.as_mut().take_output().unwrap_or(Ok(()));
        let (rows, truncated) = outcome?;
        ended.map_err(map_err)?;
        if !columns_sent {
            let statement = self.client.prepare(sql).await.map_err(map_err)?;
            sink.event(StreamEvent::Columns {
                index,
                columns: result_columns(statement.columns()),
            });
        }
        sink.event(StreamEvent::Done {
            index,
            affected: None,
            rows,
            truncated,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
        Ok(())
    }

    /// Any other statement: its describe, its execution and a probe of the transaction state after it are sent together
    /// (one round trip). Rows past the cap are read and dropped: the statement must finish (it may be changing rows).
    async fn run_direct(&self, index: usize, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink, started: Instant) -> Result<()> {
        use futures_util::{future::maybe_done, pin_mut, poll};
        let described = maybe_done(self.client.prepare(sql));
        let no_params = std::iter::empty::<(&(dyn tokio_postgres::types::ToSql + Sync), tokio_postgres::types::Type)>();
        let executed = maybe_done(self.client.query_typed_raw(sql, no_params));
        let probed = maybe_done(self.client.simple_query(STATE_PROBE));
        pin_mut!(described, executed, probed);
        let _ = poll!(described.as_mut());
        let _ = poll!(executed.as_mut());
        let _ = poll!(probed.as_mut());
        let outcome = self.finish_direct(index, sql, opts, sink, started, described, executed).await;
        probed.as_mut().await;
        self.store_state(probed.as_mut().take_output().expect("polled to completion"));
        outcome
    }

    async fn finish_direct(
        &self,
        index: usize,
        sql: &str,
        opts: &RunOptions,
        sink: &mut dyn ResultSink,
        started: Instant,
        mut described: std::pin::Pin<
            &mut futures_util::future::MaybeDone<
                impl std::future::Future<Output = std::result::Result<tokio_postgres::Statement, tokio_postgres::Error>>,
            >,
        >,
        mut executed: std::pin::Pin<
            &mut futures_util::future::MaybeDone<
                impl std::future::Future<Output = std::result::Result<tokio_postgres::RowStream, tokio_postgres::Error>>,
            >,
        >,
    ) -> Result<()> {
        use futures_util::{pin_mut, TryStreamExt};
        described.as_mut().await;
        let statement = described.as_mut().take_output().expect("polled to completion");
        executed.as_mut().await;
        let stream = executed.as_mut().take_output().expect("polled to completion");
        // Both fail alike on a statement that does not parse; the describe's error is the one to report.
        let statement = statement.map_err(map_err)?;
        let stream = stream.map_err(map_err)?;
        pin_mut!(stream);
        if statement.columns().is_empty() {
            while stream.try_next().await.map_err(map_err)?.is_some() {}
            sink.event(StreamEvent::Done {
                index,
                affected: Some(stream.rows_affected().unwrap_or(0)),
                rows: 0,
                truncated: false,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
            return Ok(());
        }
        let columns = result_columns(statement.columns());
        let wants_rows = sink.event(StreamEvent::Columns { index, columns }) == Flow::Continue;
        let mut batcher = Batcher::new(index, opts);
        let mut truncated = false;
        let mut reading = wants_rows;
        while let Some(row) = stream.try_next().await.map_err(map_err)? {
            if reading {
                reading = batcher.push(types::row_values(&row), sink);
            } else {
                truncated = true;
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

/// One `ROLLBACK`/`ABORT` (also `ROLLBACK TO SAVEPOINT`) and nothing else.
fn is_lone_rollback(sql: &str) -> bool {
    let statements = split::split(Dialect::Postgres, sql);
    let [only] = statements.as_slice() else {
        return false;
    };
    matches!(
        split::words(Dialect::Postgres, &only.text).first().map(String::as_str),
        Some("ROLLBACK" | "ABORT")
    )
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
