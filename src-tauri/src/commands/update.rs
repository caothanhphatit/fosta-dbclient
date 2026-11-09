//! Settings → "Check for updates" and the status bar's update item (logic in [`crate::update`]).

use chrono::{SecondsFormat, Utc};

use crate::{
    config::AppConfig,
    error::{AppError, AppResult},
    update::{self, UpdateInfo, CURRENT_VERSION},
    AppState,
};

/// The latest release compared with this build. Without `force` (the check at startup) it asks GitHub at most once a
/// day and only when enabled; between checks it repeats a newer version found earlier. `None` when there is nothing to show.
pub async fn check_update(state: &AppState, force: bool) -> AppResult<Option<UpdateInfo>> {
    let settings = state.config.read(|config| config.update_check.clone());
    if !force && !settings.enabled {
        return Ok(None);
    }
    if !force && !update::check_due(settings.last_checked.as_deref(), Utc::now()) {
        return Ok(update::remembered(CURRENT_VERSION, &settings));
    }
    let body = update::fetch_latest_release().await?;
    let info = update::release_info(CURRENT_VERSION, &body)?;
    state.config.update(|config| {
        config.update_check.last_checked = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
        config.update_check.latest_version = info.as_ref().map(|info| info.latest.clone());
        Ok(())
    })?;
    Ok(info)
}

/// How this copy was installed: `"installer"` (NSIS, updates itself), `"portable"` or `"web"`.
pub fn install_kind(state: &AppState) -> String {
    if state.capabilities.edition == "web" {
        return "web".into();
    }
    let exe = std::env::current_exe().ok();
    update::install_kind_in(exe.as_deref().and_then(|exe| exe.parent())).into()
}

pub fn set_update_check(state: &AppState, enabled: bool) -> AppResult<AppConfig> {
    state.config.update(|config| {
        config.update_check.enabled = enabled;
        Ok(())
    })
}

/// Called once at startup: the version to announce ("What's new in X") when this is the first start after an update.
pub fn whats_new(state: &AppState) -> AppResult<Option<String>> {
    let mut settings = state.config.read(|config| config.update_check.clone());
    let (announce, changed) = update::record_run(&mut settings, CURRENT_VERSION);
    if changed {
        state.config.update(|config| {
            config.update_check.last_run_version = settings.last_run_version.take();
            Ok(())
        })?;
    }
    Ok(announce)
}

/// Desktop only: opens a release page of this repository in the default browser.
pub fn open_release_page(url: String) -> AppResult<()> {
    if !update::is_release_page(&url) && url != update::COMMUNITY_URL {
        return Err(AppError::new("Only Fosta Studio release pages and the community invite can be opened."));
    }
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let program = "xdg-open";
    std::process::Command::new(program).arg(&url).spawn()?;
    Ok(())
}
