//! Query-tab sessions: one dedicated client each, so `BEGIN TRANSACTION … COMMIT` spans runs. A run sends the text as
//! one batch (with row-count queries after data changes, see `batch`) and streams every result set as `Statement` /
//! `Columns` / `Rows` / `Done` events, row counts as `Done { affected }`.
//!
//! tiberius has no TDS attention (cancel) request, so Stop and the statement timeout end the run on the client side:
//! `KILL <spid>` from a pooled client stops the server's work (when the login may kill sessions), then the session's
//! socket is dropped and the next run reconnects. An open transaction is rolled back by the server either way.
//!
//! PRINT and other informational messages are not reported: tiberius reads INFO tokens but does not expose them.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicI64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::{future::BoxFuture, TryStreamExt};
use tiberius::{Config, QueryItem};
use tokio::sync::{Mutex, Notify};

use super::{
    batch::{result_labels, statement_at_line, with_row_counts, ROWCOUNT_COLUMN},
    catalog::{bracket, text_rows},
    connect::{connect, is_statement_error, ClientPool, TdsClient, OPEN_TIMEOUT},
    values::{result_column, to_value},
};
use crate::{
    contract::{
        Batcher, Canceller, ColumnInfo, Flow, ResultSink, RunOptions, RunRequest, Session, StreamEvent, TransactionOp, TransactionState,
        ValueKind,
    },
    error::{AppError, AppResult},
};

const SESSION_REOPENED: &str = "The connection of this tab was lost and has been reopened; the server rolled back the open transaction.";
const CANCELLED: &str = "The query was stopped. The tab reconnects for the next run; an open transaction was rolled back.";
/// `@@TRANCOUNT` and the IMPLICIT_TRANSACTIONS bit of `@@OPTIONS` (2): auto-commit is off when it is set.
const TRANSACTION_STATE_SQL: &str = "SELECT @@TRANCOUNT, CASE WHEN (@@OPTIONS & 2) = 2 THEN 0 ELSE 1 END";
/// How long Stop waits for `KILL`.
const KILL_TIMEOUT: Duration = Duration::from_secs(3);

/// Shared by a session and its canceller.
#[derive(Default)]
pub struct CancelSignal {
    notify: Notify,
    /// A run is in progress.
    running: AtomicBool,
    /// Stop was asked for during a run; the session's client is no longer usable.
    requested: AtomicBool,
    /// The server process id of the session's client.
    spid: AtomicI64,
}

struct SessionState {
    client: Option<TdsClient>,
    /// The database chosen with the tab's picker, used again after a reconnect.
    database: Option<String>,
    /// What the user chose with the auto-commit switch; applied again after a reconnect.
    auto_commit: bool,
}

pub struct MssqlSession {
    config: Config,
    pool: Arc<ClientPool>,
    signal: Arc<CancelSignal>,
    state: Mutex<SessionState>,
}

async fn spid_of(client: &mut TdsClient) -> AppResult<i64> {
    let rows = text_rows(client, "SELECT CAST(@@SPID AS BIGINT)", &[]).await?;
    Ok(rows.first().and_then(|row| row.number(0)).unwrap_or(0))
}

async fn execute(client: &mut TdsClient, sql: &str) -> AppResult<()> {
    client.simple_query(sql).await?.into_results().await?;
    Ok(())
}

/// Ends the server's work on `spid` from a pooled client. Errors (a login without ALTER ANY CONNECTION) are ignored:
/// dropping the socket still ends the run here.
async fn kill(pool: &ClientPool, spid: i64) {
    if spid <= 0 {
        return;
    }
    let attempt = async {
        let mut client = pool.take().await?;
        let outcome = execute(&mut client, &format!("KILL {spid}")).await;
        pool.release(client, &outcome);
        outcome
    };
    let _ = tokio::time::timeout(KILL_TIMEOUT, attempt).await;
}

