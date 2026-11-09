//! The open tabs of each workspace (`fosta-tabs.json` next to the config, on the web server in the data volume), so
//! tabs and the text of query tabs come back after a restart.
//!
//! The UI owns the shape of a workspace's state (which tabs, the active one, query text); the store keeps it as JSON,
//! one entry per workspace, and bounds its size. Writes go to a temporary file first, so a crash never leaves half a file.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    error::{AppError, AppResult},
    lock::lock,
};

pub const OPEN_TABS_FILE: &str = "fosta-tabs.json";
/// The most one workspace's tabs may take as JSON (the UI keeps each query text to 1 MB and stays under this).
pub const MAX_WORKSPACE_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORKSPACE_ID_CHARS: usize = 200;
const VERSION: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
struct StoredTabs {
    version: u32,
    #[serde(default)]
    workspaces: Map<String, Value>,
}

pub struct OpenTabsStore {
    path: PathBuf,
    /// Serialises read-modify-write of the file across concurrent saves.
    guard: Mutex<()>,
}

impl OpenTabsStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            guard: Mutex::new(()),
        }
    }

    /// The saved state of every workspace, by workspace id. A missing or unreadable file gives none (tabs are a
    /// convenience: a broken file must not keep the app from starting).
    pub fn load(&self) -> AppResult<Map<String, Value>> {
        let _guard = lock(&self.guard);
        Ok(read(&self.path).workspaces)
    }

    /// Stores (or, with `None`, forgets) the state of one workspace; the other workspaces' entries stay.
    pub fn save(&self, workspace_id: &str, state: Option<Value>) -> AppResult<()> {
        let id = workspace_id.trim();
        if id.is_empty() || id.chars().count() > MAX_WORKSPACE_ID_CHARS {
            return Err(AppError::new("Saving the open tabs needs a workspace id"));
        }
        if let Some(state) = &state {
            let size = serde_json::to_vec(state)?.len();
            if size > MAX_WORKSPACE_BYTES {
                return Err(AppError::new(format!(
                    "The open tabs of this workspace are too large to save ({} MB, at most {} MB)",
                    size / (1024 * 1024),
                    MAX_WORKSPACE_BYTES / (1024 * 1024)
                )));
            }
        }
        let _guard = lock(&self.guard);
        let mut stored = read(&self.path);
        stored.version = VERSION;
        match state {
            Some(state) => {
                stored.workspaces.insert(id.to_owned(), state);
            }
            None => {
                if stored.workspaces.remove(id).is_none() && !self.path.exists() {
                    return Ok(());
                }
            }
        }
        write(&self.path, &stored)
    }
}

fn read(path: &Path) -> StoredTabs {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StoredTabs>(&bytes).ok())
        .unwrap_or_default()
}

fn write(path: &Path, stored: &StoredTabs) -> AppResult<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, serde_json::to_vec(stored)?)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("fosta-open-tabs-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temporary folder");
            Self(path)
        }

        fn store(&self) -> OpenTabsStore {
            OpenTabsStore::new(self.0.join(OPEN_TABS_FILE))
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn workspaces_are_saved_and_loaded_on_their_own() {
        let folder = Folder::new("roundtrip");
        let store = folder.store();
        assert!(store.load().unwrap().is_empty());
        let first = json!({ "version": 1, "tabs": [{ "kind": "query", "text": "SELECT 1" }], "active": 0 });
        store.save("ws-1", Some(first.clone())).unwrap();
        store
            .save("ws-2", Some(json!({ "version": 1, "tabs": [], "active": null })))
            .unwrap();
        // A new store (the next start) reads both back.
        let loaded = folder.store().load().unwrap();
        assert_eq!(loaded.get("ws-1"), Some(&first));
        assert_eq!(loaded.len(), 2);
        store.save("ws-2", None).unwrap();
        assert_eq!(folder.store().load().unwrap().len(), 1);
    }

    #[test]
    fn oversized_state_and_missing_ids_are_refused() {
        let folder = Folder::new("limits");
        let store = folder.store();
        let huge = json!({ "tabs": [{ "text": "x".repeat(MAX_WORKSPACE_BYTES + 1) }] });
        assert!(store.save("ws", Some(huge)).is_err());
        assert!(store.save("  ", Some(json!({}))).is_err());
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_file_loads_as_no_tabs_and_is_replaced_on_save() {
        let folder = Folder::new("corrupt");
        fs::write(folder.0.join(OPEN_TABS_FILE), b"{ not json").unwrap();
        let store = folder.store();
        assert!(store.load().unwrap().is_empty());
        store.save("ws", Some(json!({ "tabs": [] }))).unwrap();
        assert_eq!(store.load().unwrap().len(), 1);
    }

    #[test]
    fn forgetting_a_workspace_without_a_file_writes_nothing() {
        let folder = Folder::new("forget");
        folder.store().save("ws", None).unwrap();
        assert!(!folder.0.join(OPEN_TABS_FILE).exists());
    }
}
