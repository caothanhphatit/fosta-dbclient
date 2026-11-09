//! Exports land in the user's Downloads folder, written from Rust so they do
//! not depend on the webview's download handling.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use crate::{
    error::{AppError, AppResult},
    lock::lock,
};

/// Files this run of the app wrote with `save_download`; "Show in folder" only opens these, so the
/// webview cannot point Explorer at an arbitrary path.
static WRITTEN: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

/// Writes `bytes` as `name` in Downloads (adding " (2)", " (3)", … when taken) and returns the path.
pub fn save_download(name: &str, bytes: &[u8]) -> AppResult<String> {
    let directory = downloads_dir()?;
    fs::create_dir_all(&directory)?;
    let path = unique_path(&directory, &sanitize(name));
    fs::write(&path, bytes)?;
    remember_written(&path);
    Ok(path.to_string_lossy().into_owned())
}

/// Creates `name` in Downloads (adding " (2)", " (3)", … when taken) for an export written in chunks.
pub fn create_download(name: &str) -> AppResult<(fs::File, PathBuf)> {
    let directory = downloads_dir()?;
    fs::create_dir_all(&directory)?;
    let path = unique_path(&directory, &sanitize(name));
    let file = fs::File::create(&path)?;
    remember_written(&path);
    Ok((file, path))
}

fn remember_written(path: &Path) {
    lock(&WRITTEN).insert(path.to_path_buf());
}

/// Whether `path` is a file this run exported.
fn was_written(path: &Path) -> bool {
    lock(&WRITTEN).contains(path)
}

/// Opens Explorer with the file selected. Only for files the app exported in this run.
pub fn reveal(path: &str) -> AppResult<()> {
    let target = Path::new(path);
    if !was_written(target) {
        return Err(AppError::new(
            "Only files exported by Fosta Studio in this session can be shown in their folder.",
        ));
    }
    if !target.exists() {
        return Err(AppError::new(format!("{path} no longer exists")));
    }
    std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn()?;
    Ok(())
}

fn downloads_dir() -> AppResult<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| AppError::new("Cannot find your home folder"))?;
    Ok(PathBuf::from(home).join("Downloads"))
}

fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if matches!(character, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || character.is_control() {
                '_'
            } else {
                character
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        String::from("export")
    } else {
        trimmed.to_owned()
    }
}

fn unique_path(directory: &Path, name: &str) -> PathBuf {
    let candidate = directory.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name.to_owned(), String::new()),
    };
    (2..)
        .map(|index| directory.join(format!("{stem} ({index}){extension}")))
        .find(|path| !path.exists())
        .expect("an unused file name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exported_files_can_be_revealed() {
        let error = reveal(r"C:\Windows\System32\cmd.exe").expect_err("not exported by the app");
        assert!(error.to_string().starts_with("Only files exported"), "{error}");
        let exported = std::env::temp_dir().join("fosta-reveal-test-missing.csv");
        remember_written(&exported);
        assert!(was_written(&exported));
        // Remembered but deleted since: reported as missing, Explorer is not started.
        let missing = reveal(&exported.to_string_lossy()).expect_err("missing");
        assert!(missing.to_string().ends_with("no longer exists"), "{missing}");
    }

    /// Writes into the real Downloads folder (twice, to check the " (2)" suffix), then cleans up.
    #[test]
    #[ignore = "writes into the real Downloads folder"]
    fn live_save_download() {
        let first = save_download(
            "db-studio-selftest.csv",
            b"a,b
1,2
",
        )
        .expect("save");
        let second = save_download("db-studio-selftest.csv", b"x").expect("save again");
        assert!(second.ends_with("db-studio-selftest (2).csv"), "{second}");
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "a,b
1,2
"
        );
        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
    }
}
