//! Application log: `tracing` events written to a daily rolling file in the data directory (14 files kept), plus stderr
//! in debug builds. The level comes from `FOSTA_STUDIO_LOG` (an `EnvFilter` directive, default `info`) and can be raised
//! to `debug` at run time from Settings.
//!
//! What is logged: one span per command and per engine call (engine id, a hash of the connection id, the operation, its
//! duration, row counts and the error code). Never passwords, keys, tokens or row data; statement text only at `debug`
//! and truncated ([`statement_excerpt`]). Panics are logged with a backtrace by the panic hook (`runtime::contain`).
//! Warnings and errors are also kept in memory for `diagnostics()`.

use std::{
    collections::VecDeque,
    future::Future,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Instant,
};

use serde::Serialize;
use tracing::{field::Visit, Event, Level, Subscriber};
use tracing_subscriber::{
    filter::EnvFilter,
    fmt,
    layer::{Context, SubscriberExt},
    reload,
    util::SubscriberInitExt,
    Layer, Registry,
};

use crate::{error::AppResult, lock::lock};

/// Environment variable with the log filter (`info`, `debug`, `fosta_studio=trace,warn`, …).
pub const LOG_ENV: &str = "FOSTA_STUDIO_LOG";
const RECENT_ERRORS: usize = 50;
const STATEMENT_EXCERPT: usize = 300;
const KEEP_FILES: usize = 14;

/// Where the log files go for each front-end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLocation {
    /// `<data dir>/fosta-studio-logs`, next to `fosta-studio.json`.
    Desktop,
    /// `<data dir>/logs` (`/data/logs` in the Docker image).
    Web,
}

struct State {
    dir: PathBuf,
    reload: reload::Handle<EnvFilter, Registry>,
    base: String,
}

static STATE: OnceLock<State> = OnceLock::new();
static RECENT: Mutex<VecDeque<RecentError>> = Mutex::new(VecDeque::new());

/// A warning or error kept for `diagnostics()`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentError {
    pub time: String,
    pub level: String,
    pub target: String,
    pub message: String,
    pub code: Option<String>,
}

/// Installs the global subscriber once; later calls (and calls when another subscriber is set, as in tests) return the
/// directory without changing anything. Returns the log directory.
pub fn init(data_dir: &Path, location: LogLocation) -> PathBuf {
    let dir = match location {
        LogLocation::Desktop => data_dir.join("fosta-studio-logs"),
        LogLocation::Web => data_dir.join("logs"),
    };
    if let Some(state) = STATE.get() {
        return state.dir.clone();
    }
    let base = std::env::var(LOG_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "info".to_owned());
    let filter = EnvFilter::try_new(&base).unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter, handle) = reload::Layer::new(filter);
    let file = std::fs::create_dir_all(&dir).ok().and_then(|_| {
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("fosta-studio")
            .filename_suffix("log")
            .max_log_files(KEEP_FILES)
            .build(&dir)
            .ok()
    });
    let file_layer = file.map(|appender| fmt::layer().with_ansi(false).with_target(true).with_writer(appender));
    let stderr_layer = cfg!(debug_assertions).then(|| fmt::layer().with_writer(std::io::stderr));
    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .with(RecentLayer)
        .try_init()
        .is_ok();
    if installed {
        let _ = STATE.set(State {
            dir: dir.clone(),
            reload: handle,
            base,
        });
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS,
            "Fosta Studio started"
        );
    }
    dir
}

/// The log directory, once logging is set up.
pub fn log_dir() -> Option<PathBuf> {
    STATE.get().map(|state| state.dir.clone())
}

/// Raises the level to `debug` (Settings → Debug logging) or returns to `FOSTA_STUDIO_LOG`.
pub fn set_debug(enabled: bool) -> AppResult<()> {
    let Some(state) = STATE.get() else { return Ok(()) };
    let directive = if enabled { "debug" } else { state.base.as_str() };
    let filter = EnvFilter::try_new(directive).unwrap_or_else(|_| EnvFilter::new("info"));
    state
        .reload
        .reload(filter)
        .map_err(|error| crate::error::AppError::new(format!("Cannot change the log level: {error}")))?;
    tracing::info!(debug = enabled, "log level changed");
    Ok(())
}

/// The newest warnings and errors, oldest first.
pub fn recent_errors() -> Vec<RecentError> {
    lock(&RECENT).iter().cloned().collect()
}

