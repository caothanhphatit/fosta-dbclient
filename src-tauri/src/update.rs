//! Update check, shared by every edition: asks GitHub for the latest release and compares it with this build.
//!
//! The request carries only a `User-Agent` of `fosta-studio/<version>`. The automatic check runs at most once a day
//! (Settings → Check for updates); the manual one always runs. Installing is the UI's job: the installed desktop app
//! downloads through the Tauri updater, the portable app and the web edition only link to the release.

use std::{path::Path, time::Duration};

use chrono::{DateTime, Utc};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/caothanhphatit/fosta-dbclient/releases/latest";
/// The community Discord invite, the one link besides release pages that `open_release_page` opens.
pub const COMMUNITY_URL: &str = "https://discord.gg/pBFkZD5mhN";
const RELEASES_PAGE: &str = "https://github.com/caothanhphatit/fosta-dbclient/releases";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_INTERVAL_HOURS: i64 = 24;
/// The NSIS installer leaves its uninstaller next to the app; the portable exe has none.
const UNINSTALLER: &str = "uninstall.exe";
/// The error code of a failed check; the UI ignores it unless the user asked.
pub const CHECK_FAILED: &str = "UPDATE_CHECK_FAILED";

/// `updateCheck` in the config file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateCheckSettings {
    /// Check once a day at startup (on unless turned off).
    pub enabled: bool,
    /// When the last check succeeded (RFC 3339).
    pub last_checked: Option<String>,
    /// The newest release the last check found, so "Update X" stays visible until the next check.
    pub latest_version: Option<String>,
    /// The version that started last; a newer one shows "What's new" once.
    pub last_run_version: Option<String>,
}

impl Default for UpdateCheckSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            last_checked: None,
            latest_version: None,
            last_run_version: None,
        }
    }
}

/// What `check_update` returns.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub newer: bool,
    /// The release page (notes and downloads).
    pub url: String,
    pub published_at: Option<String>,
}

/// The fields of GitHub's release object that matter here.
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// `v1.2.3` or `1.2.3` as a version; anything else is `None`.
pub fn parse_version(tag: &str) -> Option<Version> {
    let tag = tag.trim();
    Version::parse(tag.strip_prefix(['v', 'V']).unwrap_or(tag)).ok()
}

/// True when `candidate` is a stable release newer than `current`; pre-releases and unparseable tags never are.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    match (parse_version(current), parse_version(candidate)) {
        (Some(current), Some(candidate)) => candidate.pre.is_empty() && candidate > current,
        _ => false,
    }
}

/// True for a page under this repository's releases, the only links the update item opens.
pub fn is_release_page(url: &str) -> bool {
    url.strip_prefix(RELEASES_PAGE).is_some_and(|rest| {
        rest.is_empty() || (rest.starts_with('/') && rest.chars().all(|c| c.is_ascii_alphanumeric() || "/-._".contains(c)))
    })
}

/// The release page of a version, for links built without asking GitHub.
fn tag_url(version: &str) -> String {
    format!("{RELEASES_PAGE}/tag/v{version}")
}

/// Reads GitHub's answer; `None` for a draft, a pre-release or a tag that is not a version.
pub fn release_info(current: &str, body: &str) -> AppResult<Option<UpdateInfo>> {
    let release: Release =
        serde_json::from_str(body).map_err(|error| AppError::coded(CHECK_FAILED, format!("Unexpected answer from GitHub: {error}")))?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(version) = parse_version(&release.tag_name).filter(|version| version.pre.is_empty()) else {
        return Ok(None);
    };
    let latest = version.to_string();
    // Only GitHub links are shown, whatever the answer says.
    let url = if is_release_page(&release.html_url) {
        release.html_url
    } else {
        tag_url(&latest)
    };
    Ok(Some(UpdateInfo {
        current: current.to_owned(),
        newer: is_newer(current, &latest),
        latest,
        url,
        published_at: release.published_at,
    }))
}

