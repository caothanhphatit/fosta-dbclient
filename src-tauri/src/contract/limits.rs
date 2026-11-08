//! Engine-level backstops set by the workspace's safety rules, and the context every engine call receives.

use std::time::{Duration, Instant};

use super::stream::RunOptions;

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
#[derive(Clone, Debug, Default)]
pub struct CallContext {
    pub limits: Limits,
    pub deadline: Option<Instant>,
}

impl CallContext {
    pub fn new(limits: Limits) -> Self {
        Self { limits, deadline: None }
    }
}
