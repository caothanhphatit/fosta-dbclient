//! An Oracle query session: its own connection, streaming fetches, `break_execution` (OCIBreak) for cancel and the statement
//! timeout, implicit result sets from PL/SQL (`DBMS_SQL.RETURN_RESULT`), and the transaction state from
//! `DBMS_TRANSACTION.LOCAL_TRANSACTION_ID`.
//!
//! OCI autocommit stays off. With `auto_commit` off (the default here) Oracle opens a transaction implicitly on the first
//! change and it stays open until COMMIT or ROLLBACK, like SQL*Plus. With it on (what query tabs use, see `run.rs`) the
//! session commits after every statement that can change data, so a tab behaves like the other engines' until a
//! transaction is begun.

use super::{map_err, row_values, types};
use crate::engines::common::split::{self, Dialect};
use crate::engines::common::{
    blocking, run_blocking, Batcher, ColumnInfo, EngineError, ErrorKind, Flow, Result, ResultSink, RunOptions, StreamEvent, TxState, Value,
    ValueKind,
};
use oracle::{Connection, ResultSet, Row};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct OraSession {
    conn: Arc<Connection>,
    auto_commit: Arc<AtomicBool>,
    /// The schema set with `ALTER SESSION SET CURRENT_SCHEMA` (None: the login schema).
    schema: Mutex<Option<String>>,
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

/// A statement of a script failed: the error, which statement it was and where it starts in the script (bytes).
#[derive(Debug)]
pub struct RunFailure {
    pub error: EngineError,
    pub index: usize,
    pub text: String,
    pub start: usize,
}

impl OraSession {
    pub(super) fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(conn),
            auto_commit: Arc::new(AtomicBool::new(false)),
            schema: Mutex::new(None),
        }
    }

    pub fn canceller(&self) -> OraCancel {
        OraCancel { conn: self.conn.clone() }
    }

    /// Commit after every statement that can change data (on) or leave transactions open until COMMIT (off).
    pub fn set_auto_commit(&self, on: bool) {
        self.auto_commit.store(on, Ordering::SeqCst);
    }

    pub fn auto_commit(&self) -> bool {
        self.auto_commit.load(Ordering::SeqCst)
    }

    /// Makes `schema` the session's current schema (unqualified names resolve there). A name that is not a user of the
    /// database (a query tab may pass the service name) is ignored; `None` keeps the current one.
    pub async fn use_schema(&self, schema: Option<&str>) -> Result<()> {
        let Some(wanted) = schema.map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned) else {
            return Ok(());
        };
        if self.schema.lock().map(|s| s.as_deref() == Some(wanted.as_str())).unwrap_or(false) {
            return Ok(());
        }
        let conn = self.conn.clone();
        let name = wanted.clone();
        let switched = blocking(move || {
            let exists: i64 = conn
                .query_row_as("SELECT COUNT(*) FROM all_users WHERE username = :1", &[&name])
                .map_err(map_err)?;
            if exists == 0 {
                return Ok(false);
            }
            conn.execute(&format!("ALTER SESSION SET CURRENT_SCHEMA = \"{}\"", name.replace('"', "")), &[])
                .map_err(map_err)?;
            Ok(true)
        })
        .await?;
        if let (true, Ok(mut current)) = (switched, self.schema.lock()) {
            *current = Some(wanted);
        }
        Ok(())
    }

    /// Runs a script statement by statement (PL/SQL blocks end at a `/` line). Stops at the first error.
    pub async fn run(&self, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
        self.run_detailed(sql, opts, sink).await.map_err(|failure| failure.error)
    }

    /// [`run`](Self::run) that says which statement failed (index, text and its start in the script).
    pub async fn run_detailed(&self, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> std::result::Result<(), Box<RunFailure>> {
        let statements = split::split(Dialect::Oracle, sql);
        let conn = self.conn.clone();
        let opts2 = opts.clone();
        let auto_commit = self.auto_commit();
        let started: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let timed_out = Arc::new(AtomicBool::new(false));
        let failed: Arc<Mutex<Option<(usize, String, usize)>>> = Arc::new(Mutex::new(None));
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
        let (started_w, failed_w) = (started.clone(), failed.clone());
        let read_only = opts.read_only;
        let result = run_blocking(sink, move |sink| {
            for (index, st) in statements.iter().enumerate() {
                if let Ok(mut s) = started_w.lock() {
                    *s = Some(Instant::now());
                }
                let r = if read_only {
                    run_read_only(&conn, index, &st.text, &opts2, sink)
                } else {
                    run_statement(&conn, index, &st.text, &opts2, auto_commit, sink)
                };
                if let Ok(mut s) = started_w.lock() {
                    *s = None;
                }
                if let Err(e) = r {
                    if let Ok(mut f) = failed_w.lock() {
                        *f = Some((index, st.text.clone(), st.start));
                    }
                    if auto_commit && !read_only {
                        // Statement-level rollback already undid the failed statement; this clears anything else it left.
                        let _ = conn.rollback();
                    }
                    return Err(e);
                }
            }
            Ok(())
        })
        .await;
        if let Some(w) = watchdog {
            w.abort();
        }
        let result = match result {
            Err(e) if e.kind == ErrorKind::Cancelled && timed_out.load(Ordering::SeqCst) => {
                Err(EngineError::timeout("The statement", opts.timeout.unwrap_or_default()))
            }
            other => other,
        };
        result.map_err(|error| {
            let (index, text, start) = failed.lock().ok().and_then(|mut f| f.take()).unwrap_or_default();
            Box::new(RunFailure { error, index, text, start })
        })
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

    /// Ends the session: an open transaction is rolled back, then the connection is closed.
    pub async fn close(&self) {
        let conn = self.conn.clone();
        let _ = blocking(move || {
            let _ = conn.rollback();
            conn.close().map_err(map_err)
        })
        .await;
    }
}

fn elapsed(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// Streams one result set; returns (rows kept, truncated). Columns in `json` (indexes) were fetched as text through
/// `JSON_SERIALIZE` and are reported and read as JSON.
fn stream_rows(
    rows: ResultSet<'_, Row>,
    index: usize,
    opts: &RunOptions,
    json: &[usize],
    sink: &mut dyn ResultSink,
) -> Result<(usize, bool)> {
    let columns: Vec<ColumnInfo> = rows
        .column_info()
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut col = if json.contains(&i) {
                ColumnInfo::result(c.name(), "JSON", ValueKind::Json)
            } else {
                ColumnInfo::result(c.name(), &c.oracle_type().to_string(), types::kind_of(c.oracle_type()))
            };
            col.nullable = c.nullable();
            col
        })
        .collect();
    let read = |row: &Row| -> Vec<Value> {
        if json.is_empty() {
            row_values(row)
        } else {
            types::read_row(&columns, row.sql_values())
        }
    };
    if sink.event(StreamEvent::Columns {
        index,
        columns: columns.clone(),
    }) == Flow::Stop
    {
        return Ok((0, true));
    }
    let mut batcher = Batcher::new(index, opts);
    let mut truncated = false;
    let mut rows = rows;
    while let Some(row) = rows.next() {
        let row = row.map_err(map_err)?;
        if !batcher.push(read(&row), sink) {
            // Dropping the result set closes the cursor; the server stops there.
            truncated = rows.next().is_some();
            break;
        }
    }
    batcher.flush(sink);
    Ok((batcher.kept, truncated))
}

