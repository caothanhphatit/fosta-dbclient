//! The open tabs of each workspace (`fosta-studio-tabs.json` next to the config, on the web server in the data volume), so
//! tabs and the text of query tabs come back after a restart.
//!
//! The UI owns the shape of a workspace's state (which tabs, the active one, query text); the store keeps it as JSON,
//! one entry per workspace, and bounds its size.
//!
//! - **Revisions.** Each workspace's entry has a revision that every save increments. A save names the revision it was
//!   based on; when another window saved in between, nothing is written and the save gets the stored state back
//!   (`SaveOutcome::saved` false), so the UI can merge its tabs into it instead of overwriting them.
//! - **Reads.** A missing file is an empty store. Any other read error fails the call and nothing is written, so a
//!   transient error never wipes the other workspaces. A file that does not parse is moved aside to
//!   `fosta-studio-tabs.json.corrupt` (replacing an older one) and the store starts empty.
//! - **Writes** go to a temporary file of their own, flushed to disk, then renamed over the file, so a crash never
//!   leaves half a file.
//! - Entries of workspaces that are no longer in the configuration are dropped on the next save.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    data_files::private_options,
    error::{AppError, AppResult},
    lock::lock,
};

pub const OPEN_TABS_FILE: &str = "fosta-studio-tabs.json";
/// Where a tabs file that does not parse is moved (the last one is kept).
pub const CORRUPT_SUFFIX: &str = ".corrupt";
/// The most one workspace's tabs may take as JSON (the UI keeps each query text to 1 MB and stays under this).
pub const MAX_WORKSPACE_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORKSPACE_ID_CHARS: usize = 200;
const VERSION: u32 = 1;

/// Numbers the temporary files, so two writes never share one.
static WRITES: AtomicU64 = AtomicU64::new(0);

#[derive(Default, Serialize, Deserialize)]
struct StoredTabs {
    version: u32,
    #[serde(default)]
    workspaces: Map<String, Value>,
    /// The revision of each workspace's entry (absent: 0, a file written before revisions).
    #[serde(default)]
    revisions: BTreeMap<String, u64>,
}

impl StoredTabs {
    fn revision(&self, id: &str) -> u64 {
        self.revisions.get(id).copied().unwrap_or(0)
    }
}

/// Every workspace's saved tabs and the revision each was saved at (`load_open_tabs`).
#[derive(Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedTabs {
    pub workspaces: Map<String, Value>,
    pub revisions: BTreeMap<String, u64>,
}

/// The answer to a save (`save_open_tabs`).
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOutcome {
    /// False when the stored revision was not the one the save was based on: nothing was written.
    pub saved: bool,
    /// The workspace's revision now (after the write, or the stored one on a conflict).
    pub revision: u64,
    /// On a conflict, the stored state of the workspace (null when it has none).
    pub current: Option<Value>,
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

    /// The saved state of every workspace, by workspace id. A missing file gives none; a file that does not parse is
    /// moved aside and gives none; any other read error fails.
    pub fn load(&self) -> AppResult<LoadedTabs> {
        let _guard = lock(&self.guard);
        let stored = read(&self.path)?;
        Ok(LoadedTabs {
            workspaces: stored.workspaces,
            revisions: stored.revisions,
        })
    }

    /// Stores (or, with `None`, forgets) the state of one workspace; the other workspaces' entries stay, except those
    /// `exists` no longer knows. `expected`: the revision the state is based on (0: none saved yet); a different stored
    /// revision writes nothing and returns the stored state. `None` saves whatever is stored (forgetting a workspace).
    pub fn save(
        &self,
        workspace_id: &str,
        state: Option<Value>,
        expected: Option<u64>,
        exists: &dyn Fn(&str) -> bool,
    ) -> AppResult<SaveOutcome> {
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
        let mut stored = read(&self.path)?;
        let current = stored.revision(id);
        if expected.is_some_and(|expected| expected != current) {
            return Ok(SaveOutcome {
                saved: false,
                revision: current,
                current: stored.workspaces.get(id).cloned(),
            });
        }
        let before = stored.workspaces.len() + stored.revisions.len();
        stored.workspaces.retain(|key, _| exists(key));
        stored.revisions.retain(|key, _| exists(key));
        let mut changed = before != stored.workspaces.len() + stored.revisions.len();
        stored.version = VERSION;
        let revision = match state {
            // A workspace deleted meanwhile (a window that had not heard yet) is not stored again.
            Some(state) if exists(id) => {
                let revision = current + 1;
                stored.workspaces.insert(id.to_owned(), state);
                stored.revisions.insert(id.to_owned(), revision);
                changed = true;
                revision
            }
            _ => {
                changed |= stored.workspaces.remove(id).is_some();
                changed |= stored.revisions.remove(id).is_some();
                0
            }
        };
        if changed {
            write(&self.path, &stored)?;
        }
        Ok(SaveOutcome {
            saved: true,
            revision,
            current: None,
        })
    }
}

