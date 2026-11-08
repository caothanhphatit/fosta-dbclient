//! Streaming a run's results: events, the sink that receives them with back-pressure, and helpers that enforce the row
//! limit and move blocking clients (SQLite, Oracle) onto Tokio's blocking pool.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{
    describe::ColumnInfo,
    errors::{EngineError, EngineResult},
    value::Value,
};

/// Per-run limits, from the workspace's safety rules (`Limits::run_options`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    /// Stop reading each result set after this many rows (the rest is cancelled or discarded on the server).
    pub row_limit: usize,
    /// Rows per streamed batch.
    pub batch_size: usize,
    /// Server-side (or client-enforced) limit per statement.
    pub timeout: Option<Duration>,
    /// Put the session in read-only mode where the engine has one.
    pub read_only: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            row_limit: 1000,
            batch_size: 200,
            timeout: None,
            read_only: false,
        }
    }
}

/// What a session streams while it runs a script.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum StreamEvent {
    /// A statement started; `index` counts statements in the script.
    Statement {
        index: usize,
        sql: String,
    },
    /// A result set starts.
    Columns {
        index: usize,
        columns: Vec<ColumnInfo>,
    },
    Rows {
        index: usize,
        rows: Vec<Vec<Value>>,
    },
    /// A statement finished. `affected` is set for statements that change rows; `truncated` when rows were left unread.
    #[serde(rename_all = "camelCase")]
    Done {
        index: usize,
        affected: Option<u64>,
        rows: usize,
        truncated: bool,
        elapsed_ms: u64,
    },
    /// A server message (SQL Server `PRINT`, PostgreSQL `NOTICE`, MySQL warnings).
    Notice {
        index: usize,
        text: String,
    },
}

/// Whether the consumer wants more rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// Receives a run's events in order. Returning [`Flow::Stop`] stops reading the current result set early and ends the run
/// at the next statement boundary.
pub trait ResultSink: Send {
    fn event(&mut self, event: StreamEvent) -> Flow;
}

impl<F: FnMut(StreamEvent) -> Flow + Send> ResultSink for F {
    fn event(&mut self, event: StreamEvent) -> Flow {
        self(event)
    }
}

/// A finished result set, for callers that want everything in memory.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSet {
    pub sql: String,
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<Value>>,
    pub affected: Option<u64>,
    pub truncated: bool,
    pub notices: Vec<String>,
}

/// A sink that keeps every result set in memory.
#[derive(Debug, Default)]
pub struct CollectSink {
    pub sets: Vec<ResultSet>,
    pub batches: usize,
}

impl ResultSink for CollectSink {
    fn event(&mut self, event: StreamEvent) -> Flow {
        match event {
            StreamEvent::Statement { sql, .. } => self.sets.push(ResultSet { sql, ..Default::default() }),
            StreamEvent::Columns { columns, .. } => {
                if let Some(set) = self.sets.last_mut() {
                    set.columns = columns;
                }
            }
            StreamEvent::Rows { rows, .. } => {
                self.batches += 1;
                if let Some(set) = self.sets.last_mut() {
                    set.rows.extend(rows);
                }
            }
            StreamEvent::Done { affected, truncated, .. } => {
                if let Some(set) = self.sets.last_mut() {
                    set.affected = affected;
                    set.truncated = truncated;
                }
            }
            StreamEvent::Notice { text, .. } => {
                if let Some(set) = self.sets.last_mut() {
                    set.notices.push(text);
                }
            }
        }
        Flow::Continue
    }
}

/// Fills `StreamEvent::Rows` batches from a row source and enforces the row limit. Returns `(rows_kept, truncated)` once the
/// source is exhausted or the limit or the consumer stops it; `next` is called until it returns `None` or the batch loop ends.
pub struct Batcher {
    pub index: usize,
    pub limit: usize,
    pub batch_size: usize,
    pub kept: usize,
    pub stopped: bool,
    buffer: Vec<Vec<Value>>,
}

