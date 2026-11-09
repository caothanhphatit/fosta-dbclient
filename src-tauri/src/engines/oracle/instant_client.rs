//! Oracle Instant Client provisioning: finds a client already installed on the machine, or downloads Oracle Instant Client
//! Basic Light for the current platform into the data directory, verifies it against a pinned SHA-256 and extracts it, then
//! initialises the `oracle` crate (ODPI-C) with that library directory.
//!
//! Resolution order (first hit wins):
//! 1. a directory the user configured explicitly;
//! 2. a system install: `PATH` (Windows), `LD_LIBRARY_PATH` (Linux), `DYLD_LIBRARY_PATH` (macOS), `ORACLE_HOME`;
//! 3. a client this module downloaded, in `<data dir>/fosta-studio-oracle/instantclient_<version>` on the desktop (next to
//!    `fosta-studio.json`) or `<FOSTA_STUDIO_DATA>/oracle` in the web edition (`/data/oracle` in the Docker image).
//!
//! ODPI-C loads the client once per process, the first time it is initialised; a failed load leaves it uninitialised, so a
//! download after a "client not found" error takes effect without restarting the app. [`initialize`] must run before the first
//! `oracle::Connector::connect`, or ODPI-C falls back to its default search and ignores the downloaded copy.
//!
//! License: Instant Client is distributed by Oracle under the "Oracle Technology Network Development and Distribution License
//! Terms for Instant Client" (https://www.oracle.com/downloads/licenses/instant-client-lic.html). Fosta Studio never bundles or
//! redistributes the binaries (none are in the repository); the app downloads them from Oracle's official server on the user's
//! request, onto the user's machine, under those terms. The UI must tell the user that the download comes from Oracle and is
//! covered by Oracle's license, and link to the terms above, before starting it.

use super::{missing_client_error, INSTANT_CLIENT_URL};
use crate::engines::common::EngineError;
use serde::Serialize;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// A downloadable Instant Client Basic Light package, pinned by version, size and SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// `<os>-<arch>` as in [`std::env::consts`], e.g. `windows-x86_64`.
    pub platform: &'static str,
    pub version: &'static str,
    pub url: &'static str,
    pub size: u64,
    /// Lowercase hex, exactly as Oracle publishes it on the platform's download page.
    pub sha256: &'static str,
}

impl Package {
    pub fn file_name(&self) -> &'static str {
        self.url.rsplit('/').next().unwrap_or(self.url)
    }

    /// The folder the package is installed to under the install root: `instantclient_23_26_4_0_0`.
    pub fn dir_name(&self) -> String {
        format!("instantclient_{}", self.version.replace('.', "_"))
    }
}

// Pinned packages. Checksums and sizes are copied from Oracle's download pages (retrieved 2026-11-08):
//   https://www.oracle.com/database/technologies/instant-client/winx64-64-downloads.html
//   https://www.oracle.com/database/technologies/instant-client/linux-x86-64-downloads.html
//   https://www.oracle.com/database/technologies/instant-client/linux-arm-aarch64-downloads.html
// To move to a newer release, update version, URL, size and SHA-256 together from the same page.
// macOS is not listed yet: Oracle ships it as a .dmg disk image (not a ZIP), which needs `hdiutil` to unpack; add it here with a
// DMG step in `extract_package` when macOS support lands.
// The Windows 23.26 client needs the Microsoft Visual C++ 2015-2022 Redistributable; Linux needs libaio (libaio1t64 on newer
// Debian/Ubuntu).
const PACKAGES: &[Package] = &[
    Package {
        platform: "windows-x86_64",
        version: "23.26.4.0.0",
        url: "https://download.oracle.com/otn_software/nt/instantclient/2326400/instantclient-basiclite-windows.x64-23.26.4.0.0.zip",
        size: 60_406_433,
        sha256: "7586c02a2926f619a0b16a9ae32e85bb3605798aa177155b065fffb48a1ee0b8",
    },
    Package {
        platform: "linux-x86_64",
        version: "23.26.4.0.0",
        url: "https://download.oracle.com/otn_software/linux/instantclient/2326400/instantclient-basiclite-linux.x64-23.26.4.0.0.zip",
        size: 76_612_271,
        sha256: "d049e593bba0041feb15c14ea894e2f1a437377462b99d9bdb6b7ab30b2dd660",
    },
    Package {
        platform: "linux-aarch64",
        version: "23.26.4.0.0",
        url: "https://download.oracle.com/otn_software/linux/instantclient/2326400/instantclient-basiclite-linux.arm64-23.26.4.0.0.zip",
        size: 68_005_804,
        sha256: "61d55b028e9ba5792fbf497e9bcb8f2f14116ed81fc7a8625afc3dd5d0f28c27",
    },
];

/// Written into an installed client folder after a verified extraction; holds the archive's SHA-256.
const MARKER_FILE: &str = ".fosta-instantclient";

/// The pinned package for `os` / `arch` (values of [`std::env::consts::OS`] / [`std::env::consts::ARCH`]).
pub fn package_for(os: &str, arch: &str) -> Option<&'static Package> {
    let platform = format!("{os}-{arch}");
    PACKAGES.iter().find(|p| p.platform == platform)
}