/// The session's transaction state as the server reports it.
async fn transaction_state(client: &mut TdsClient) -> TransactionState {
    match text_rows(client, TRANSACTION_STATE_SQL, &[]).await {
        Ok(rows) => {
            let row = rows.first();
            TransactionState {
                supported: true,
                open: row.and_then(|row| row.number(0)).unwrap_or(0) > 0,
                auto_commit: row.and_then(|row| row.number(1)).unwrap_or(1) != 0,
                failed: false,
            }
        }
        Err(_) => TransactionState::unsupported(),
    }
}

/// The savepoint a read-only run sets inside a transaction the tab already has open.
const READ_ONLY_SAVEPOINT: &str = "fosta_read_only";

/// SQL Server has no read-only session. A read-only run (the assistant, a Read-only workspace) is wrapped in a transaction
/// that is rolled back afterwards, so anything the gateway let through by mistake (DML reached through a procedure or a
/// function) is undone. Inside a transaction the tab already has open, a savepoint is used instead, so the user's own
/// transaction survives. When the run is stopped or times out the connection is dropped and the server rolls back.
///
/// Limits: a COMMIT inside the batch ends the wrapper; work on a linked server (OPENQUERY, four-part names), mail,
/// extended procedures and identity / sequence increments are not rolled back. Those are kept out by the classification
/// (Admin / Write), not by this backstop.
enum ReadOnlyGuard {
    Transaction,
    Savepoint,
}

impl ReadOnlyGuard {
    async fn begin(client: &mut TdsClient) -> AppResult<Self> {
        if transaction_state(client).await.open {
            execute(client, &format!("SAVE TRANSACTION {READ_ONLY_SAVEPOINT}")).await?;
            Ok(Self::Savepoint)
        } else {
            execute(client, "BEGIN TRANSACTION").await?;
            Ok(Self::Transaction)
        }
    }

    async fn end(self, client: &mut TdsClient) -> AppResult<()> {
        let sql = match self {
            Self::Transaction => "IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION".to_owned(),
            // XACT_STATE() -1 (doomed) cannot roll back to a savepoint; the tab reports the failed transaction.
            Self::Savepoint => format!("IF XACT_STATE() = 1 ROLLBACK TRANSACTION {READ_ONLY_SAVEPOINT}"),
        };
        execute(client, &sql).await
    }
}

/// A server error with the failing statement of the batch (from the error's line).
fn map_error(error: tiberius::error::Error, text: &str) -> AppError {
    let line = match &error {
        tiberius::error::Error::Server(token) => Some(token.line()),
        _ => None,
    };
    let mapped = AppError::from(error);
    match line.filter(|line| *line > 0).and_then(|line| statement_at_line(text, line)) {
        Some((index, statement)) => mapped.with_statement(index, &statement),
        None => mapped,
    }
}

/// The result set being read.
enum Current {
    Rows {
        index: usize,
        columns: Vec<ColumnInfo>,
        batcher: Batcher,
        truncated: bool,
        started: Instant,
    },
    Count {
        index: usize,
        affected: u64,
        started: Instant,
    },
}

impl Current {
    fn finish(self, sink: &mut dyn ResultSink) -> Flow {
        match self {
            Current::Rows {
                index,
                mut batcher,
                truncated,
                started,
                ..
            } => {
                batcher.flush(sink);
                sink.event(StreamEvent::Done {
                    index,
                    affected: None,
                    rows: batcher.kept,
                    truncated,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                })
            }
            Current::Count { index, affected, started } => sink.event(StreamEvent::Done {
                index,
                affected: Some(affected),
                rows: 0,
                truncated: false,
                elapsed_ms: started.elapsed().as_millis() as u64,
            }),
        }
    }
}

