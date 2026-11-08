//! Crash containment (ADR 0002): every engine call runs in its own task, so a panic inside a driver becomes an
//! `ENGINE_CRASHED` error for that call instead of closing the app. The connection that panicked is marked poisoned by
//! its owner and refuses further calls until the user connects again. Panics go to the application log with a backtrace.

use std::{any::Any, future::Future, sync::OnceLock, time::Instant};

use serde_json::json;
use tracing::Instrument as _;

use crate::error::{AppError, AppResult};

pub const ENGINE_CRASHED: &str = "ENGINE_CRASHED";

/// Runs `fut` in its own task. A panic in it becomes a structured `ENGINE_CRASHED` error naming the engine and what it
/// was doing (`what` reads after "while it", e.g. "listed the tables"). Each call is logged with its duration.
pub async fn contained<T: Send + 'static>(
    engine: &str,
    what: &str,
    fut: impl Future<Output = AppResult<T>> + Send + 'static,
) -> AppResult<T> {
    let started = Instant::now();
    let span = tracing::debug_span!("engine", engine, op = what);
    let outcome = match tokio::spawn(fut.instrument(span)).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => Err(crashed(engine, what, &panic_text(error.into_panic()))),
        Err(_) => Err(AppError::new("The operation was cancelled")),
    };
    let ms = started.elapsed().as_millis() as u64;
    match &outcome {
        Ok(_) => tracing::debug!(engine, op = what, ms, "engine call"),
        Err(error) if is_crash(error) => tracing::error!(engine, op = what, ms, code = ENGINE_CRASHED, "{error}"),
        Err(error) => tracing::info!(
            engine,
            op = what,
            ms,
            code = error.code().unwrap_or("ERROR"),
            "engine call failed: {error}"
        ),
    }
    outcome
}

/// The error returned for a panic, and for every later call on the poisoned connection. `engine` is the label used in
/// the message; the caller adds the engine id (`AppError::with_engine`).
pub fn crashed(engine: &str, what: &str, detail: &str) -> AppError {
    AppError::structured(
        format!("The {engine} driver failed unexpectedly while it {what}. The connection was closed; connect again."),
        json!({ "code": ENGINE_CRASHED, "detail": detail }),
    )
}

/// Whether `error` is the result of a contained panic.
pub fn is_crash(error: &AppError) -> bool {
    error.code() == Some(ENGINE_CRASHED)
}

fn panic_text(payload: Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic".to_owned()
    }
}

static HOOK: OnceLock<()> = OnceLock::new();

/// Chains a hook in front of the default one that writes the panic (thread, message, location, backtrace) to the
/// application log and keeps it for `diagnostics()`. Installed once at startup by both front-ends.
pub fn install_panic_hook() {
    if HOOK.set(()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("unnamed").to_owned();
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(target: "panic", code = ENGINE_CRASHED, thread, location, "panic: {message}\n{backtrace}");
        crate::logging::remember("ERROR", "panic", &format!("panic at {location}: {message}"), Some(ENGINE_CRASHED));
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panic_becomes_engine_crashed() {
        let error = contained("MariaDB", "listed the tables", async {
            if std::hint::black_box(true) {
                panic!("index out of bounds");
            }
            Ok(1)
        })
        .await
        .expect_err("panicked");
        assert!(is_crash(&error));
        let detail = error.payload().expect("structured");
        assert_eq!(detail["detail"], "index out of bounds");
        assert!(error.to_string().contains("while it listed the tables"), "{error}");
    }

    #[tokio::test]
    async fn results_and_errors_pass_through() {
        assert_eq!(contained("x", "ran", async { Ok(7) }).await.unwrap(), 7);
        let error = contained::<()>("x", "ran", async { Err(AppError::new("boom")) }).await.unwrap_err();
        assert_eq!(error.to_string(), "boom");
        assert!(!is_crash(&error));
    }
}