fn corrupt_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(CORRUPT_SUFFIX);
    PathBuf::from(name)
}

fn read(path: &Path) -> AppResult<StoredTabs> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(StoredTabs::default()),
        Err(error) => return Err(AppError::new(format!("Could not read the open tabs of the last session: {error}"))),
    };
    match serde_json::from_slice::<StoredTabs>(&bytes) {
        Ok(stored) => Ok(stored),
        Err(parse) => {
            // Kept for a look (the last one only); the tabs start over instead of keeping the app from starting.
            fs::rename(path, corrupt_path(path))
                .map_err(|error| AppError::new(format!("The open tabs file is damaged and could not be moved aside: {error}")))?;
            tracing::warn!(error = %parse, "the open tabs file did not parse; it was moved aside");
            Ok(StoredTabs::default())
        }
    }
}

fn write(path: &Path, stored: &StoredTabs) -> AppResult<()> {
    let bytes = serde_json::to_vec(stored)?;
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}-{}.tmp", std::process::id(), WRITES.fetch_add(1, Ordering::Relaxed)));
    let temporary = PathBuf::from(name);
    let written = (|| -> io::Result<()> {
        // Query text as typed: readable by the owner only, like the other data files.
        let mut file = private_options().write(true).create(true).truncate(true).open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
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

        fn file(&self) -> PathBuf {
            self.0.join(OPEN_TABS_FILE)
        }

        fn store(&self) -> OpenTabsStore {
            OpenTabsStore::new(self.file())
        }

        fn entries(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn all(_: &str) -> bool {
        true
    }

    #[test]
    fn workspaces_are_saved_and_loaded_on_their_own() {
        let folder = Folder::new("roundtrip");
        let store = folder.store();
        assert!(store.load().unwrap().workspaces.is_empty());
        let first = json!({ "version": 1, "tabs": [{ "kind": "query", "text": "SELECT 1" }], "active": 0 });
        store.save("ws-1", Some(first.clone()), Some(0), &all).unwrap();
        store
            .save("ws-2", Some(json!({ "version": 1, "tabs": [], "active": null })), None, &all)
            .unwrap();
        // A new store (the next start) reads both back.
        let loaded = folder.store().load().unwrap();
        assert_eq!(loaded.workspaces.get("ws-1"), Some(&first));
        assert_eq!(loaded.workspaces.len(), 2);
        assert_eq!(loaded.revisions.get("ws-1"), Some(&1));
        store.save("ws-2", None, None, &all).unwrap();
        let loaded = folder.store().load().unwrap();
        assert_eq!(loaded.workspaces.len(), 1);
        assert!(!loaded.revisions.contains_key("ws-2"));
    }

    #[test]
    fn two_saves_in_a_row_both_land_and_leave_no_temporary_files() {
        let folder = Folder::new("twice");
        let store = folder.store();
        let first = store.save("ws", Some(json!({ "tabs": [1] })), Some(0), &all).unwrap();
        assert_eq!((first.saved, first.revision), (true, 1));
        let second = store.save("ws", Some(json!({ "tabs": [2] })), Some(1), &all).unwrap();
        assert_eq!((second.saved, second.revision), (true, 2));
        assert_eq!(store.load().unwrap().workspaces.get("ws"), Some(&json!({ "tabs": [2] })));
        assert_eq!(folder.entries(), [OPEN_TABS_FILE]);
    }

    #[test]
    fn a_save_based_on_an_old_revision_writes_nothing_and_gets_the_stored_state() {
        let folder = Folder::new("conflict");
        let store = folder.store();
        store.save("ws", Some(json!({ "tabs": ["a"] })), Some(0), &all).unwrap();
        // A second window that loaded before the first one saved.
        let stale = store.save("ws", Some(json!({ "tabs": ["b"] })), Some(0), &all).unwrap();
        assert_eq!(
            stale,
            SaveOutcome {
                saved: false,
                revision: 1,
                current: Some(json!({ "tabs": ["a"] }))
            }
        );
        assert_eq!(store.load().unwrap().workspaces.get("ws"), Some(&json!({ "tabs": ["a"] })));
        // Merged and based on the stored revision, it lands.
        let merged = store.save("ws", Some(json!({ "tabs": ["a", "b"] })), Some(1), &all).unwrap();
        assert_eq!((merged.saved, merged.revision), (true, 2));
    }

    #[test]
    fn oversized_state_and_missing_ids_are_refused() {
        let folder = Folder::new("limits");
        let store = folder.store();
        let huge = json!({ "tabs": [{ "text": "x".repeat(MAX_WORKSPACE_BYTES + 1) }] });
        assert!(store.save("ws", Some(huge), None, &all).is_err());
        assert!(store.save("  ", Some(json!({})), None, &all).is_err());
        assert!(store.load().unwrap().workspaces.is_empty());
    }

    #[test]
    fn a_file_that_does_not_parse_is_moved_aside_and_the_store_starts_empty() {
        let folder = Folder::new("corrupt");
        fs::write(folder.file(), b"{ not json").unwrap();
        let store = folder.store();
        assert!(store.load().unwrap().workspaces.is_empty());
        let aside = corrupt_path(&folder.file());
        assert_eq!(fs::read(&aside).unwrap(), b"{ not json");
        assert!(!folder.file().exists());
        store.save("ws", Some(json!({ "tabs": [] })), Some(0), &all).unwrap();
        assert_eq!(store.load().unwrap().workspaces.len(), 1);
        // A second damaged file replaces the first one aside.
        fs::write(folder.file(), b"[truncated").unwrap();
        store.save("ws", Some(json!({ "tabs": [] })), Some(0), &all).unwrap();
        assert_eq!(fs::read(&aside).unwrap(), b"[truncated");
    }

    #[test]
    fn a_read_error_fails_the_save_and_writes_nothing() {
        let folder = Folder::new("unreadable");
        // A folder where the file should be: reading it fails, and not with "not found".
        fs::create_dir(folder.file()).unwrap();
        fs::write(folder.file().join("keep"), b"x").unwrap();
        let store = folder.store();
        assert!(store.load().is_err());
        assert!(store.save("ws", Some(json!({ "tabs": [] })), None, &all).is_err());
        assert!(folder.file().is_dir());
        assert!(folder.file().join("keep").exists());
        assert_eq!(folder.entries(), [OPEN_TABS_FILE]);
    }

    #[test]
    fn workspaces_no_longer_configured_are_pruned_on_save() {
        let folder = Folder::new("prune");
        let store = folder.store();
        store.save("gone", Some(json!({ "tabs": [] })), None, &all).unwrap();
        store.save("kept", Some(json!({ "tabs": [] })), None, &all).unwrap();
        let only_kept = |id: &str| id == "kept";
        store.save("kept", Some(json!({ "tabs": [1] })), None, &only_kept).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.workspaces.keys().collect::<Vec<_>>(), ["kept"]);
        assert_eq!(loaded.revisions.keys().collect::<Vec<_>>(), ["kept"]);
        // A save for a workspace deleted meanwhile is not stored.
        store.save("gone", Some(json!({ "tabs": [] })), None, &only_kept).unwrap();
        assert!(!store.load().unwrap().workspaces.contains_key("gone"));
    }

    #[test]
    fn forgetting_a_workspace_without_a_file_writes_nothing() {
        let folder = Folder::new("forget");
        folder.store().save("ws", None, None, &all).unwrap();
        assert!(!folder.file().exists());
    }
}