/// The pinned package for this machine.
pub fn current_package() -> Result<&'static Package, ClientError> {
    package_for(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        ClientError::new(
            ClientErrorKind::UnsupportedPlatform,
            format!(
                "Fosta Studio cannot download Oracle Instant Client for {}-{} yet. Install it from {INSTANT_CLIENT_URL} and add its \
                 folder to the library path.",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        )
    })
}

/// The client library ODPI-C loads.
fn client_lib_name() -> &'static str {
    if cfg!(windows) {
        "oci.dll"
    } else if cfg!(target_os = "macos") {
        "libclntsh.dylib"
    } else {
        "libclntsh.so"
    }
}

fn has_client_lib(dir: &Path) -> bool {
    dir.join(client_lib_name()).is_file()
}

// ---------------------------------------------------------------------------------------------------------------------------
// Errors and status

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientErrorKind {
    /// No client was found and none was loaded.
    NotInstalled,
    /// No pinned package exists for this OS / CPU.
    UnsupportedPlatform,
    /// The download failed (DNS, TLS, connection, HTTP status).
    Network,
    /// The downloaded archive does not match the pinned SHA-256 (or size).
    ChecksumMismatch,
    /// The disk ran out of space while downloading or extracting.
    DiskFull,
    /// The user cancelled the download.
    Cancelled,
    /// The archive is not a valid Instant Client ZIP.
    Archive,
    /// Any other file system error.
    Io,
    /// ODPI-C could not load the client library that was found.
    Load,
}

/// A structured provisioning error for the UI: `kind` picks the message and the retry button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientError {
    pub kind: ClientErrorKind,
    pub message: String,
}

