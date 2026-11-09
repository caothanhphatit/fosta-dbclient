//! Query-tab sessions: one dedicated connection each, so `BEGIN … COMMIT` spans runs. A run sends the whole script with
//! `query_iter` (the server splits it) and streams every result as `Statement` / `Columns` / `Rows` / `Done` events.
//! Stop sends `KILL QUERY` from a connection of its own (the pool may be busy), without the session's lock. A read that
//! reaches the row cap as the script's last statement is stopped the same way instead of being read to its end.

use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use mysql_async::{prelude::*, Conn, Opts};
use tokio::sync::Mutex;

use super::{
    catalog::quote_ident,
    connect::{within, CHECKOUT_TIMEOUT},
    values::{result_column, to_value},
};
use crate::{
    contract::{
        Batcher, Canceller, Category, ColumnInfo, Flow, ResultSink, RunOptions, RunRequest, Session, SqlDialectId, StreamEvent,
        TransactionOp, TransactionState,
    },
    engines::common::sqlclassify::classify_sql,
    error::{AppError, AppResult},
};

/// How long opening the side connection for `KILL QUERY` may take.
const KILL_TIMEOUT: Duration = Duration::from_secs(5);

/// `KILL QUERY <thread>` from a connection of its own: the pool may be exhausted by the very query being stopped.
pub(super) async fn kill_query(opts: &Opts, thread_id: u32) -> AppResult<()> {
    let mut conn = within(KILL_TIMEOUT, "Opening a connection to stop the query", Conn::new(opts.clone())).await?;
    let killed = conn.query_drop(format!("KILL QUERY {thread_id}")).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.disconnect()).await;
    Ok(killed?)
}

/// A statement that only reads, so stopping it at the row cap loses nothing: SELECT / WITH / VALUES / TABLE / SHOW that
/// the classifier calls a read (not `INSERT … RETURNING`, `CALL` or `SELECT … INTO`).
fn stoppable(mariadb: bool, statement: &str) -> bool {
    let first = first_word(statement).to_ascii_uppercase();
    let dialect = if mariadb { SqlDialectId::MariaDb } else { SqlDialectId::MySql };
    matches!(first.as_str(), "SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW")
        && classify_sql(dialect, statement).worst() == Some(Category::Read)
}

/// The first word of a statement after comments, white space and opening parentheses.
fn first_word(statement: &str) -> &str {
    let mut rest = statement;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
        if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        } else if rest.starts_with('#') || rest.starts_with("--") {
            rest = rest.split_once('\n').map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    let end = rest.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(rest.len());
    &rest[..end]
}

const SESSION_REOPENED: &str = "The connection of this tab was lost and has been reopened; the server rolled back the open transaction.";

/// A session's connection and the settings already applied to it.
struct SessionConn {
    conn: Option<Conn>,
    /// `SET SESSION TRANSACTION READ ONLY/WRITE` last applied; `None` = not yet.
    read_only: Option<bool>,
    /// Statement timeout last applied (0 = none).
    timeout_ms: Option<u64>,
    /// What the user chose with the auto-commit switch; re-applied after a reconnect.
    auto_commit: bool,
}

impl SessionConn {
    fn conn(&mut self) -> AppResult<&mut Conn> {
        self.conn
            .as_mut()
            .ok_or_else(|| AppError::new("This query tab's session is closed. Open a new query."))
    }
}

pub struct MySqlSession {
    opts: Opts,
    mariadb: bool,
    thread_id: Arc<AtomicU32>,
    state: Mutex<SessionConn>,
}

impl MySqlSession {
    pub async fn open(opts: Opts, mariadb: bool, database: Option<&str>) -> AppResult<(Self, Arc<AtomicU32>)> {
        let mut conn = within(CHECKOUT_TIMEOUT, "Opening a query session", Conn::new(opts.clone())).await?;
        if let Some(database) = database.filter(|name| !name.is_empty()) {
            conn.query_drop(format!("USE {}", quote_ident(database))).await?;
        }
        let thread_id = Arc::new(AtomicU32::new(conn.id()));
        let session = Self {
            opts,
            mariadb,
            thread_id: Arc::clone(&thread_id),
            state: Mutex::new(SessionConn {
                conn: Some(conn),
                read_only: None,
                timeout_ms: None,
                auto_commit: true,
            }),
        };
        Ok((session, thread_id))
    }