/// Records an error directly (used where no subscriber may be installed, e.g. the panic hook during startup).
pub fn remember(level: &str, target: &str, message: &str, code: Option<&str>) {
    let mut recent = lock(&RECENT);
    if recent.len() >= RECENT_ERRORS {
        recent.pop_front();
    }
    recent.push_back(RecentError {
        time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        level: level.to_owned(),
        target: target.to_owned(),
        message: message.to_owned(),
        code: code.map(str::to_owned),
    });
}

/// Keeps WARN and ERROR events in memory.
struct RecentLayer;

#[derive(Default)]
struct MessageVisitor {
    message: String,
    code: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_owned(),
            "code" => self.code = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "code" => self.code = Some(format!("{value:?}").trim_matches('"').to_owned()),
            "error" if self.message.is_empty() => self.message = format!("{value:?}"),
            _ => {}
        }
    }
}

impl<S: Subscriber> Layer<S> for RecentLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::WARN {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        remember(
            &level.to_string(),
            event.metadata().target(),
            &visitor.message,
            visitor.code.as_deref(),
        );
    }
}

/// A short, stable, non-reversible tag for a connection id (ids can contain user-chosen names).
pub fn id_hash(id: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

/// Statement text for `debug` logs: whitespace collapsed and cut.
pub fn statement_excerpt(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= STATEMENT_EXCERPT {
        collapsed
    } else {
        let mut cut: String = collapsed.chars().take(STATEMENT_EXCERPT).collect();
        cut.push('…');
        cut
    }
}

/// What a command returned, for its log line.
pub trait Outcome {
    /// `Some((code, message))` when the command failed.
    fn failure(&self) -> Option<(String, String)> {
        None
    }
}

impl<T> Outcome for AppResult<T> {
    fn failure(&self) -> Option<(String, String)> {
        self.as_ref()
            .err()
            .map(|error| (error.code().unwrap_or("ERROR").to_owned(), error.to_string()))
    }
}

macro_rules! infallible_outcome {
    ($($output:ty),* $(,)?) => { $(impl Outcome for $output {})* };
}

infallible_outcome!(
    (),
    String,
    crate::config::AppConfig,
    crate::Capabilities,
    Vec<String>,
    Vec<crate::tasks::TaskStatus>,
    crate::tasks::TaskStatus,
    crate::commands::diagnostics::Diagnostics,
    Vec<crate::contract::EngineDescriptor>,
    crate::mcp::McpStatus
);

fn log_command<T: Outcome>(name: &str, started: Instant, output: &T) {
    let ms = started.elapsed().as_millis() as u64;
    match output.failure() {
        // Gateway confirmations and failed update checks (offline) are part of the normal flow; everything else is worth a warning.
        Some((code, message))
            if matches!(
                code.as_str(),
                "CONFIRMATION_REQUIRED" | "PASSWORD_REQUIRED" | crate::update::CHECK_FAILED
            ) =>
        {
            tracing::debug!(command = name, ms, code = code.as_str(), "{message}")
        }
        Some((code, message)) => tracing::warn!(command = name, ms, code = code.as_str(), "{message}"),
        None => tracing::debug!(command = name, ms, "ok"),
    }
}

/// Runs a synchronous command and logs its duration and outcome.
pub fn command_sync<T: Outcome>(name: &str, run: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let output = run();
    log_command(name, started, &output);
    output
}

/// Runs an async command inside a `command` span and logs its duration and outcome.
pub async fn command<T: Outcome>(name: &'static str, fut: impl Future<Output = T>) -> T {
    use tracing::Instrument as _;
    let started = Instant::now();
    let output = fut.instrument(tracing::info_span!("command", name)).await;
    log_command(name, started, &output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_excerpts_hide_detail() {
        assert_eq!(id_hash("local-mariadb"), id_hash("local-mariadb"));
        assert_ne!(id_hash("a"), id_hash("b"));
        assert_eq!(id_hash("x").len(), 8);
        let long = "SELECT ".repeat(200);
        assert!(statement_excerpt(&long).ends_with('…'));
        assert_eq!(statement_excerpt("SELECT\n   1"), "SELECT 1");
    }

    #[test]
    fn recent_errors_are_capped() {
        for i in 0..(RECENT_ERRORS + 5) {
            remember("ERROR", "test", &format!("e{i}"), Some("X"));
        }
        let recent = recent_errors();
        assert_eq!(recent.len(), RECENT_ERRORS);
        assert!(recent.last().unwrap().message.starts_with('e'));
    }
}