/// What the last check found, while the next one is not due yet.
pub fn remembered(current: &str, settings: &UpdateCheckSettings) -> Option<UpdateInfo> {
    let latest = settings.latest_version.as_deref()?;
    is_newer(current, latest).then(|| UpdateInfo {
        current: current.to_owned(),
        latest: latest.to_owned(),
        newer: true,
        url: tag_url(latest),
        published_at: None,
    })
}

/// True when the automatic check should run: never checked, a day has passed, or the stamp is unreadable or in the future.
pub fn check_due(last_checked: Option<&str>, now: DateTime<Utc>) -> bool {
    let Some(last) = last_checked.and_then(|text| DateTime::parse_from_rfc3339(text).ok()) else {
        return true;
    };
    let elapsed = now.signed_duration_since(last.with_timezone(&Utc));
    elapsed < chrono::TimeDelta::zero() || elapsed >= chrono::TimeDelta::hours(CHECK_INTERVAL_HOURS)
}

/// `"installer"` when the NSIS uninstaller sits in `dir` (the folder of the running exe), else `"portable"`.
pub fn install_kind_in(dir: Option<&Path>) -> &'static str {
    match dir {
        Some(dir) if dir.join(UNINSTALLER).is_file() => "installer",
        _ => "portable",
    }
}

/// Records `current` as the version that started last. Returns the version to announce ("What's new") when it is
/// newer than the previous start, and whether the settings changed.
pub fn record_run(settings: &mut UpdateCheckSettings, current: &str) -> (Option<String>, bool) {
    if settings.last_run_version.as_deref() == Some(current) {
        return (None, false);
    }
    let previous = settings.last_run_version.replace(current.to_owned());
    let announce = previous
        .is_some_and(|previous| is_newer(&previous, current))
        .then(|| current.to_owned());
    (announce, true)
}