/// Whether opening a result set failed on a native JSON column, which the `oracle` crate cannot fetch.
fn is_json_fetch_error(e: &oracle::Error) -> bool {
    e.to_string().contains("unsupported Oracle type JSON")
}

/// [`map_err`] for opening a result set, with a way out for native JSON columns.
fn query_err(e: oracle::Error) -> EngineError {
    let json = is_json_fetch_error(&e);
    let mut err = map_err(e);
    if json {
        err.kind = ErrorKind::Input;
        err.message = "This result has a JSON column, which Fosta Studio cannot read directly yet. Select it as text with \
                       JSON_SERIALIZE(column), e.g. SELECT JSON_SERIALIZE(doc PRETTY) AS doc FROM ..."
            .into();
    }
    err
}

/// `DBMS_SQL.DESCRIBE_COLUMNS2` type code of the native JSON type.
const JSON_TYPE_CODE: &str = "119";

/// The query rewritten so native JSON columns arrive as text: `SELECT q."A", JSON_SERIALIZE(q."DOC" RETURNING CLOB) AS
/// "DOC" FROM (<query>) q`, with the indexes of those columns. The columns are described with DBMS_SQL (parsing a query
/// does not run it). `None` when there is no JSON column or two columns share a name (they could not be selected by name).
fn json_rewrite(conn: &Connection, sql: &str) -> Result<Option<(String, Vec<usize>)>> {
    let mut stmt = conn
        .statement(
            "DECLARE c INTEGER := DBMS_SQL.OPEN_CURSOR; n INTEGER; d DBMS_SQL.DESC_TAB2; r VARCHAR2(32767); \
             BEGIN DBMS_SQL.PARSE(c, :1, DBMS_SQL.NATIVE); DBMS_SQL.DESCRIBE_COLUMNS2(c, n, d); DBMS_SQL.CLOSE_CURSOR(c); \
             FOR i IN 1 .. n LOOP r := r || d(i).col_type || ':' || d(i).col_name || CHR(10); END LOOP; :2 := r; END;",
        )
        .build()
        .map_err(map_err)?;
    stmt.execute(&[&sql, &oracle::sql_type::OracleType::Varchar2(32767)])
        .map_err(map_err)?;
    let described: Option<String> = stmt.bind_value(2).map_err(map_err)?;
    let columns: Vec<(String, String)> = described
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once(':').map(|(t, n)| (t.to_owned(), n.to_owned())))
        .collect();
    let json: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter(|(_, (t, _))| t == JSON_TYPE_CODE)
        .map(|(i, _)| i)
        .collect();
    let mut names: Vec<&str> = columns.iter().map(|(_, n)| n.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if json.is_empty() || names.len() != columns.len() {
        return Ok(None);
    }
    let list: Vec<String> = columns
        .iter()
        .map(|(t, name)| {
            let quoted = format!("\"{}\"", name.replace('"', "\"\""));
            if t == JSON_TYPE_CODE {
                format!("JSON_SERIALIZE(\"q\".{quoted} RETURNING CLOB) AS {quoted}")
            } else {
                format!("\"q\".{quoted}")
            }
        })
        .collect();
    let inner = sql.trim().trim_end_matches(';');
    Ok(Some((format!("SELECT {} FROM ({inner}) \"q\"", list.join(", ")), json)))
}