impl ClientError {
    pub fn new(kind: ClientErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn io(context: &str, e: io::Error) -> Self {
        if is_disk_full(&e) {
            return Self::new(ClientErrorKind::DiskFull, format!("Not enough disk space to {context}: {e}"));
        }
        Self::new(ClientErrorKind::Io, format!("Cannot {context}: {e}"))
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

impl From<ClientError> for EngineError {
    fn from(e: ClientError) -> Self {
        match e.kind {
            ClientErrorKind::NotInstalled => missing_client_error(&e.message),
            ClientErrorKind::Cancelled => EngineError::cancelled(),
            _ => EngineError::connect(e.message),
        }
    }
}

fn is_disk_full(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::StorageFull || e.kind() == io::ErrorKind::QuotaExceeded {
        return true;
    }
    // ERROR_HANDLE_DISK_FULL (39), ERROR_DISK_FULL (112) on Windows; ENOSPC (28), EDQUOT (122 Linux, 69 macOS) elsewhere.
    match e.raw_os_error() {
        Some(code) if cfg!(windows) => code == 39 || code == 112,
        Some(code) if cfg!(target_os = "macos") => code == 28 || code == 69,
        Some(code) => code == 28 || code == 122,
        None => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientSource {
    /// Installed on the machine (PATH, ORACLE_HOME, ...) or a folder the user configured.
    System,
    /// Downloaded by Fosta Studio into the data directory.
    Downloaded,
    None,
}

/// What [`status`] reports to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub available: bool,
    pub source: ClientSource,
    /// `23.26.4.0.0` once loaded; before that the version in the folder name when there is one (`23.26`).
    pub version: Option<String>,
    /// The folder holding the client library.
    pub path: Option<PathBuf>,
    /// Whether ODPI-C has loaded a client in this process.
    pub loaded: bool,
    /// The package [`download`] would fetch on this platform, for the "Download Oracle client" prompt.
    pub download: Option<DownloadOffer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadOffer {
    pub version: String,
    pub size: u64,
    pub url: String,
    pub license_url: String,
}

pub const LICENSE_URL: &str = "https://www.oracle.com/downloads/licenses/instant-client-lic.html";

/// A client folder found on the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub source: ClientSource,
    pub dir: PathBuf,
}

/// The environment a resolution looks at (separate from the process environment so tests can supply their own).
#[derive(Debug, Clone, Default)]
pub struct SearchEnv {
    /// `PATH` on Windows, `LD_LIBRARY_PATH` on Linux, `DYLD_LIBRARY_PATH` on macOS.
    pub lib_path: Option<OsString>,
    pub oracle_home: Option<OsString>,
}

impl SearchEnv {
    pub fn from_process() -> Self {
        let var = if cfg!(windows) {
            "PATH"
        } else if cfg!(target_os = "macos") {
            "DYLD_LIBRARY_PATH"
        } else {
            "LD_LIBRARY_PATH"
        };
        Self {
            lib_path: std::env::var_os(var).filter(|v| !v.is_empty()),
            oracle_home: std::env::var_os("ORACLE_HOME").filter(|v| !v.is_empty()),
        }
    }
}

/// Where downloaded clients live for a data directory: `<data>/oracle` in the web edition (`FOSTA_STUDIO_DATA` set), otherwise
/// `<data>/fosta-studio-oracle` next to the desktop config.
pub fn install_root_for(data_dir: &Path, web: bool) -> PathBuf {
    data_dir.join(if web { "oracle" } else { "fosta-studio-oracle" })
}

/// The install root for this process, from [`crate::config::data_dir`].
pub fn default_install_root() -> Result<PathBuf, ClientError> {
    let web = std::env::var_os(crate::config::DATA_DIR_ENV).is_some_and(|v| !v.is_empty());
    let data = crate::config::data_dir().map_err(|e| ClientError::new(ClientErrorKind::Io, e.to_string()))?;
    Ok(install_root_for(&data, web))
}

/// Finds a client: the configured folder, then a system install, then a downloaded one.
pub fn resolve(configured: Option<&Path>, env: &SearchEnv, install_root: &Path) -> Option<Resolved> {
    if let Some(dir) = configured.filter(|d| has_client_lib(d)) {
        return Some(Resolved {
            source: ClientSource::System,
            dir: dir.to_owned(),
        });
    }
    if let Some(dir) = find_system(env) {
        return Some(Resolved {
            source: ClientSource::System,
            dir,
        });
    }
    find_downloaded(install_root).map(|dir| Resolved {
        source: ClientSource::Downloaded,
        dir,
    })
}

fn find_system(env: &SearchEnv) -> Option<PathBuf> {
    if let Some(path) = &env.lib_path {
        if let Some(dir) = std::env::split_paths(path).find(|d| !d.as_os_str().is_empty() && has_client_lib(d)) {
            return Some(dir);
        }
    }
    let home = PathBuf::from(env.oracle_home.as_ref()?);
    let sub = if cfg!(windows) { "bin" } else { "lib" };
    [home.join(sub), home].into_iter().find(|d| has_client_lib(d))
}

/// The newest verified client under `install_root` (a folder `instantclient_*` with the marker file and the client library).
pub fn find_downloaded(install_root: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(install_root).ok()?;
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("instantclient_"))
                && p.join(MARKER_FILE).is_file()
                && has_client_lib(p)
        })
        .max_by_key(|p| version_from_dir(p).map(|v| version_key(&v)).unwrap_or_default())
}

/// `instantclient_23_26` -> `23.26`, `instantclient_23_26_4_0_0` -> `23.26.4.0.0`.
pub fn version_from_dir(dir: &Path) -> Option<String> {
    let name = dir.file_name()?.to_str()?;
    let rest = name.strip_prefix("instantclient_")?;
    let parts: Vec<&str> = rest.split('_').collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some(parts.join("."))
}

fn version_key(v: &str) -> Vec<u32> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

// ---------------------------------------------------------------------------------------------------------------------------
// Status and initialisation

/// What this module loaded into ODPI-C, if anything.
static LOADED: Mutex<Option<Resolved>> = Mutex::new(None);

fn loaded() -> Option<Resolved> {
    LOADED.lock().ok().and_then(|l| l.clone())
}

fn client_version() -> Option<String> {
    oracle::Version::client().ok().map(|v| v.to_string())
}

/// The client state for the UI, with the process environment and the default install root.
pub fn status(configured: Option<&Path>) -> ClientStatus {
    let root = default_install_root().ok();
    status_at(configured, &SearchEnv::from_process(), root.as_deref())
}

pub fn status_at(configured: Option<&Path>, env: &SearchEnv, install_root: Option<&Path>) -> ClientStatus {
    let download = current_package().ok().map(|p| DownloadOffer {
        version: p.version.into(),
        size: p.size,
        url: p.url.into(),
        license_url: LICENSE_URL.into(),
    });
    let is_loaded = oracle::InitParams::is_initialized();
    if is_loaded {
        let found = loaded();
        return ClientStatus {
            available: true,
            source: found.as_ref().map(|r| r.source).unwrap_or(ClientSource::System),
            version: client_version(),
            path: found.map(|r| r.dir),
            loaded: true,
            download,
        };
    }
    let found = resolve(configured, env, install_root.unwrap_or(Path::new("")));
    ClientStatus {
        available: found.is_some(),
        source: found.as_ref().map(|r| r.source).unwrap_or(ClientSource::None),
        version: found.as_ref().and_then(|r| version_from_dir(&r.dir)),
        path: found.map(|r| r.dir),
        loaded: false,
        download,
    }
}

/// Loads the client into ODPI-C (once per process) from the folder [`resolve`] finds; without one, tries ODPI-C's own search
/// (the system loader path). Call it before the first Oracle connection. Cheap once a client is loaded.
pub fn initialize(configured: Option<&Path>) -> Result<ClientStatus, ClientError> {
    let root = default_install_root().ok();
    initialize_at(configured, &SearchEnv::from_process(), root.as_deref())
}

pub fn initialize_at(configured: Option<&Path>, env: &SearchEnv, install_root: Option<&Path>) -> Result<ClientStatus, ClientError> {
    // One initialisation at a time, so LOADED always describes what ODPI-C loaded.
    static INIT: Mutex<()> = Mutex::new(());
    let _guard = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if !oracle::InitParams::is_initialized() {
        let found = resolve(configured, env, install_root.unwrap_or(Path::new("")));
        let mut params = oracle::InitParams::new();
        if let Some(r) = &found {
            #[cfg(target_os = "linux")]
            preload_dependencies(&r.dir);
            params
                .oracle_client_lib_dir(&r.dir)
                .map_err(|e| ClientError::new(ClientErrorKind::Load, e.to_string()))?;
        }
        params.load_error_url(INSTANT_CLIENT_URL).ok();
        match params.init() {
            Ok(_) => {
                if let Ok(mut l) = LOADED.lock() {
                    *l = found;
                }
            }
            Err(e) => {
                let text = e.to_string();
                return Err(match &found {
                    Some(r) => ClientError::new(
                        ClientErrorKind::Load,
                        format!("Oracle Instant Client in {} could not be loaded: {text}", r.dir.display()),
                    ),
                    None => ClientError::new(ClientErrorKind::NotInstalled, text.lines().next().unwrap_or(&text).to_owned()),
                });
            }
        }
    }
    Ok(status_at(configured, env, install_root))
}

/// On Linux the loader does not look next to `libclntsh.so` for the libraries it needs (`libclntshcore`, `libnnz`):
/// Instant Client has no RPATH, so loading it from a folder that is not on `LD_LIBRARY_PATH` fails with
/// "libnnz.so: cannot open shared object file". Loading those libraries first, by full path and with global symbols,
/// lets the loader reuse them by their SONAME when ODPI-C loads `libclntsh.so`. Failures are left to that load to report.
#[cfg(target_os = "linux")]
fn preload_dependencies(dir: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<OsString> = entries.filter_map(|entry| Some(entry.ok()?.file_name())).collect();
    names.sort();
    for prefix in ["libclntshcore.so", "libnnz"] {
        for name in names.iter().filter(|name| {
            let name = name.to_string_lossy();
            name.starts_with(prefix) && name.contains(".so")
        }) {
            let Ok(path) = std::ffi::CString::new(dir.join(name).as_os_str().as_bytes()) else {
                continue;
            };
            // SAFETY: `path` is a valid NUL-terminated string; the handle is kept for the life of the process (the
            // client stays loaded once ODPI-C has it), so it is never closed.
            unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------
// Download

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Downloading,
    Verifying,
    Extracting,
    Done,
}

/// A progress event for the UI. `downloaded` / `total` are bytes of the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub phase: Phase,
    pub downloaded: u64,
    pub total: u64,
}

/// Downloads, verifies and installs the pinned package for this platform into the default install root. Returns the new status
/// (not yet loaded: call [`initialize`] next). A cancelled or interrupted download keeps its `.part` file and resumes from it
/// on the next call. Progress is reported at most every 256 KiB.
pub async fn download(progress: &mut (dyn FnMut(Progress) + Send), cancel: &AtomicBool) -> Result<ClientStatus, ClientError> {
    let package = current_package()?;
    let root = default_install_root()?;
    install(package, &root, progress, cancel).await?;
    Ok(status_at(None, &SearchEnv::from_process(), Some(&root)))
}

/// Downloads and installs `package` under `install_root`; returns the installed folder.
pub async fn install(
    package: &Package,
    install_root: &Path,
    progress: &mut (dyn FnMut(Progress) + Send),
    cancel: &AtomicBool,
) -> Result<PathBuf, ClientError> {
    // One download at a time per process: two would write the same .part file.
    static DOWNLOAD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = DOWNLOAD.lock().await;

    let target = install_root.join(package.dir_name());
    if target.join(MARKER_FILE).is_file() && has_client_lib(&target) {
        progress(Progress {
            phase: Phase::Done,
            downloaded: package.size,
            total: package.size,
        });
        return Ok(target);
    }
    fs::create_dir_all(install_root).map_err(|e| ClientError::io("create the Oracle client folder", e))?;
    let part = install_root.join(format!("{}.part", package.file_name()));
    fetch(package, &part, progress, cancel).await?;

    progress(Progress {
        phase: Phase::Verifying,
        downloaded: package.size,
        total: package.size,
    });
    let (part2, sha) = (part.clone(), package.sha256);
    let actual = tokio::task::spawn_blocking(move || sha256_file(&part2))
        .await
        .map_err(|e| ClientError::new(ClientErrorKind::Io, format!("The worker thread failed: {e}")))?
        .map_err(|e| ClientError::io("read the downloaded archive", e))?;
    if !actual.eq_ignore_ascii_case(sha) {
        let _ = fs::remove_file(&part);
        return Err(ClientError::new(
            ClientErrorKind::ChecksumMismatch,
            format!(
                "The downloaded Oracle Instant Client does not match Oracle's published checksum (expected SHA-256 {sha}, got \
                 {actual}). The file was deleted; try again."
            ),
        ));
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(cancelled());
    }

    progress(Progress {
        phase: Phase::Extracting,
        downloaded: package.size,
        total: package.size,
    });
    let (part2, target2, sha2) = (part.clone(), target.clone(), actual);
    tokio::task::spawn_blocking(move || extract_package(&part2, &target2, &sha2))
        .await
        .map_err(|e| ClientError::new(ClientErrorKind::Io, format!("The worker thread failed: {e}")))??;
    let _ = fs::remove_file(&part);
    progress(Progress {
        phase: Phase::Done,
        downloaded: package.size,
        total: package.size,
    });
    Ok(target)
}

fn cancelled() -> ClientError {
    ClientError::new(ClientErrorKind::Cancelled, "The Oracle Instant Client download was cancelled.")
}

fn network(e: impl fmt::Display) -> ClientError {
    ClientError::new(
        ClientErrorKind::Network,
        format!("Could not download Oracle Instant Client from download.oracle.com: {e}"),
    )
}

/// Downloads `package` into `part`, resuming from the bytes already there (HTTP Range). Leaves the complete file in `part`.
async fn fetch(
    package: &Package,
    part: &Path,
    progress: &mut (dyn FnMut(Progress) + Send),
    cancel: &AtomicBool,
) -> Result<(), ClientError> {
    let mut have = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if have > package.size {
        fs::remove_file(part).map_err(|e| ClientError::io("remove a stale partial download", e))?;
        have = 0;
    }
    if have == package.size {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("fosta-studio/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(network)?;
    let mut request = client.get(package.url);
    if have > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let mut response = request.send().await.map_err(network)?;
    let status = response.status();
    let mut file = if have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
        fs::OpenOptions::new()
            .append(true)
            .open(part)
            .map_err(|e| ClientError::io("open the partial download", e))?
    } else if status.is_success() {
        // The server ignored the range (or there was nothing to resume): start over.
        have = 0;
        File::create(part).map_err(|e| ClientError::io("create the download file", e))?
    } else if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        let _ = fs::remove_file(part);
        return Err(network("the server rejected the resume request; try again"));
    } else {
        return Err(network(format!("HTTP {status}")));
    };
    let mut reported = have;
    progress(Progress {
        phase: Phase::Downloading,
        downloaded: have,
        total: package.size,
    });
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = file.flush();
            return Err(cancelled());
        }
        let Some(chunk) = response.chunk().await.map_err(network)? else {
            break;
        };
        have += chunk.len() as u64;
        if have > package.size {
            drop(file);
            let _ = fs::remove_file(part);
            return Err(ClientError::new(
                ClientErrorKind::ChecksumMismatch,
                format!("The download is larger than the expected {} bytes; it was deleted.", package.size),
            ));
        }
        file.write_all(&chunk).map_err(|e| ClientError::io("save the download", e))?;
        if have - reported >= 256 * 1024 || have == package.size {
            reported = have;
            progress(Progress {
                phase: Phase::Downloading,
                downloaded: have,
                total: package.size,
            });
        }
    }
    file.sync_all().map_err(|e| ClientError::io("save the download", e))?;
    if have < package.size {
        return Err(network(format!(
            "the connection closed after {have} of {} bytes; try again to resume",
            package.size
        )));
    }
    Ok(())
}