    /// Reconnects a dead session. Returns true when it had to (an open transaction is then gone).
    async fn revive(&self, state: &mut SessionConn) -> AppResult<bool> {
        if let Some(conn) = state.conn.as_mut() {
            if conn.ping().await.is_ok() {
                return Ok(false);
            }
        }
        let mut fresh = within(CHECKOUT_TIMEOUT, "Reopening the session", Conn::new(self.opts.clone())).await?;
        if !state.auto_commit {
            fresh.query_drop("SET autocommit = 0").await?;
        }
        self.thread_id.store(fresh.id(), Ordering::Relaxed);
        state.conn = Some(fresh);
        state.read_only = None;
        state.timeout_ms = None;
        Ok(true)
    }

    /// Applies the read-only mode and the statement timeout of the rules. Read-only is set again before every read-only
    /// run: a statement of an earlier run may have lifted it (`SET SESSION TRANSACTION READ WRITE`,
    /// ``SET `transaction_read_only` = 0``), and the remembered state would not know.
    async fn apply_limits(&self, state: &mut SessionConn, options: &RunOptions) -> AppResult<()> {
        if options.read_only || state.read_only != Some(options.read_only) {
            let mode = if options.read_only { "READ ONLY" } else { "READ WRITE" };
            state.conn()?.query_drop(format!("SET SESSION TRANSACTION {mode}")).await?;
            state.read_only = Some(options.read_only);
        }
        let timeout_ms = options.timeout.map_or(0, |timeout| timeout.as_millis() as u64);
        if state.timeout_ms != Some(timeout_ms) {
            let sql = if self.mariadb {
                format!("SET SESSION max_statement_time = {}", timeout_ms as f64 / 1000.0)
            } else {
                // MySQL only limits SELECT statements.
                format!("SET SESSION max_execution_time = {timeout_ms}")
            };
            state.conn()?.query_drop(sql).await?;
            state.timeout_ms = Some(timeout_ms);
        }
        Ok(())
    }

    async fn state_of(&self, state: &mut SessionConn) -> TransactionState {
        match state.conn.as_mut() {
            Some(conn) => transaction_state(conn, self.mariadb).await,
            None => TransactionState::unsupported(),
        }
    }
}

/// The session's transaction state as the server reports it.
pub async fn transaction_state(conn: &mut Conn, mariadb: bool) -> TransactionState {
    let sql = if mariadb {
        "SELECT @@in_transaction, @@autocommit"
    } else {
        "SELECT (SELECT COUNT(*) FROM performance_schema.events_transactions_current \
         WHERE THREAD_ID = PS_CURRENT_THREAD_ID() AND STATE = 'ACTIVE'), @@autocommit"
    };
    match conn.query_first::<(i64, i64), _>(sql).await {
        Ok(Some((open, auto_commit))) => TransactionState {
            supported: true,
            open: open > 0,
            auto_commit: auto_commit != 0,
            failed: false,
        },
        _ => TransactionState::unsupported(),
    }
}

/// A server error with the codes the UI acts on: interrupted (`KILL QUERY`) and statement timeouts.
fn map_error(error: mysql_async::Error) -> AppError {
    let code = match &error {
        mysql_async::Error::Server(server) => Some(server.code),
        _ => None,
    };
    let mapped = AppError::from(error);
    match code {
        Some(1317) => mapped.with("code", "CANCELLED"),
        // MariaDB max_statement_time, MySQL max_execution_time.
        Some(1969 | 3024) => mapped.with("code", "TIMEOUT"),
        _ => mapped,
    }
}

