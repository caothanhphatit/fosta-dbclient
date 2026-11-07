//! Exports land in the user's Downloads folder, written from Rust so they do
//! not depend on the webview's download handling.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::error::{AppError, AppResult};

/// Writes `bytes` as `name` in Downloads (adding " (2)", " (3)", … when taken) and returns the path.
pub fn save_download(name: &str, bytes: &[u8]) -> AppResult<String> {
    let directory = downloads_dir()?;
    fs::create_dir_all(&directory)?;
    let path = unique_path(&directory, &sanitize(name));
    fs::write(&path, bytes)?;
    Ok(path.to_string_lossy().into_owned())
}

/// Opens Explorer with the file selected.
pub fn reveal(path: &str) -> AppResult<()> {
    if !Path::new(path).exists() {
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
