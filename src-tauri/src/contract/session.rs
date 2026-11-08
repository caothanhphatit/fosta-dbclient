//! Query sessions: one per query tab, with its own server connection, transaction state and canceller.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use super::stream::{ResultSink, RunOptions};
use crate::error::{AppError, AppResult};

/// Transaction state of an engine session, reported after each run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TxState {
    /// Autocommit: every statement commits on its own.
    Idle,
    /// Inside an explicit transaction; changes are pending until COMMIT.
    Active,
    /// Inside a transaction that failed; only ROLLBACK (or ROLLBACK TO SAVEPOINT) works.
    Failed,
    /// The engine has no transactions (ClickHouse).
    Unsupported,
}

/// A transaction control from the query toolbar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransactionOp {
    Begin,
    Commit,
    Rollback,
    AutoCommitOn,
    AutoCommitOff,
    /// Only reads the state.
    Status,
}

/// Transaction state of a query-tab session, read from the engine (`@@in_transaction`, `@@TRANCOUNT`), never guessed by
/// the UI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionState {
    /// False when the engine (or this driver) cannot report or control transactions.
    pub supported: bool,
    pub open: bool,
    pub auto_commit: bool,
    /// The open transaction failed; only a rollback works (PostgreSQL).
    #[serde(default)]
    pub failed: bool,
}

pub const TRANSACTION_ALREADY_OPEN: &str = "A transaction is already open in this tab. Commit or roll it back before starting a new one.";

impl TransactionState {
    pub fn unsupported() -> Self {
        Self {
            supported: false,
            open: false,
            auto_commit: true,
            failed: false,
        }
    }

    /// Refuses Begin while a transaction is open: MySQL/MariaDB would silently commit it (`START TRANSACTION` ends the
    /// current one) and SQL Server would nest it, so a later Commit or Rollback would not do what the user expects.
    pub fn ensure_can_begin(&self) -> AppResult<()> {
        if self.open {
            return Err(AppError::new(TRANSACTION_ALREADY_OPEN));
        }
        Ok(())
    }
}

impl From<TxState> for TransactionState {
    fn from(state: TxState) -> Self {
        match state {
            TxState::Idle => Self {
                supported: true,
                open: false,
                auto_commit: true,
                failed: false,
            },
            TxState::Active => Self {
                supported: true,
                open: true,
                auto_commit: false,
                failed: false,
            },
            TxState::Failed => Self {
                supported: true,
                open: true,
                auto_commit: false,
                failed: true,
            },
            TxState::Unsupported => Self::unsupported(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOptions {
    /// A language id from the descriptor; the default language when `None`.
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub language: String,
    pub transaction: TransactionState,
    pub cancellable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRequest {
    pub text: String,
    #[serde(default)]
    pub database: Option<String>,
    /// Start of the selection in the editor, so error positions can be mapped back.
    #[serde(default)]
    pub offset: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub statements: usize,
    pub elapsed_ms: u64,
    /// The consumer stopped the run (closed tab, cancelled request).
    pub stopped: bool,
    pub transaction: TransactionState,
}

/// One query tab's session. `run` streams events into `sink`; errors after some statements ran are returned after the
/// events of the statements that finished.
#[async_trait]
pub trait Session: Send + Sync + 'static {
    fn language(&self) -> &str;
    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()>;
    async fn transaction(&self, _op: TransactionOp, _options: &RunOptions) -> AppResult<TransactionState> {
        Ok(TransactionState::unsupported())
    }
    async fn transaction_state(&self) -> TransactionState {
        TransactionState::unsupported()
    }
    async fn close(&self) {}
}

/// Cancels the statement a session is running. Must not wait for the session's own lock (the run holds it).
pub trait Canceller: Send + Sync {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>>;
}

pub struct OpenedSession {
    pub session: Arc<dyn Session>,
    pub canceller: Option<Arc<dyn Canceller>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_states_map_to_the_toolbar_state() {
        assert!(TransactionState::from(TxState::Failed).failed);
        assert!(TransactionState::from(TxState::Active).open);
        assert!(!TransactionState::from(TxState::Unsupported).supported);
        let open = TransactionState::from(TxState::Active);
        assert_eq!(open.ensure_can_begin().unwrap_err().to_string(), TRANSACTION_ALREADY_OPEN);
        assert_eq!(
            serde_json::to_value(TransactionOp::AutoCommitOff).unwrap(),
            serde_json::json!("autoCommitOff")
        );
    }
}
