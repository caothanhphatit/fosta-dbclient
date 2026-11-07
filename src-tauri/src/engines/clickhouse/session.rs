//! A ClickHouse query session: one HTTP `session_id`, one request per statement, streaming line by line.

use super::{new_query_id, server_error, Http, RowReader, FORMAT};
use crate::engines::common::split::{self, Dialect};
use crate::engines::common::{
    Batcher, ColumnInfo, EngineError, Flow, Result, ResultSink, RunOptions, StreamEvent, TxState, Value, ValueKind,
};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct ChSession {
    http: Http,
    session_id: String,
    running: Arc<Mutex<Option<String>>>,
}

/// Cancels the statement a session is running with `KILL QUERY` (on a separate request).
#[derive(Clone)]
pub struct ChCancel {
    http: Http,
    running: Arc<Mutex<Option<String>>>,
}

impl ChCancel {
    pub async fn cancel(&self) -> Result<()> {
        let id = self.running.lock().unwrap_or_else(|p| p.into_inner()).clone();
        match id {
            Some(id) => self.http.kill(&id, false).await,
            None => Ok(()),
        }
    }
}

impl ChSession {
    pub(super) fn new(http: Http) -> Self {
        Self {
            http,
            session_id: new_query_id().replacen("fosta-", "fosta-session-", 1),
            running: Arc::new(Mutex::new(None)),
        }
    }

    pub fn canceller(&self) -> ChCancel {
        ChCancel {
            http: self.http.clone(),
            running: self.running.clone(),
        }
    }

    /// ClickHouse has no transactions.
    pub fn transaction_state(&self) -> TxState {
        TxState::Unsupported
    }

    fn set_running(&self, id: Option<String>) {
        *self.running.lock().unwrap_or_else(|p| p.into_inner()) = id;
    }

    /// Runs a script statement by statement (the HTTP interface takes one statement per request). Stops at the first error.
    pub async fn run(&self, sql: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
        for (index, st) in split::split(Dialect::ClickHouse, sql).into_iter().enumerate() {
            sink.event(StreamEvent::Statement {
                index,
                sql: st.text.clone(),
            });
            let query_id = new_query_id();
            self.set_running(Some(query_id.clone()));
            let r = self.run_statement(index, &st.text, &query_id, opts, sink).await;
            self.set_running(None);
            r?;
        }
        Ok(())
    }

    async fn run_statement(&self, index: usize, sql: &str, query_id: &str, opts: &RunOptions, sink: &mut dyn ResultSink) -> Result<()> {
        let started = Instant::now();
        let mut settings = vec![("session_id".to_owned(), self.session_id.clone())];
        if let Some(t) = opts.timeout {
            settings.push(("max_execution_time".into(), format!("{:.3}", t.as_secs_f64())));
            settings.push(("timeout_overflow_mode".into(), "throw".into()));
        }
        let resp = self.http.send(sql, query_id, &settings).await?;
        let summary = resp
            .inner
            .headers()
            .get("X-ClickHouse-Summary")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let written = summary
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|j| j.get("written_rows")?.as_str()?.parse::<u64>().ok());
        let mut inner = resp.inner;
        let elapsed = |s: Instant| s.elapsed().as_millis() as u64;

        match resp.format.as_deref() {
            Some(FORMAT) => {
                let mut reader = RowReader::default();
                let mut batcher = Batcher::new(index, opts);
                let mut sent_columns = false;
                let mut truncated = false;
                'read: loop {
                    let chunk = inner.chunk().await.map_err(|e| read_error(&e))?;
                    let done = chunk.is_none();
                    reader.feed(chunk.as_deref().unwrap_or_default(), done)?;
                    loop {
                        let row = reader.next_row()?;
                        if !sent_columns && reader.has_header() {
                            sent_columns = true;
                            if sink.event(StreamEvent::Columns {
                                index,
                                columns: reader.columns.clone(),
                            }) == Flow::Stop
                            {
                                truncated = true;
                                break 'read;
                            }
                        }
                        let Some(row) = row else { break };
                        if !batcher.push(row, sink) {
                            truncated = true;
                            break 'read;
                        }
                    }
                    if done {
                        break;
                    }
                }
                batcher.flush(sink);
                if truncated {
                    // Closing the connection stops a read-only query; KILL also stops anything else still running.
                    drop(inner);
                    // SYNC: the session stays locked until the query has stopped.
                    let _ = self.http.kill(query_id, true).await;
                }
                sink.event(StreamEvent::Done {
                    index,
                    affected: None,
                    rows: batcher.kept,
                    truncated,
                    elapsed_ms: elapsed(started),
                });
            }
            Some(_) => {
                // The statement chose its own FORMAT: hand the text over as one cell.
                let text = inner.text().await.map_err(|e| read_error(&e))?;
                let col = ColumnInfo::result("result", "String", ValueKind::Text);
                sink.event(StreamEvent::Columns { index, columns: vec![col] });
                sink.event(StreamEvent::Rows {
                    index,
                    rows: vec![vec![Value::Text(text)]],
                });
                sink.event(StreamEvent::Done {
                    index,
                    affected: None,
                    rows: 1,
                    truncated: false,
                    elapsed_ms: elapsed(started),
                });
            }
            None => {
                let text = inner.text().await.map_err(|e| read_error(&e))?;
                if text.contains("DB::Exception") {
                    return Err(server_error(&text));
                }
                sink.event(StreamEvent::Done {
                    index,
                    affected: written.filter(|n| *n > 0),
                    rows: 0,
                    truncated: false,
                    elapsed_ms: elapsed(started),
                });
            }
        }
        Ok(())
    }
}

fn read_error(e: &reqwest::Error) -> EngineError {
    EngineError::server(format!("Reading the result failed: {e}"), None)
}
