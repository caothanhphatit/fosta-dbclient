//! The Oracle Instant Client as the commands see it: status, an on-demand download with progress events, and cancel.
//! Commands stay engine-neutral (they never name `engines::oracle`), so they call these, which forward to the Oracle engine
//! module when it is compiled in and say so when it is not.

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::error::{AppError, AppResult};

/// Error code of every Instant Client failure; `detail.kind` says which (`notInstalled`, `network`, `cancelled`, ...).
pub const ORACLE_CLIENT: &str = "ORACLE_CLIENT";

/// Set by [`cancel`], cleared when a download starts.
static CANCEL: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "engine-oracle")]
mod imp {
    use super::*;
    use crate::engines::oracle::instant_client::{self, ClientError};

    fn to_value(value: impl serde::Serialize) -> AppResult<Value> {
        serde_json::to_value(value).map_err(|error| AppError::new(error.to_string()))
    }

    fn client_error(error: ClientError) -> AppError {
        AppError::coded(ORACLE_CLIENT, error.message.clone())
            .with_engine("oracle")
            .with("kind", error.kind)
    }

    pub fn status() -> AppResult<Value> {
        to_value(instant_client::status(None))
    }

    pub async fn download(progress: &mut (dyn FnMut(Value) + Send)) -> AppResult<Value> {
        let mut report = |event: instant_client::Progress| {
            if let Ok(value) = serde_json::to_value(event) {
                progress(value);
            }
        };
        instant_client::download(&mut report, &CANCEL).await.map_err(client_error)?;
        // Load it now, so the next connect finds it without a restart.
        to_value(instant_client::initialize(None).map_err(client_error)?)
    }
}

#[cfg(not(feature = "engine-oracle"))]
mod imp {
    use super::*;

    fn not_in_build() -> AppError {
        AppError::coded(ORACLE_CLIENT, "Oracle is not part of this build.").with_engine("oracle")
    }

    pub fn status() -> AppResult<Value> {
        Err(not_in_build())
    }

    pub async fn download(_progress: &mut (dyn FnMut(Value) + Send)) -> AppResult<Value> {
        Err(not_in_build())
    }
}

/// Whether a client is available and loaded, and what a download would fetch (`ClientStatus` as JSON).
pub fn status() -> AppResult<Value> {
    imp::status()
}

/// Downloads, verifies, installs and loads the pinned client for this platform, reporting `Progress` events; resolves with
/// the new status. A cancelled download resumes from its partial file next time.
pub async fn download(progress: &mut (dyn FnMut(Value) + Send)) -> AppResult<Value> {
    CANCEL.store(false, Ordering::SeqCst);
    imp::download(progress).await
}

/// Stops a running download (it fails with kind `cancelled`).
pub fn cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}