/// Whether a query keeps row locks (`SELECT … FOR UPDATE`), which only a commit releases.
fn locks_rows(sql: &str) -> bool {
    let upper = sql.to_ascii_uppercase();
    upper
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|w| w == ["FOR", "UPDATE"])
}

/// Oracle's code for `SET TRANSACTION` that is not the first statement of its transaction.
const NOT_FIRST_IN_TRANSACTION: &str = "ORA-01453";

/// A statement of a read-only run (the assistant, a Read-only workspace) inside its own `SET TRANSACTION READ ONLY`,
/// rolled back afterwards whatever happened, and never auto-committed (a commit would end the read-only transaction).
/// Oracle then refuses DML, `SELECT … FOR UPDATE` and DML in called PL/SQL (ORA-01456) that the gateway let through by
/// mistake. Not covered: DDL (it commits implicitly first), autonomous transactions (`PRAGMA AUTONOMOUS_TRANSACTION`)
/// and work on a remote database through a link; those are kept out by the classification. A session that already has
/// uncommitted changes is refused rather than rolled back, so the user's changes stay.
///
/// `EXPLAIN PLAN … FOR` (without `INTO`) only writes plan rows into the session's PLAN_TABLE, which a read-only transaction
/// forbids; it runs in its own transaction instead and those rows are committed, so a following `DBMS_XPLAN.DISPLAY`
/// sees them.
fn run_read_only(conn: &Connection, index: usize, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
    let explain = is_plain_explain_plan(sql);
    let begin = if explain {
        "SET TRANSACTION READ WRITE"
    } else {
        "SET TRANSACTION READ ONLY"
    };
    for attempt in 0.. {
        begin_read_only(conn, begin)?;
        let outcome = run_statement(conn, index, sql, opts, false, sink);
        let end = if explain && outcome.is_ok() {
            conn.commit()
        } else {
            conn.rollback()
        };
        // A read-only transaction cannot read a table whose definition changed after its snapshot; Oracle tracks DDL times
        // to a few seconds, so a table created or altered just before fails with ORA-01466 until that window has passed.
        let changed = matches!(&outcome, Err(error) if error.code.as_deref() == Some(TABLE_DEFINITION_CHANGED));
        if changed && attempt < READ_ONLY_RETRIES {
            end.map_err(map_err)?;
            std::thread::sleep(std::time::Duration::from_secs(1));
            continue;
        }
        return outcome.and(end.map_err(map_err));
    }
    unreachable!("the retry loop returns")
}

