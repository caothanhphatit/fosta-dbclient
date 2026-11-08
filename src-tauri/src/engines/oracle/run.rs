//! A query tab's session on the contract: `OraSession` in auto-commit mode, error positions mapped into the script, the
//! tab's database as the session's current schema, and the transaction controls of the toolbar.
//!
//! Oracle has no BEGIN: a transaction starts with the first change. "Begin" (and "auto-commit off") therefore switches the
//! session to keep its changes open until Commit or Rollback, and the tab reports that transaction as open from then on,
//! even before the first change.

use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use futures_util::future::BoxFuture;

use super::session::{OraCancel, OraSession, RunFailure};
use crate::{
    contract::{Canceller, ResultSink, RunOptions, RunRequest, Session, TransactionOp, TransactionState},
    engines::common::{EngineError, TxState},
    error::{AppError, AppResult, ErrorPosition},
};

pub struct OraContractSession {
    inner: OraSession,
    /// A transaction was begun from the toolbar (auto-commit is off until Commit / Rollback / auto-commit on).
    explicit: AtomicBool,
}

impl OraContractSession {
    pub fn new(inner: OraSession) -> Self {
        inner.set_auto_commit(true);
        Self {
            inner,
            explicit: AtomicBool::new(false),
        }
    }

    pub async fn use_schema(&self, schema: Option<&str>) -> AppResult<()> {
        self.inner.use_schema(schema).await.map_err(engine)
    }

    fn set_explicit(&self, on: bool) {
        self.explicit.store(on, Ordering::SeqCst);
        self.inner.set_auto_commit(!on);
    }

    async fn state(&self) -> TransactionState {
        if self.explicit.load(Ordering::SeqCst) {
            return TransactionState::from(TxState::Active);
        }
        TransactionState::from(self.inner.transaction_state().await)
    }
}

fn engine(error: EngineError) -> AppError {
    AppError::from(error).with_engine(super::descriptor::ID)
}

/// Characters in the first `bytes` bytes of `text` (Oracle reports parse offsets in bytes).
fn chars_before(text: &str, bytes: usize) -> usize {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].chars().count()
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
        .map(|position| chars_before(script, start) + chars_before(&text, position as usize));
    let mut app = engine(error);
    if let Some(offset) = offset {
        app = app.with_position(ErrorPosition::at_offset(script, offset));
    }
    if !text.is_empty() {
        app = app.with_statement(index, &text);
    }
    app
}

#[async_trait]
impl Session for OraContractSession {
    fn language(&self) -> &str {
        "sql"
    }

    async fn run(&self, request: &RunRequest, options: &RunOptions, sink: &mut dyn ResultSink) -> AppResult<()> {
        self.use_schema(request.database.as_deref()).await?;
        self.inner
            .run_detailed(&request.text, options, sink)
            .await
            .map_err(|failure| failure_error(*failure, &request.text))
    }

    async fn transaction(&self, op: TransactionOp, _options: &RunOptions) -> AppResult<TransactionState> {
        match op {
            TransactionOp::Begin => {
                self.state().await.ensure_can_begin()?;
                self.set_explicit(true);
            }
            TransactionOp::Commit => {
                self.inner.commit().await.map_err(engine)?;
                self.set_explicit(false);
            }
            TransactionOp::Rollback => {
                self.inner.rollback().await.map_err(engine)?;
                self.set_explicit(false);
            }
            TransactionOp::AutoCommitOff => self.set_explicit(true),
            TransactionOp::AutoCommitOn => {
                if self.state().await.open {
                    self.inner.commit().await.map_err(engine)?;
                }
                self.set_explicit(false);
            }
            TransactionOp::Status => {}
        }
        Ok(self.state().await)
    }

    async fn transaction_state(&self) -> TransactionState {
        self.state().await
    }

    async fn close(&self) {
        self.inner.close().await;
    }
}

/// Breaks the running call (OCIBreak) from another thread; the session's own work is not waited for.
pub struct OraCanceller(pub OraCancel);

impl Canceller for OraCanceller {
    fn cancel(&self) -> BoxFuture<'static, AppResult<()>> {
        let cancel = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || cancel.cancel())
                .await
                .map_err(|error| AppError::new(format!("The worker thread failed: {error}")))?
                .map_err(engine)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::common::ErrorKind;

    #[test]
    fn error_positions_are_relative_to_the_whole_script() {
        let script = "SELECT 1 FROM dual;\nSELECT 'é' FORM dual";
        let error = EngineError {
            code: Some("ORA-00923".into()),
            // Byte offset of FORM in the second statement: 'é' takes two bytes.
            position: Some(12),
            ..EngineError::new(ErrorKind::Server, "FROM keyword not found where expected")
        };
        let app = failure_error(
            RunFailure {
                error,
                index: 1,
                text: "SELECT 'é' FORM dual".into(),
                start: 20,
            },
            script,
        );
        let payload = app.payload().unwrap();
        assert_eq!(payload["vendorCode"], "ORA-00923");
        assert_eq!(payload["position"]["offset"], 31);
        assert_eq!(payload["position"]["line"], 2);
        assert_eq!(payload["position"]["column"], 12);
        assert_eq!(payload["statement"]["index"], 1);
        assert_eq!(payload["engine"], "oracle");
    }
}