/// The SHA-256 of a file, lowercase hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// Extracts the client folder of a verified archive (the folder holding the client library; `META-INF` and other folders are
/// skipped) into `target`, then writes the marker file through a temporary file and a rename. The marker is the commit point:
/// [`find_downloaded`] and [`install`] ignore a folder without it, and a re-run wipes the folder first, so a half-extracted
/// client is never used. (Extracting into a staging folder and renaming it into place does not work reliably on Windows: a
/// folder of freshly written DLLs often cannot be renamed while antivirus scanners hold them open.)
pub fn extract_package(archive: &Path, target: &Path, sha256: &str) -> Result<(), ClientError> {
    let bad = |m: String| ClientError::new(ClientErrorKind::Archive, m);
    let zip_err = |e: zip::result::ZipError| match e {
        zip::result::ZipError::Io(io) => ClientError::io("extract Oracle Instant Client", io),
        other => ClientError::new(ClientErrorKind::Archive, format!("Cannot extract Oracle Instant Client: {other}")),
    };
    let extract_err = |e: io::Error| ClientError::io("extract Oracle Instant Client", e);
    let file = File::open(archive).map_err(|e| ClientError::io("open the downloaded archive", e))?;
    let mut zip =
        zip::ZipArchive::new(io::BufReader::new(file)).map_err(|e| bad(format!("The downloaded archive is not a valid ZIP: {e}")))?;
    // The folder of the client library, e.g. `instantclient_23_26`; the shallowest one if there are several.
    let mut prefix: Option<PathBuf> = None;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(zip_err)?;
        let Some(name) = entry.enclosed_name() else { continue };
        if !entry.is_dir() && name.file_name().is_some_and(|n| n == client_lib_name()) {
            let parent = name.parent().unwrap_or(Path::new("")).to_owned();
            if prefix.as_ref().is_none_or(|p| parent.components().count() < p.components().count()) {
                prefix = Some(parent);
            }
        }
    }
    let prefix = prefix.ok_or_else(|| {
        bad(format!(
            "The archive does not contain {}; it is not an Instant Client package.",
            client_lib_name()
        ))
    })?;
    if target.exists() {
        fs::remove_dir_all(target).map_err(|e| ClientError::io("replace the previous Oracle client folder", e))?;
    }
    fs::create_dir_all(target).map_err(|e| ClientError::io("create the Oracle client folder", e))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(zip_err)?;
        // `enclosed_name` is None for names that would leave the folder (`..`, absolute paths): those are skipped.
        let Some(name) = entry.enclosed_name() else { continue };
        let Ok(relative) = name.strip_prefix(&prefix) else { continue };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let out = target.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(extract_err)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(extract_err)?;
        }
        #[cfg(unix)]
        if entry.is_symlink() {
            // The Linux packages link libclntsh.so to libclntsh.so.<version>; only links inside the folder are kept.
            let mut link = String::new();
            entry.read_to_string(&mut link).map_err(extract_err)?;
            let link = Path::new(link.trim());
            if link.is_absolute() || link.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
                continue;
            }
            std::os::unix::fs::symlink(link, &out).map_err(extract_err)?;
            continue;
        }
        let mut f = File::create(&out).map_err(extract_err)?;
        io::copy(&mut entry, &mut f).map_err(extract_err)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o755));
        }
    }
    if !has_client_lib(target) {
        return Err(bad(format!("{} is missing after extraction.", client_lib_name())));
    }
    let tmp = target.join(format!("{MARKER_FILE}.tmp"));
    fs::write(&tmp, format!("{sha256}\n")).map_err(|e| ClientError::io("write the install marker", e))?;
    fs::rename(&tmp, target.join(MARKER_FILE)).map_err(|e| ClientError::io("write the install marker", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A unique temporary folder, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("fosta-ic-{tag}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fake_client(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(client_lib_name()), b"fake").unwrap();
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    fn sha_hex(data: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, data)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn pinned_packages_are_well_formed() {
        for p in PACKAGES {
            assert!(p.url.starts_with("https://download.oracle.com/otn_software/"), "{}", p.url);
            assert!(p.file_name().starts_with("instantclient-basiclite-") && p.file_name().ends_with(".zip"));
            assert!(p.file_name().contains(p.version), "{}", p.url);
            assert_eq!(p.sha256.len(), 64);
            assert!(p.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
            assert!(p.size > 10_000_000);
        }
        let win = package_for("windows", "x86_64").unwrap();
        assert_eq!(win.dir_name(), "instantclient_23_26_4_0_0");
        assert!(package_for("macos", "aarch64").is_none());
        assert!(package_for("windows", "x86").is_none());
    }

    #[test]
    fn sha256_of_a_file() {
        let tmp = TempDir::new("sha");
        let f = tmp.path().join("abc");
        fs::write(&f, b"abc").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::write(&f, b"").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn version_from_folder_names() {
        assert_eq!(version_from_dir(Path::new("/x/instantclient_23_26")).as_deref(), Some("23.26"));
        assert_eq!(
            version_from_dir(Path::new("instantclient_23_26_4_0_0")).as_deref(),
            Some("23.26.4.0.0")
        );
        assert_eq!(version_from_dir(Path::new("instantclient_x")), None);
        assert_eq!(version_from_dir(Path::new("instantclient_")), None);
        assert_eq!(version_from_dir(Path::new("oracle")), None);
    }

    #[test]
    fn install_roots() {
        let data = Path::new("data");
        assert_eq!(install_root_for(data, false), data.join("fosta-studio-oracle"));
        assert_eq!(install_root_for(data, true), data.join("oracle"));
    }

    #[test]
    fn resolution_order() {
        let tmp = TempDir::new("resolve");
        let configured = tmp.path().join("configured");
        let on_path = tmp.path().join("onpath");
        let home = tmp.path().join("home");
        let root = tmp.path().join("root");
        let empty = tmp.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        fake_client(&configured);
        fake_client(&on_path);
        fake_client(&home.join(if cfg!(windows) { "bin" } else { "lib" }));
        let downloaded = root.join("instantclient_23_26_4_0_0");
        fake_client(&downloaded);
        fs::write(downloaded.join(MARKER_FILE), "x").unwrap();

        let path_var = std::env::join_paths([empty.clone(), on_path.clone()]).unwrap();
        let env = SearchEnv {
            lib_path: Some(path_var),
            oracle_home: Some(home.clone().into()),
        };
        let r = resolve(Some(&configured), &env, &root).unwrap();
        assert_eq!((r.source, r.dir), (ClientSource::System, configured.clone()));
        // A configured folder without the library is skipped.
        let r = resolve(Some(&empty), &env, &root).unwrap();
        assert_eq!((r.source, r.dir), (ClientSource::System, on_path.clone()));
        let env_home = SearchEnv {
            lib_path: Some(empty.clone().into()),
            oracle_home: Some(home.clone().into()),
        };
        let r = resolve(None, &env_home, &root).unwrap();
        assert_eq!(r.dir, home.join(if cfg!(windows) { "bin" } else { "lib" }));
        let r = resolve(None, &SearchEnv::default(), &root).unwrap();
        assert_eq!((r.source, r.dir), (ClientSource::Downloaded, downloaded.clone()));
        assert_eq!(resolve(None, &SearchEnv::default(), &empty), None);
    }

    #[test]
    fn downloaded_needs_marker_and_prefers_newest() {
        let tmp = TempDir::new("newest");
        let root = tmp.path();
        let old = root.join("instantclient_21_23_0_0_0");
        let new = root.join("instantclient_23_26_4_0_0");
        let unverified = root.join("instantclient_99_1");
        for d in [&old, &new, &unverified] {
            fake_client(d);
        }
        fs::write(old.join(MARKER_FILE), "x").unwrap();
        assert_eq!(find_downloaded(root), Some(old.clone()));
        fs::write(new.join(MARKER_FILE), "x").unwrap();
        assert_eq!(find_downloaded(root), Some(new));
    }

    #[test]
    fn status_without_a_client() {
        if oracle::InitParams::is_initialized() {
            return; // another test in this process loaded a client
        }
        let tmp = TempDir::new("status");
        let s = status_at(None, &SearchEnv::default(), Some(tmp.path()));
        assert!(!s.available);
        assert_eq!(s.source, ClientSource::None);
        assert_eq!(s.path, None);
        assert!(!s.loaded);
        assert_eq!(s.download.is_some(), current_package().is_ok());
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["source"], "none");
    }

    #[test]
    fn extraction_finds_the_nested_client_folder() {
        let tmp = TempDir::new("extract");
        let lib = client_lib_name();
        let archive = tmp.path().join("ic.zip");
        let inner = format!("instantclient_23_26/{lib}");
        let other = "instantclient_23_26/oraociei.dll".to_string();
        let bytes = zip_bytes(&[(&inner, b"lib"), (&other, b"data"), ("META-INF/readme.txt", b"hi")]);
        fs::write(&archive, &bytes).unwrap();
        let root = tmp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("instantclient_23_26_4_0_0");
        // A leftover from an earlier failed install is replaced.
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("stale"), b"x").unwrap();
        assert_eq!(find_downloaded(&root), None);
        extract_package(&archive, &target, &sha_hex(&bytes)).unwrap();
        assert_eq!(fs::read(target.join(lib)).unwrap(), b"lib");
        assert_eq!(fs::read(target.join("oraociei.dll")).unwrap(), b"data");
        assert!(!target.join("stale").exists());
        assert_eq!(fs::read_to_string(target.join(MARKER_FILE)).unwrap().trim(), sha_hex(&bytes));
        // Only the client folder's contents are installed (no META-INF, no wrapper folder, no temporary marker).
        let mut names: Vec<_> = fs::read_dir(&target).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(
            names,
            vec![OsString::from(MARKER_FILE), OsString::from(lib), OsString::from("oraociei.dll")]
        );
        assert_eq!(find_downloaded(&root), Some(target));
    }

    #[test]
    fn extraction_rejects_archives_without_the_library_and_bad_zips() {
        let tmp = TempDir::new("badzip");
        let root = tmp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("instantclient_1_0");
        let archive = tmp.path().join("nolib.zip");
        fs::write(&archive, zip_bytes(&[("x/readme.txt", b"hi")])).unwrap();
        let e = extract_package(&archive, &target, "00").unwrap_err();
        assert_eq!(e.kind, ClientErrorKind::Archive);
        assert!(!target.exists());
        fs::write(&archive, b"not a zip").unwrap();
        assert_eq!(extract_package(&archive, &target, "00").unwrap_err().kind, ClientErrorKind::Archive);
        // Entries that climb out of the folder are skipped.
        let lib = client_lib_name();
        let evil = zip_bytes(&[("../../escaped.txt", b"x"), (lib, b"lib")]);
        fs::write(&archive, evil).unwrap();
        extract_package(&archive, &target, "00").unwrap();
        assert!(!tmp.path().join("escaped.txt").exists());
        assert!(!root.join("escaped.txt").exists());
        assert!(target.join(lib).is_file());
    }

    #[test]
    fn disk_full_errors_are_recognised() {
        let e = ClientError::io("save", io::Error::from(io::ErrorKind::StorageFull));
        assert_eq!(e.kind, ClientErrorKind::DiskFull);
        let raw = if cfg!(windows) { 112 } else { 28 };
        assert_eq!(
            ClientError::io("save", io::Error::from_raw_os_error(raw)).kind,
            ClientErrorKind::DiskFull
        );
        assert_eq!(
            ClientError::io("save", io::Error::from(io::ErrorKind::PermissionDenied)).kind,
            ClientErrorKind::Io
        );
        let engine: EngineError = ClientError::new(ClientErrorKind::NotInstalled, "DPI-1047").into();
        assert!(engine.message.contains(INSTANT_CLIENT_URL));
        let json = serde_json::to_value(ClientError::new(ClientErrorKind::ChecksumMismatch, "m")).unwrap();
        assert_eq!(json["kind"], "checksumMismatch");
    }

    /// A one-file HTTP server with Range support; records the Range header of each request.
    async fn serve(body: Vec<u8>) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/instantclient-basiclite-test-1.0.zip", listener.local_addr().unwrap());
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let seen = ranges.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let text = String::from_utf8_lossy(&req).to_string();
                let range = text.lines().find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .map(|r| r.trim_end_matches('-').to_owned())
                });
                seen.lock().unwrap().push(range.clone());
                let start: usize = range.and_then(|r| r.parse().ok()).unwrap_or(0);
                let head = if start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len() - start,
                        body.len() - 1,
                        body.len()
                    )
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                };
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body[start..]).await;
                let _ = sock.shutdown().await;
            }
        });
        (url, ranges)
    }

    fn test_package(url: String, bytes: &[u8], sha: String) -> Package {
        Package {
            platform: "test",
            version: "1.0",
            url: Box::leak(url.into_boxed_str()),
            size: bytes.len() as u64,
            sha256: Box::leak(sha.into_boxed_str()),
        }
    }

    #[tokio::test]
    async fn download_resumes_verifies_and_installs() {
        let lib = client_lib_name();
        let payload: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
        let bytes = zip_bytes(&[(&format!("instantclient_1_0/{lib}"), &payload)]);
        let (url, ranges) = serve(bytes.clone()).await;
        let package = test_package(url, &bytes, sha_hex(&bytes));
        let tmp = TempDir::new("download");
        let root = tmp.path().join("oracle");
        fs::create_dir_all(&root).unwrap();
        // An earlier attempt stopped half way.
        let half = bytes.len() / 2;
        fs::write(root.join(format!("{}.part", package.file_name())), &bytes[..half]).unwrap();
        let mut events = Vec::new();
        let cancel = AtomicBool::new(false);
        let dir = install(&package, &root, &mut |p| events.push(p), &cancel).await.unwrap();
        assert_eq!(dir, root.join("instantclient_1_0"));
        assert_eq!(fs::read(dir.join(lib)).unwrap(), payload);
        assert_eq!(ranges.lock().unwrap().clone(), vec![Some(half.to_string())]);
        assert_eq!(
            events.first().map(|p| (p.phase, p.downloaded)),
            Some((Phase::Downloading, half as u64))
        );
        let phases: Vec<Phase> = events.iter().map(|p| p.phase).collect();
        assert!(phases.contains(&Phase::Verifying) && phases.contains(&Phase::Extracting));
        assert_eq!(events.last().unwrap().phase, Phase::Done);
        assert!(events.windows(2).all(|w| w[0].downloaded <= w[1].downloaded));
        assert!(!root.join(format!("{}.part", package.file_name())).exists());
        // Installed already: no new request.
        install(&package, &root, &mut |_| {}, &cancel).await.unwrap();
        assert_eq!(ranges.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn download_checksum_mismatch_and_cancel() {
        let lib = client_lib_name();
        let bytes = zip_bytes(&[(&format!("instantclient_1_0/{lib}"), b"lib")]);
        let (url, _) = serve(bytes.clone()).await;
        let tmp = TempDir::new("mismatch");
        let root = tmp.path().join("oracle");
        let bad = test_package(url.clone(), &bytes, "0".repeat(64));
        let e = install(&bad, &root, &mut |_| {}, &AtomicBool::new(false)).await.unwrap_err();
        assert_eq!(e.kind, ClientErrorKind::ChecksumMismatch);
        assert!(!root.join(format!("{}.part", bad.file_name())).exists());
        assert!(!root.join("instantclient_1_0").exists());

        let good = test_package(url, &bytes, sha_hex(&bytes));
        let e = install(&good, &root, &mut |_| {}, &AtomicBool::new(true)).await.unwrap_err();
        assert_eq!(e.kind, ClientErrorKind::Cancelled);
        assert!(!root.join("instantclient_1_0").exists());
    }

    #[tokio::test]
    async fn download_network_error() {
        // Nothing listens on a port that was just released.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let tmp = TempDir::new("network");
        let package = test_package(format!("http://127.0.0.1:{port}/x.zip"), &[0; 10], "0".repeat(64));
        let e = install(&package, tmp.path(), &mut |_| {}, &AtomicBool::new(false))
            .await
            .unwrap_err();
        assert_eq!(e.kind, ClientErrorKind::Network);
    }

    /// Downloads the real package for this platform into the default install root (target/<profile>/deps/fosta-studio-oracle
    /// for the test binary), so the Oracle live tests find it. Run with
    /// `cargo test live_oracle_instant_client_download -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_oracle_instant_client_download() {
        let package = current_package().unwrap();
        let root = default_install_root().unwrap();
        let mut last = 0;
        let dir = install(
            package,
            &root,
            &mut |p| {
                if p.phase != Phase::Downloading || p.downloaded - last >= 10 << 20 {
                    last = p.downloaded;
                    eprintln!("{:?} {}/{}", p.phase, p.downloaded, p.total);
                }
            },
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert!(has_client_lib(&dir));
        let s = initialize_at(None, &SearchEnv::default(), Some(&root)).unwrap();
        assert!(s.available && s.loaded);
        assert_eq!(s.source, ClientSource::Downloaded);
        assert_eq!(s.version.as_deref(), Some(package.version));
        eprintln!("{s:?}");
    }
}