/// Starts the read-only (or, for a plain EXPLAIN PLAN, read-write) transaction of one read-only statement.
fn begin_read_only(conn: &Connection, begin: &str) -> Result<()> {
    if let Err(error) = conn.execute(begin, &[]) {
        let error = map_err(error);
        if error.code.as_deref() == Some(NOT_FIRST_IN_TRANSACTION) {
            return Err(EngineError::new(
                ErrorKind::Server,
                "This tab has uncommitted changes, and a read-only run cannot start its read-only transaction. Commit or roll \
                 back first.",
            ));
        }
        return Err(error);
    }
    Ok(())
}

/// Oracle's code for a read-only transaction reading a table whose definition changed after the transaction began.
const TABLE_DEFINITION_CHANGED: &str = "ORA-01466";
/// Retries (one second apart) of a read-only statement that hit ORA-01466 right after DDL on its table.
const READ_ONLY_RETRIES: u32 = 4;

/// `EXPLAIN PLAN [SET STATEMENT_ID = '…'] FOR …`: writes plan rows into PLAN_TABLE and runs nothing. With `INTO` it could
/// write into any table, so a statement with the word INTO anywhere (also `… FOR INSERT INTO`) is not plain.
fn is_plain_explain_plan(sql: &str) -> bool {
    let upper = sql.trim_start().to_ascii_uppercase();
    let mut words = upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty());
    words.next() == Some("EXPLAIN") && words.next() == Some("PLAN") && !words.any(|word| word == "INTO")
}

fn run_statement(
    conn: &Connection,
    index: usize,
    sql: &str,
    opts: &RunOptions,
    auto_commit: bool,
    sink: &mut dyn ResultSink,
) -> Result<()> {
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
        let (kept, truncated) = match stmt.query(&[]) {
            Ok(rows) => stream_rows(rows, index, opts, &[], sink)?,
            Err(e) if is_json_fetch_error(&e) => {
                let Some((rewritten, json)) = json_rewrite(conn, sql)? else {
                    return Err(query_err(e));
                };
                let mut again = conn
                    .statement(&rewritten)
                    .fetch_array_size(batch)
                    .prefetch_rows(batch.min(opts.row_limit.max(1) as u32))
                    .build()
                    .map_err(map_err)?;
                let rows = again.query(&[]).map_err(query_err)?;
                stream_rows(rows, index, opts, &json, sink)?
            }
            Err(e) => return Err(query_err(e)),
        };
        if auto_commit && locks_rows(sql) {
            conn.commit().map_err(map_err)?;
        }
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
    if auto_commit && (stmt.is_dml() || stmt.is_plsql()) {
        conn.commit().map_err(map_err)?;
    }
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
            let (kept, truncated) = stream_rows(rows, index, opts, &[], sink)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explain_plan_without_into_skips_the_read_only_transaction() {
        assert!(is_plain_explain_plan("EXPLAIN PLAN FOR SELECT * FROM t"));
        assert!(is_plain_explain_plan("explain plan set statement_id = 'a' for select 1 from dual"));
        assert!(!is_plain_explain_plan("EXPLAIN PLAN INTO shop.orders FOR SELECT 1 FROM dual"));
        assert!(!is_plain_explain_plan(
            "EXPLAIN PLAN SET STATEMENT_ID = 'x FOR y' INTO t FOR SELECT 1 FROM dual"
        ));
        assert!(!is_plain_explain_plan("SELECT 'EXPLAIN PLAN' FROM dual"));
        assert!(!is_plain_explain_plan("/* x */ EXPLAIN PLAN FOR SELECT 1 FROM dual"));
    }

    #[test]
    fn select_for_update_is_recognised_as_locking() {
        assert!(locks_rows("SELECT * FROM t WHERE id = 1 FOR UPDATE NOWAIT"));
        assert!(locks_rows("select *\nfrom t for\n update"));
        assert!(!locks_rows("SELECT update_time FROM t"));
    }
}
