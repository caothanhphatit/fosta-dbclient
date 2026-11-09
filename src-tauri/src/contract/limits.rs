//! Engine-level backstops set by the workspace's safety rules, and the context every engine call receives.

use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use super::stream::RunOptions;
use crate::error::AppError;

pub const CANCELLED: &str = "CANCELLED";

/// The error of a call that was cancelled (a replaced page, a closed tab, a stopped export).
pub fn cancelled() -> AppError {
    AppError::coded(CANCELLED, "The request was cancelled")
}

/// A cancel signal shared by a caller and the call it started; clones share it.
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<CancelState>);

#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Completes once [`cancel`](Self::cancel) was called (at once when it already was).
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CancellationToken").field(&self.is_cancelled()).finish()
    }
}

const DEFAULT_MAX_ROWS: usize = 1000;
const DEFAULT_BATCH_SIZE: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Rows kept per result set in query tabs.
    pub max_rows: usize,
    /// Statement timeout in seconds; 0 = none.
    pub timeout_secs: u32,
    /// Put sessions in read-only mode where the engine has one.
    pub read_only: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            timeout_secs: 0,
            read_only: false,
        }
    }
}

impl Limits {
    pub fn run_options(&self) -> RunOptions {
        RunOptions {
            row_limit: self.max_rows,
            batch_size: DEFAULT_BATCH_SIZE,
            timeout: (self.timeout_secs > 0).then(|| Duration::from_secs(u64::from(self.timeout_secs))),
            read_only: self.read_only,
        }
    }
}

/// Passed to every engine call: the rules' backstops and an optional deadline.
/// Engines that can stop the server's work (MariaDB/MySQL `KILL QUERY`, PostgreSQL's CancelRequest) watch `cancel`;
/// the caller ends the others' task.
#[derive(Clone, Debug, Default)]
pub struct CallContext {
    pub limits: Limits,
    pub deadline: Option<Instant>,
    pub cancel: CancellationToken,
}

impl CallContext {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            deadline: None,
            cancel: CancellationToken::new(),
        }
    }

    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }
}