/// The latest release as GitHub's JSON.
pub async fn fetch_latest_release() -> AppResult<String> {
    let failed = |error: reqwest::Error| AppError::coded(CHECK_FAILED, format!("Could not reach GitHub: {error}"));
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(format!("fosta-studio/{CURRENT_VERSION}"))
        .build()
        .map_err(failed)?;
    let response = client
        .get(LATEST_RELEASE_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::coded(CHECK_FAILED, format!("GitHub answered {status}")));
    }
    response.text().await.map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_semver() {
        assert!(is_newer("0.1.0", "v0.1.1"));
        assert!(is_newer("0.1.0", "0.2.0"));
        assert!(is_newer("0.9.0", "v0.10.0"), "numeric, not text order");
        assert!(is_newer("1.0.0-rc.1", "v1.0.0"));
        assert!(!is_newer("0.1.1", "v0.1.1"));
        assert!(!is_newer("0.2.0", "v0.1.9"));
    }

    #[test]
    fn prereleases_and_garbage_are_never_newer() {
        assert!(!is_newer("0.1.0", "v0.2.0-rc.1"));
        assert!(!is_newer("0.1.0", "latest"));
        assert!(!is_newer("0.1.0", "v1.2"));
        assert!(!is_newer("0.1.0", ""));
        assert!(!is_newer("garbage", "v9.9.9"));
        assert_eq!(parse_version(" V1.2.3 "), Some(Version::new(1, 2, 3)));
    }

    #[test]
    fn reads_the_release_and_skips_drafts_and_prereleases() {
        let body = r#"{ "tag_name": "v0.1.1", "html_url": "https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.1",
            "published_at": "2026-11-10T08:00:00Z", "draft": false, "prerelease": false, "assets": [] }"#;
        let info = release_info("0.1.0", body).unwrap().unwrap();
        assert_eq!(info.latest, "0.1.1");
        assert!(info.newer);
        assert_eq!(info.published_at.as_deref(), Some("2026-11-10T08:00:00Z"));
        assert!(!release_info("0.1.1", body).unwrap().unwrap().newer);

        assert!(release_info("0.1.0", r#"{ "tag_name": "v0.2.0", "draft": true }"#)
            .unwrap()
            .is_none());
        assert!(release_info("0.1.0", r#"{ "tag_name": "v0.2.0", "prerelease": true }"#)
            .unwrap()
            .is_none());
        assert!(release_info("0.1.0", r#"{ "tag_name": "v0.2.0-beta.1" }"#).unwrap().is_none());
        assert!(release_info("0.1.0", r#"{ "tag_name": "nightly" }"#).unwrap().is_none());
        let error = release_info("0.1.0", "<html>").unwrap_err();
        assert_eq!(error.code(), Some(CHECK_FAILED));
    }

    #[test]
    fn links_only_to_the_repository() {
        let info = release_info("0.1.0", r#"{ "tag_name": "v0.1.1", "html_url": "https://evil.example/x" }"#)
            .unwrap()
            .unwrap();
        assert_eq!(info.url, "https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.1");
        assert!(is_release_page("https://github.com/caothanhphatit/fosta-dbclient/releases/latest"));
        assert!(!is_release_page(
            "https://github.com/caothanhphatit/fosta-dbclient/releases.evil.example"
        ));
        assert!(!is_release_page(
            "https://github.com/caothanhphatit/fosta-dbclient/releases/x\" & calc"
        ));
    }

    #[test]
    fn the_automatic_check_runs_once_a_day() {
        let now = DateTime::parse_from_rfc3339("2026-11-10T12:00:00Z").unwrap().with_timezone(&Utc);
        assert!(check_due(None, now));
        assert!(check_due(Some("not a date"), now));
        assert!(!check_due(Some("2026-11-10T11:00:00Z"), now));
        assert!(!check_due(Some("2026-11-09T12:00:01Z"), now));
        assert!(check_due(Some("2026-11-09T12:00:00Z"), now));
        assert!(check_due(Some("2026-11-09T19:00:00+07:00"), now), "offsets count");
        assert!(
            check_due(Some("2026-11-11T12:00:00Z"), now),
            "a stamp in the future does not block checks"
        );
    }

    #[test]
    fn remembers_a_newer_release_between_checks() {
        let mut settings = UpdateCheckSettings::default();
        assert!(remembered("0.1.0", &settings).is_none());
        settings.latest_version = Some("0.1.1".into());
        let info = remembered("0.1.0", &settings).unwrap();
        assert_eq!(info.url, "https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.1");
        assert!(remembered("0.1.1", &settings).is_none(), "gone once installed");
    }

    #[test]
    fn install_kind_follows_the_uninstaller() {
        let dir = std::env::temp_dir().join(format!("fosta-install-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(install_kind_in(Some(&dir)), "portable");
        std::fs::write(dir.join(UNINSTALLER), b"").unwrap();
        assert_eq!(install_kind_in(Some(&dir)), "installer");
        assert_eq!(install_kind_in(None), "portable");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn whats_new_shows_once_after_an_upgrade() {
        let mut settings = UpdateCheckSettings::default();
        assert_eq!(record_run(&mut settings, "0.1.0"), (None, true), "first start: nothing to announce");
        assert_eq!(record_run(&mut settings, "0.1.0"), (None, false));
        assert_eq!(record_run(&mut settings, "0.1.1"), (Some("0.1.1".into()), true));
        assert_eq!(record_run(&mut settings, "0.1.1"), (None, false));
        assert_eq!(record_run(&mut settings, "0.1.0"), (None, true), "a downgrade is not announced");
    }

    #[test]
    fn settings_default_on_and_round_trip() {
        let config: crate::config::AppConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.update_check, UpdateCheckSettings::default());
        assert!(config.update_check.enabled);
        let partial: crate::config::AppConfig = serde_json::from_str(r#"{ "updateCheck": {} }"#).unwrap();
        assert!(partial.update_check.enabled);

        let config = crate::config::AppConfig {
            update_check: UpdateCheckSettings {
                enabled: false,
                last_checked: Some("2026-11-10T12:00:00+00:00".into()),
                latest_version: Some("0.1.1".into()),
                last_run_version: Some("0.1.0".into()),
            },
            ..Default::default()
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["updateCheck"]["enabled"], false);
        assert_eq!(json["updateCheck"]["lastChecked"], "2026-11-10T12:00:00+00:00");
        let back: crate::config::AppConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.update_check, config.update_check);
    }
}
