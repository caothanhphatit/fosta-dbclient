//! Fosta Studio's backend core, shared by its two front-ends:
//!
//! - the desktop app (`main.rs`, feature `desktop`): a Tauri window; the commands are Tauri commands;
//! - the web server (`server/`, feature `web`): the Docker image; the commands are `POST /api/invoke/<command>`.
//!
//! Neither front-end adds behaviour of its own to a command: the bodies live in [`commands`], and both
//! front-ends are generated from [`for_each_command!`]. What differs is behind small traits: where secrets
//! are kept ([`secrets::SecretStore`]), how events reach the UI ([`events::EventEmitter`],
//! [`ai::ChatSink`]) and what the edition supports ([`Capabilities`]).

pub mod ai;
#[cfg(test)]
mod architecture_tests;
pub mod audit;
pub mod commands;
pub mod config;
pub mod connection_command;
pub mod contract;
pub mod data_files;
pub mod embedding;
pub mod engines;
pub mod error;
pub mod events;
pub mod export;
pub mod files;
pub mod gateway;
pub mod history;
mod job;
pub mod lang;
mod lock;
pub mod logging;
pub mod mcp;
pub mod net;
pub mod open_tabs;
#[cfg(test)]
mod perf_tests;
pub mod runtime;
pub mod secrets;
pub mod tasks;
pub mod update;

use std::sync::Arc;

use serde::Serialize;

use crate::{error::AppResult, events::EventEmitter};

pub struct AppState {
    pub config: config::ConfigStore,
    pub engines: runtime::ConnectionManager,
    pub tasks: Arc<tasks::TaskManager>,
    pub ai: ai::AiRuns,
    pub history: Arc<history::HistoryStore>,
    /// The open tabs of each workspace, restored on the next start.
    pub open_tabs: Arc<open_tabs::OpenTabsStore>,
    pub capabilities: Capabilities,
    /// The MCP server (`mcp`); `attach` it once the state is in an `Arc`.
    pub mcp: mcp::McpState,
}

impl AppState {
    /// Loads the config and the query history from the data directory and starts the active workspace's
    /// auto-start tasks. `executor` runs background work; `events` carries task events to the UI.
    pub fn new(capabilities: Capabilities, executor: tokio::runtime::Handle, events: Arc<dyn EventEmitter>) -> AppResult<Self> {
        // In a container 127.0.0.1 is the container itself, so a first start there seeds no connection.
        let config = config::ConfigStore::load(!capabilities.in_container)?;
        let mcp = mcp::McpState::new(Arc::clone(&events), executor.clone());
        let tasks = Arc::new(tasks::TaskManager::new(executor, events, capabilities.tasks));
        tasks.start_auto(&config.snapshot().active().tasks);
        if config.snapshot().debug_logging {
            let _ = logging::set_debug(true);
        }
        Ok(Self {
            config,
            engines: runtime::ConnectionManager::default(),
            tasks,
            ai: ai::AiRuns::default(),
            history: Arc::new(history::HistoryStore::open_default()?),
            open_tabs: Arc::new(open_tabs::OpenTabsStore::new(config::data_path(open_tabs::OPEN_TABS_FILE)?)),
            capabilities,
            mcp,
        })
    }

    /// A state whose config and history live in a fresh temporary folder (tests).
    #[cfg(test)]
    pub(crate) fn for_test(name: &str, capabilities: Capabilities, config: config::AppConfig) -> Self {
        let directory = std::env::temp_dir().join(format!("fosta-state-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("temporary folder");
        let tasks = tasks::TaskManager::new(tokio::runtime::Handle::current(), Arc::new(events::NoEvents), capabilities.tasks);
        Self {
            config: config::ConfigStore::at(directory.join("fosta-studio.json"), config),
            engines: runtime::ConnectionManager::default(),
            tasks: Arc::new(tasks),
            ai: ai::AiRuns::default(),
            history: Arc::new(history::HistoryStore::new(directory.join("history.jsonl"), 100)),
            open_tabs: Arc::new(open_tabs::OpenTabsStore::new(directory.join(open_tabs::OPEN_TABS_FILE))),
            capabilities,
            mcp: mcp::McpState::new(Arc::new(events::NoEvents), tokio::runtime::Handle::current()),
        }
    }
}

/// What the running edition supports; the UI reads it with the `capabilities` command and hides the rest.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// `"desktop"` or `"web"`.
    pub edition: &'static str,
    /// Workspace commands (child processes) can run.
    pub tasks: bool,
    /// "Show in folder" after an export.
    pub reveal_file: bool,
    /// Exports and diagram images are downloaded by the browser instead of written to ~/Downloads.
    pub browser_downloads: bool,
    /// Where saved passwords and API keys go, as shown in dialogs.
    pub secret_store: &'static str,
    /// The backend runs in a container: `localhost` there is not the user's machine.
    pub in_container: bool,
}

impl Capabilities {
    pub fn desktop() -> Self {
        Self {
            edition: "desktop",
            tasks: true,
            reveal_file: true,
            browser_downloads: false,
            secret_store: "Windows Credential Manager",
            in_container: false,
        }
    }

    pub fn web(tasks: bool, in_container: bool) -> Self {
        Self {
            edition: "web",
            tasks,
            reveal_file: false,
            browser_downloads: true,
            secret_store: "the server's encrypted secret store",
            in_container,
        }
    }
}
