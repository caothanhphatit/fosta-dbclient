//! Settings → "Copy diagnostics", "Open log folder" and "Debug logging".

use serde::Serialize;

use crate::{
    config::AppConfig,
    error::{AppError, AppResult},
    logging::{self, RecentError},
    runtime::manager::OpenConnection,
    AppState,
};

/// What a bug report needs, without secrets or row data.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub app_version: &'static str,
    pub edition: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub os_version: Option<String>,
    pub log_folder: Option<String>,
    pub debug_logging: bool,
    pub connections: Vec<OpenConnection>,
    pub recent_errors: Vec<RecentError>,
}

pub fn diagnostics(state: &AppState) -> Diagnostics {
    Diagnostics {
        app_version: env!("CARGO_PKG_VERSION"),
        edition: state.capabilities.edition,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        os_version: os_version(),
        log_folder: logging::log_dir().map(|dir| dir.display().to_string()),
        debug_logging: state.config.snapshot().debug_logging,
        connections: state.engines.open_connections(),
        recent_errors: logging::recent_errors(),
    }
}

fn os_version() -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = std::process::Command::new("cmd")
            .args(["/C", "ver"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
    #[cfg(not(windows))]
    {
        let text = std::fs::read_to_string("/etc/os-release").ok()?;
        text.lines()
            .find_map(|line| line.strip_prefix("PRETTY_NAME="))
            .map(|name| name.trim_matches('"').to_owned())
    }
}

/// Turns debug-level logging on or off now and at the next start.
pub fn set_debug_logging(state: &AppState, enabled: bool) -> AppResult<AppConfig> {
    logging::set_debug(enabled)?;
    state.config.update(|config| {
        config.debug_logging = enabled;
        Ok(())
    })
}

/// Desktop only: opens the log folder in the file manager.
pub fn open_log_folder() -> AppResult<()> {
    let dir = logging::log_dir().ok_or_else(|| AppError::new("Logging is not set up in this process."))?;
    std::fs::create_dir_all(&dir)?;
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program).arg(&dir).spawn()?;
    Ok(())
}