/// The statements of a script as the server will see them, for event labels and error messages: `;` outside quotes and
/// comments ends a statement, and a stored-program definition (or `BEGIN NOT ATOMIC`) takes the rest of the script.
pub fn split_statements(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && quote != b'`' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        if bytes.get(i + 1) == Some(&quote) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'#' => i = line_end(bytes, i),
            b'-' if bytes.get(i + 1) == Some(&b'-') && bytes.get(i + 2).is_none_or(|c| c.is_ascii_whitespace()) => i = line_end(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b';' => {
                let statement = text[start..i].trim();
                if !statement.is_empty() {
                    if is_compound(statement) {
                        break;
                    }
                    statements.push(statement.to_owned());
                }
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    let rest = text.get(start..).unwrap_or_default().trim();
    if !rest.is_empty() {
        statements.push(rest.trim_end_matches(';').trim_end().to_owned());
    }
    statements
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(bytes.len(), |offset| from + offset)
}

/// Statements whose body holds `;` that the server parses itself.
fn is_compound(statement: &str) -> bool {
    let words: Vec<String> = statement.split_whitespace().take(4).map(str::to_ascii_uppercase).collect();
    let first = words.first().map(String::as_str);
    if first == Some("BEGIN") {
        return words.get(1).map(String::as_str) == Some("NOT");
    }
    first == Some("CREATE")
        && words[1..]
            .iter()
            .any(|word| matches!(word.as_str(), "PROCEDURE" | "FUNCTION" | "TRIGGER" | "EVENT") || word.starts_with("DEFINER"))
}

fn millis(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

#[async_trait]
impl Session for MySqlSession {
    fn language(&self) -> &str {
        "sql"
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        let mut state = self.state.lock().await;
        self.revive(&mut state).await?;
        self.apply_limits(&mut state, options).await?;
        let conn = state.conn()?;
        if let Some(database) = request.database.as_deref().filter(|name| !name.is_empty()) {
            conn.query_drop(format!("USE {}", quote_ident(database))).await?;
        }
        let texts = split_statements(&request.text);
        let label = |index: usize| -> String {
            match texts.len() {
                1 if index == 0 => texts[0].clone(),
                _ => texts.get(index).cloned().unwrap_or_default(),
            }
        };
        let thread_id = conn.id();
        let mut index = 0;
        let mut started = Instant::now();
        let mut result = conn
            .query_iter(request.text.as_str())
            .await
            .map_err(|error| map_error(error).with_statement(0, &label(0)))?;
        loop {
            let Some(columns) = result.columns() else {
                // A statement that failed after the previous one is held as a pending error, which `columns()` does
                // not report; reading once more returns it (or nothing when the script is done).
                return match result.next().await {
                    Ok(_) => Ok(()),
                    Err(error) => Err(map_error(error).with_statement(index, &label(index))),
                };
            };
            if sink.event(StreamEvent::Statement { index, sql: label(index) }) == Flow::Stop {
                return Ok(());
            }
            let affected = result.affected_rows();
            // Only the script's last statement is stopped at the cap: KILL QUERY would also end the statements after
            // it, so earlier ones are read to their end.
            let stop_at_cap = index + 1 >= texts.len() && stoppable(self.mariadb, &label(index));
            let mut rows = 0;
            let mut truncated = false;
            let mut killed = false;
            // One attempt per statement: a failing side connection (max_user_connections, a full server) is not retried
            // for every row past the cap.
            let mut kill_tried = false;
            let outcome: mysql_async::Result<()> = async {
                if columns.is_empty() {
                    // An OK packet: reading it moves to the next result.
                    while result.next().await?.is_some() {}
                    return Ok(());
                }
                let infos: Vec<ColumnInfo> = columns.iter().map(result_column).collect();
                sink.event(StreamEvent::Columns {
                    index,
                    columns: infos.clone(),
                });
                let mut batcher = Batcher::new(index, options);
                let read: mysql_async::Result<()> = async {
                    while let Some(row) = result.next().await? {
                        if batcher.wants_more() {
                            let values = row
                                .unwrap()
                                .into_iter()
                                .zip(&infos)
                                .map(|(value, column)| to_value(value, column))
                                .collect();
                            batcher.push(values, sink);
                        } else {
                            truncated = true;
                            if stop_at_cap && !kill_tried {
                                kill_tried = true;
                                // The server stops computing and sending; rows already on the way are dropped.
                                killed = kill_query(&self.opts, thread_id).await.is_ok();
                            }
                            // Otherwise rows past the limit are read and dropped so the connection stays usable.
                        }
                    }
                    Ok(())
                }
                .await;
                batcher.flush(sink);
                rows = batcher.kept;
                match read {
                    // The stop this run sent itself ends the statement as truncated.
                    Err(mysql_async::Error::Server(error)) if killed && error.code == 1317 => Ok(()),
                    other => other,
                }
            }
            .await;
            let elapsed_ms = millis(started);
            match outcome {
                Ok(()) => {
                    let flow = sink.event(StreamEvent::Done {
                        index,
                        affected: columns.is_empty().then_some(affected),
                        rows,
                        truncated,
                        elapsed_ms,
                    });
                    if flow == Flow::Stop {
                        return Ok(());
                    }
                }
                Err(error) => {
                    // An OK packet cannot fail itself: the error belongs to the statement after it.
                    let failed = if columns.is_empty() { index + 1 } else { index };
                    return Err(map_error(error).with_statement(failed, &label(failed)));
                }
            }
            index += 1;
            started = Instant::now();
        }
    }

    async fn transaction(&self, op: TransactionOp, options: &RunOptions) -> AppResult<TransactionState> {
        let mut state = self.state.lock().await;
        if self.revive(&mut state).await? && matches!(op, TransactionOp::Commit | TransactionOp::Rollback) {
            return Err(AppError::new(SESSION_REOPENED));
        }
        match op {
            TransactionOp::Begin => {
                // START TRANSACTION would implicitly commit an open transaction.
                self.state_of(&mut state).await.ensure_can_begin()?;
                self.apply_limits(&mut state, options).await?;
                let sql = if options.read_only {
                    "START TRANSACTION READ ONLY"
                } else {
                    "START TRANSACTION"
                };
                state.conn()?.query_drop(sql).await?;
            }
            TransactionOp::Commit => state.conn()?.query_drop("COMMIT").await?,
            TransactionOp::Rollback => state.conn()?.query_drop("ROLLBACK").await?,
            TransactionOp::AutoCommitOn | TransactionOp::AutoCommitOff => {
                let on = op == TransactionOp::AutoCommitOn;
                state.conn()?.query_drop(format!("SET autocommit = {}", u8::from(on))).await?;
                state.auto_commit = on;
            }
            TransactionOp::Status => {}
        }
        Ok(self.state_of(&mut state).await)
    }

    async fn transaction_state(&self) -> TransactionState {
        let mut state = self.state.lock().await;
        self.state_of(&mut state).await
    }

    async fn close(&self) {
        let conn = self.state.lock().await.conn.take();
        if let Some(conn) = conn {
            let _ = tokio::time::timeout(Duration::from_secs(2), conn.disconnect()).await;
        }
    }
}

/// Stop: `KILL QUERY <thread id>` from a connection of its own.
pub struct MySqlCanceller {
    pub opts: Opts,
    pub thread_id: Arc<AtomicU32>,
}

impl Canceller for MySqlCanceller {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>> {
        let (opts, thread_id) = (self.opts.clone(), self.thread_id.load(Ordering::Relaxed));
        Box::pin(async move { kill_query(&opts, thread_id).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_split_outside_quotes_and_comments() {
        assert_eq!(
            split_statements("SELECT 'a;b', `c;d` FROM t; -- x;y\nSELECT 2 # z;\n;"),
            vec!["SELECT 'a;b', `c;d` FROM t", "-- x;y\nSELECT 2 # z;"]
        );
        assert_eq!(split_statements("SELECT 'it\\'s;'; SELECT 3"), vec!["SELECT 'it\\'s;'", "SELECT 3"]);
        assert_eq!(split_statements("SELECT 1 /* ; */"), vec!["SELECT 1 /* ; */"]);
        assert_eq!(split_statements("  "), Vec::<String>::new());
    }

    #[test]
    fn stored_programs_keep_their_body() {
        let script = "SELECT 1; CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END";
        assert_eq!(
            split_statements(script),
            vec!["SELECT 1", "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END"]
        );
        assert_eq!(split_statements("BEGIN NOT ATOMIC SELECT 1; END").len(), 1);
        assert_eq!(split_statements("BEGIN; SELECT 1").len(), 2);
    }

    #[test]
    fn interrupted_and_timed_out_statements_have_their_own_codes() {
        let server = |code| {
            mysql_async::Error::Server(mysql_async::ServerError {
                code,
                message: "x".into(),
                state: "70100".into(),
            })
        };
        assert_eq!(map_error(server(1317)).code(), Some("CANCELLED"));
        assert_eq!(map_error(server(1969)).code(), Some("TIMEOUT"));
        assert_eq!(map_error(server(1064)).code(), Some("ENGINE_ERROR"));
    }

    #[test]
    fn only_reads_stop_at_the_row_cap() {
        assert!(stoppable(true, "SELECT * FROM t"));
        assert!(stoppable(true, "-- note\n/* x */ (SELECT 1) UNION (SELECT 2)"));
        assert!(stoppable(false, "WITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(stoppable(true, "SHOW PROCESSLIST"));
        assert!(!stoppable(true, "INSERT INTO t SELECT * FROM u RETURNING id"));
        assert!(!stoppable(true, "CALL p()"));
        assert!(!stoppable(true, "DELETE FROM t RETURNING *"));
        assert_eq!(first_word("  # c\n  select 1"), "select");
    }
}