impl Batcher {
    pub fn new(index: usize, opts: &RunOptions) -> Self {
        Self {
            index,
            limit: opts.row_limit,
            batch_size: opts.batch_size.max(1),
            kept: 0,
            stopped: false,
            buffer: Vec::new(),
        }
    }

    /// Adds a row. Returns false once no more rows are wanted (limit reached or the sink said stop).
    pub fn push(&mut self, row: Vec<Value>, sink: &mut dyn ResultSink) -> bool {
        if self.stopped || self.kept >= self.limit {
            self.stopped = true;
            return false;
        }
        self.buffer.push(row);
        self.kept += 1;
        if self.buffer.len() >= self.batch_size || self.kept >= self.limit {
            self.flush(sink);
        }
        !self.stopped && self.kept < self.limit
    }

    pub fn flush(&mut self, sink: &mut dyn ResultSink) {
        if self.buffer.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.buffer);
        if sink.event(StreamEvent::Rows { index: self.index, rows }) == Flow::Stop {
            self.stopped = true;
        }
    }

    /// True when the reader stopped before the source ended.
    pub fn wants_more(&self) -> bool {
        !self.stopped && self.kept < self.limit
    }
}

/// Runs a blocking statement loop (SQLite, Oracle) on Tokio's blocking pool and forwards its events to `sink` with
/// backpressure. When `sink` answers [`Flow::Stop`], the worker's next event returns `Stop` so it stops reading.
pub async fn run_blocking<T, F>(sink: &mut dyn ResultSink, work: F) -> EngineResult<T>
where
    T: Send + 'static,
    F: FnOnce(&mut dyn ResultSink) -> EngineResult<T> + Send + 'static,
{
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    struct ChannelSink {
        tx: tokio::sync::mpsc::Sender<StreamEvent>,
        stop: Arc<AtomicBool>,
    }
    impl ResultSink for ChannelSink {
        fn event(&mut self, event: StreamEvent) -> Flow {
            if self.tx.blocking_send(event).is_err() || self.stop.load(Ordering::Relaxed) {
                Flow::Stop
            } else {
                Flow::Continue
            }
        }
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let stop = Arc::new(AtomicBool::new(false));
    let mut worker_sink = ChannelSink { tx, stop: stop.clone() };
    let handle = tokio::task::spawn_blocking(move || work(&mut worker_sink));
    while let Some(ev) = rx.recv().await {
        if sink.event(ev) == Flow::Stop {
            stop.store(true, Ordering::Relaxed);
        }
    }
    handle
        .await
        .map_err(|e| EngineError::internal(format!("The worker thread failed: {e}")))?
}

/// Runs blocking work (catalog queries, row changes) on Tokio's blocking pool.
pub async fn blocking<T, F>(work: F) -> EngineResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> EngineResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| EngineError::internal(format!("The worker thread failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batcher_stops_at_the_limit_and_flushes_in_batches() {
        let opts = RunOptions {
            row_limit: 5,
            batch_size: 2,
            ..RunOptions::default()
        };
        let mut b = Batcher::new(0, &opts);
        let mut sink = CollectSink::default();
        sink.event(StreamEvent::Statement { index: 0, sql: "x".into() });
        let mut pushed = 0;
        for i in 0..10 {
            pushed += 1;
            if !b.push(vec![Value::Int(i)], &mut sink) {
                break;
            }
        }
        b.flush(&mut sink);
        assert_eq!(pushed, 5);
        assert_eq!(sink.sets[0].rows.len(), 5);
        assert_eq!(sink.batches, 3);
        assert!(!b.wants_more());
    }

    #[test]
    fn events_are_tagged_with_camel_case_fields() {
        let done = StreamEvent::Done {
            index: 0,
            affected: None,
            rows: 1000,
            truncated: true,
            elapsed_ms: 31,
        };
        let json = serde_json::to_value(&done).unwrap();
        assert_eq!(json["event"], "done");
        assert_eq!(json["elapsedMs"], 31);
        let back: StreamEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, done);
    }
}
