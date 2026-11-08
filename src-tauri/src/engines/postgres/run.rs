//! A query tab's session on the contract: `PgSession` plus error positions mapped into the script and the transaction
//! controls of the toolbar.

use async_trait::async_trait;
use futures_util::future::BoxFuture;

use super::session::{PgCancel, PgSession, RunFailure};
use crate::{
    contract::{Canceller, ResultSink, RunOptions, RunRequest, Session, TransactionOp, TransactionState},
    engines::common::EngineError,
    error::{AppError, AppResult, ErrorPosition},
};

pub struct PgContractSession {
    pub inner: PgSession,
}

/// The failed statement's error with its position in the whole script (line, column, offset) and the statement.
pub fn failure_error(failure: RunFailure, script: &str) -> AppError {
    let RunFailure {
        mut error,
        index,
        text,
        start,
    } = failure;
    let offset = error
        .position
        .take()
        .map(|position| script.get(..start).map_or(0, |before| before.chars().count()) + position as usize);
    let mut app = AppError::from(error).with_engine(super::descriptor::ID);
    if let Some(offset) = offset {
        app = app.with_position(ErrorPosition::at_offset(script, offset));
    }
    if !text.is_empty() {
        app = app.with_statement(index, &text);
    }
    app
}

impl PgContractSession {
    async fn state(&self) -> TransactionState {
        TransactionState::from(self.inner.refresh_state().await)
    }
}

#[async_trait]
impl Session for PgContractSession {
    fn language(&self) -> &str {
        "sql"
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        self.inner
            .run_detailed(&request.text, options, sink)
            .await
            .map_err(|failure| failure_error(*failure, &request.text))
    }

    async fn transaction(&self, op: TransactionOp, options: &RunOptions) -> AppResult<TransactionState> {
        let engine = |error: EngineError| AppError::from(error).with_engine(super::descriptor::ID);
        match op {
            TransactionOp::Begin => {
                self.state().await.ensure_can_begin()?;
                self.inner.apply_limits(options).await.map_err(engine)?;
                self.inner.begin().await.map_err(engine)?;
            }
            TransactionOp::Commit => self.inner.commit().await.map_err(engine)?,
            TransactionOp::Rollback => self.inner.rollback().await.map_err(engine)?,
            // PostgreSQL has no auto-commit setting to switch: "off" is a transaction that stays open until it is
            // committed, "on" ends it.
            TransactionOp::AutoCommitOff => {
                if !self.state().await.open {
                    self.inner.apply_limits(options).await.map_err(engine)?;
                    self.inner.begin().await.map_err(engine)?;
                }
            }
            TransactionOp::AutoCommitOn => {
                if self.state().await.open {
                    self.inner.commit().await.map_err(engine)?;
                }
            }
            TransactionOp::Status => {}
        }
        Ok(self.state().await)
    }

    async fn transaction_state(&self) -> TransactionState {
        TransactionState::from(self.inner.transaction_state())
    }
}

/// Sends PostgreSQL's CancelRequest on a separate connection, so the session's own lock is not needed.
pub struct PgCanceller(pub PgCancel);

impl Canceller for PgCanceller {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>> {
        let cancel = self.0.clone();
        Box::pin(async move { cancel.cancel().await.map_err(AppError::from) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::common::ErrorKind;

    #[test]
    fn error_positions_are_relative_to_the_whole_script() {
        let script = "SELECT 1;\nSELEC 2";
        let error = EngineError {
            code: Some("42601".into()),
            position: Some(0),
            ..EngineError::new(ErrorKind::Server, "syntax error at or near \"SELEC\"")
        };
        let app = failure_error(
            RunFailure {
                error,
                index: 1,
                text: "SELEC 2".into(),
                start: 10,
            },
            script,
        );
        let payload = app.payload().unwrap();
        assert_eq!(payload["vendorCode"], "42601");
        assert_eq!(payload["position"]["offset"], 10);
        assert_eq!(payload["position"]["line"], 2);
        assert_eq!(payload["statement"]["index"], 1);
        assert_eq!(payload["engine"], "postgres");
    }
}