/// Streams every result set of the batch. A batch without results (DDL, SET …) reports one statement with 0 rows.
async fn stream_batch(client: &mut TdsClient, text: &str, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
    let labels = result_labels(text);
    let label = |index: usize| labels.get(index).cloned().unwrap_or_default();
    let batch = with_row_counts(text);
    let started = Instant::now();
    let mut stream = client.simple_query(batch).await.map_err(|error| map_error(error, text))?;
    let mut current: Option<Current> = None;
    let mut next_index = 0;
    loop {
        let item = match stream.try_next().await {
            Ok(Some(item)) => item,
            Ok(None) => break,
            Err(error) => {
                if let Some(open) = current.take() {
                    open.finish(sink);
                }
                return Err(map_error(error, text));
            }
        };
        let columns = match &item {
            QueryItem::Metadata(meta) => Some(meta.columns().to_vec()),
            QueryItem::Row(row) if current.is_none() => Some(row.columns().to_vec()),
            QueryItem::Row(_) => None,
        };
        if let Some(columns) = columns {
            if let Some(open) = current.take() {
                if open.finish(sink) == Flow::Stop {
                    return Ok(());
                }
            }
            let index = next_index;
            next_index += 1;
            if sink.event(StreamEvent::Statement { index, sql: label(index) }) == Flow::Stop {
                return Ok(());
            }
            let now = Instant::now();
            current = Some(if matches!(columns.as_slice(), [column] if column.name() == ROWCOUNT_COLUMN) {
                Current::Count {
                    index,
                    affected: 0,
                    started: now,
                }
            } else {
                let infos: Vec<ColumnInfo> = columns.iter().map(result_column).collect();
                sink.event(StreamEvent::Columns {
                    index,
                    columns: infos.clone(),
                });
                Current::Rows {
                    index,
                    columns: infos,
                    batcher: Batcher::new(index, options),
                    truncated: false,
                    started: now,
                }
            });
        }
        if let QueryItem::Row(row) = item {
            match current.as_mut() {
                Some(Current::Count { affected, .. }) => {
                    let count = row
                        .into_iter()
                        .next()
                        .map(|data| to_value(data, &ColumnInfo::result("", "", ValueKind::Int)));
                    if let Some(crate::contract::Value::Int(count)) = count {
                        *affected = count.max(0) as u64;
                    }
                }
                Some(Current::Rows {
                    columns,
                    batcher,
                    truncated,
                    ..
                }) => {
                    if batcher.wants_more() {
                        let values = row
                            .into_iter()
                            .zip(columns.iter())
                            .map(|(data, column)| to_value(data, column))
                            .collect();
                        batcher.push(values, sink);
                    } else {
                        // Rows past the limit are read and dropped so the client stays usable.
                        *truncated = true;
                    }
                }
                None => {}
            }
        }
    }
    match current {
        Some(open) => {
            open.finish(sink);
        }
        None => {
            if sink.event(StreamEvent::Statement {
                index: 0,
                sql: text.trim().to_owned(),
            }) == Flow::Continue
            {
                sink.event(StreamEvent::Done {
                    index: 0,
                    affected: Some(0),
                    rows: 0,
                    truncated: false,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
        }
    }
    Ok(())
}

impl MssqlSession {
    pub async fn open(config: Config, pool: Arc<ClientPool>, database: Option<&str>) -> AppResult<(Self, Arc<CancelSignal>)> {
        let mut client = connect(&config, OPEN_TIMEOUT).await?;
        let database = database.filter(|name| !name.is_empty()).map(str::to_owned);
        if let Some(database) = &database {
            execute(&mut client, &format!("USE {}", bracket(database))).await?;
        }
        let signal = Arc::new(CancelSignal::default());
        signal.spid.store(spid_of(&mut client).await?, Ordering::Relaxed);
        let session = Self {
            config,
            pool,
            signal: Arc::clone(&signal),
            state: Mutex::new(SessionState {
                client: Some(client),
                database,
                auto_commit: true,
            }),
        };
        Ok((session, signal))
    }

    /// Reconnects after Stop, a timeout or a lost connection. Returns true when it had to (an open transaction is gone).
    async fn revive(&self, state: &mut SessionState) -> AppResult<bool> {
        if self.signal.requested.swap(false, Ordering::Relaxed) {
            state.client = None;
        }
        if state.client.is_some() {
            return Ok(false);
        }
        let mut fresh = connect(&self.config, OPEN_TIMEOUT).await?;
        if let Some(database) = &state.database {
            execute(&mut fresh, &format!("USE {}", bracket(database))).await?;
        }
        if !state.auto_commit {
            execute(&mut fresh, "SET IMPLICIT_TRANSACTIONS ON").await?;
        }
        self.signal.spid.store(spid_of(&mut fresh).await?, Ordering::Relaxed);
        state.client = Some(fresh);
        Ok(true)
    }

    fn client(state: &mut SessionState) -> AppResult<&mut TdsClient> {
        state
            .client
            .as_mut()
            .ok_or_else(|| AppError::new("This query tab's session is closed. Open a new query."))
    }

    async fn run_locked(
        &self,
        state: &mut SessionState,
        request: &RunRequest,
        options: &RunOptions,
        sink: &mut dyn ResultSink,
    ) -> AppResult<()> {
        let notified = self.signal.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        self.revive(state).await?;
        if let Some(database) = request.database.as_deref().filter(|name| !name.is_empty()) {
            execute(Self::client(state)?, &format!("USE {}", bracket(database))).await?;
            state.database = Some(database.to_owned());
        }
        let timeout = options.timeout.filter(|timeout| !timeout.is_zero());
        let guard = match options.read_only {
            true => Some(ReadOnlyGuard::begin(Self::client(state)?).await?),
            false => None,
        };
        let client = Self::client(state)?;
        let outcome = tokio::select! {
            biased;
            _ = &mut notified => Err(AppError::coded("CANCELLED", CANCELLED)),
            _ = sleep_for(timeout) => {
                // The client is still connected (and its process id still ours): stop the server's work first.
                kill(&self.pool, self.signal.spid.load(Ordering::Relaxed)).await;
                Err(AppError::coded(
                    "TIMEOUT",
                    format!(
                        "The statement did not finish within {} s (the workspace's statement timeout) and was stopped. \
                         The tab reconnects for the next run.",
                        timeout.map_or(0, |timeout| timeout.as_secs())
                    ),
                ))
            }
            result = stream_batch(client, &request.text, options, sink) => result,
        };
        let keeps_client = match &outcome {
            Ok(()) => true,
            Err(error) => is_statement_error(error) && !self.signal.requested.load(Ordering::Relaxed),
        };
        if let (Some(guard), true) = (guard, keeps_client) {
            if let Err(error) = guard.end(Self::client(state)?).await {
                // Closing the connection rolls back whatever the rollback could not.
                state.client = None;
                outcome?;
                return Err(error);
            }
        }
        let Err(error) = outcome else { return Ok(()) };
        if self.signal.requested.load(Ordering::Relaxed) {
            state.client = None;
            return Err(AppError::coded("CANCELLED", CANCELLED));
        }
        if !is_statement_error(&error) {
            // Timed out, cancelled or the connection broke: the client may be mid-response.
            state.client = None;
        }
        Err(error)
    }
}

/// Sleeps for the timeout, or forever without one.
async fn sleep_for(timeout: Option<Duration>) {
    match timeout {
        Some(timeout) => tokio::time::sleep(timeout).await,
        None => std::future::pending().await,
    }
}

#[async_trait]
impl Session for MssqlSession {
    fn language(&self) -> &str {
        "sql"
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        let mut state = self.state.lock().await;
        self.signal.running.store(true, Ordering::Relaxed);
        let outcome = self.run_locked(&mut state, request, options, sink).await;
        self.signal.running.store(false, Ordering::Relaxed);
        outcome
    }

    async fn transaction(&self, op: TransactionOp, _options: &RunOptions) -> AppResult<TransactionState> {
        let mut state = self.state.lock().await;
        if self.revive(&mut state).await? && matches!(op, TransactionOp::Commit | TransactionOp::Rollback) {
            return Err(AppError::new(SESSION_REOPENED));
        }
        let client = Self::client(&mut state)?;
        if op == TransactionOp::Begin {
            // BEGIN TRANSACTION would nest (@@TRANCOUNT 2), so one Commit would leave it open.
            transaction_state(client).await.ensure_can_begin()?;
        }
        let sql = match op {
            TransactionOp::Begin => Some("BEGIN TRANSACTION"),
            TransactionOp::Commit => Some("IF @@TRANCOUNT > 0 COMMIT TRANSACTION"),
            TransactionOp::Rollback => Some("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION"),
            TransactionOp::AutoCommitOn => Some("SET IMPLICIT_TRANSACTIONS OFF"),
            TransactionOp::AutoCommitOff => Some("SET IMPLICIT_TRANSACTIONS ON"),
            TransactionOp::Status => None,
        };
        if let Some(sql) = sql {
            execute(client, sql).await?;
        }
        match op {
            TransactionOp::AutoCommitOn => state.auto_commit = true,
            TransactionOp::AutoCommitOff => state.auto_commit = false,
            _ => {}
        }
        Ok(transaction_state(Self::client(&mut state)?).await)
    }

    async fn transaction_state(&self) -> TransactionState {
        let mut state = self.state.lock().await;
        match state.client.as_mut() {
            Some(client) if !self.signal.requested.load(Ordering::Relaxed) => transaction_state(client).await,
            _ => TransactionState::unsupported(),
        }
    }

    async fn close(&self) {
        let client = self.state.lock().await.client.take();
        if let Some(client) = client {
            let _ = tokio::time::timeout(Duration::from_secs(2), client.close()).await;
        }
    }
}

/// Stop: `KILL` the session's server process (while its socket still holds the process id), then end the run.
pub struct MssqlCanceller {
    pub pool: Arc<ClientPool>,
    pub signal: Arc<CancelSignal>,
}

impl Canceller for MssqlCanceller {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>> {
        let (pool, signal) = (Arc::clone(&self.pool), Arc::clone(&self.signal));
        Box::pin(async move {
            if !signal.running.load(Ordering::Relaxed) {
                return Ok(());
            }
            signal.requested.store(true, Ordering::Relaxed);
            kill(&pool, signal.spid.load(Ordering::Relaxed)).await;
            signal.notify.notify_waiters();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::CollectSink;

    #[test]
    fn result_sets_finish_with_their_counts() {
        let mut sink = CollectSink::default();
        sink.event(StreamEvent::Statement {
            index: 0,
            sql: "UPDATE t SET a = 1".into(),
        });
        let count = Current::Count {
            index: 0,
            affected: 3,
            started: Instant::now(),
        };
        assert_eq!(count.finish(&mut sink), Flow::Continue);
        assert_eq!(sink.sets[0].affected, Some(3));
        sink.event(StreamEvent::Statement {
            index: 1,
            sql: "SELECT 1".into(),
        });
        let options = RunOptions {
            row_limit: 1,
            ..RunOptions::default()
        };
        let mut batcher = Batcher::new(1, &options);
        batcher.push(vec![crate::contract::Value::Int(1)], &mut sink);
        let rows = Current::Rows {
            index: 1,
            columns: Vec::new(),
            batcher,
            truncated: true,
            started: Instant::now(),
        };
        rows.finish(&mut sink);
        assert_eq!(sink.sets[1].rows.len(), 1);
        assert!(sink.sets[1].truncated && sink.sets[1].affected.is_none());
    }

    #[test]
    fn server_errors_name_their_statement_from_the_line() {
        let error = AppError::from(std::io::Error::other("reset"));
        assert!(!is_statement_error(&error));
        assert_eq!(
            statement_at_line("SELECT 1;\nSELECT * FROM nope", 2).map(|(index, _)| index),
            Some(1)
        );
    }
}
