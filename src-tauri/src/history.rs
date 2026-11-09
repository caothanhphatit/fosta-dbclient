//! Query history: every run in a query tab, kept in `fosta-studio-history.jsonl` next to `fosta-studio.json`.
//!
//! The file is append-only JSON Lines (one entry per line). All access goes through one mutex, so appends
//! from several tabs never interleave. When the file grows past the cap plus some slack it is rewritten
//! with the newest `cap` entries (written to a temporary file, then renamed over the old one).
//! The history never leaves this machine and never holds passwords: only the text the user ran.

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    config,
    data_files::{private_options, restrict_to_owner, write_private},
    error::AppResult,
    lock::lock,
};

pub const HISTORY_FILE: &str = "fosta-studio-history.jsonl";
/// The history file's name before the rename to Fosta Studio; moved on first use.
const LEGACY_HISTORY_FILE: &str = "db-studio-history.jsonl";
/// Entries kept after a compaction.
pub const DEFAULT_CAP: usize = 5_000;
/// Longer statements are cut so one huge script cannot bloat the file.
const MAX_SQL_CHARS: usize = 64 * 1024;
const MAX_ERROR_CHARS: usize = 4 * 1024;
const DEFAULT_LIST_LIMIT: usize = 200;

/// One result set of a run: rows for a SELECT-like statement, affected rows otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultSummary {
    pub row_set: bool,
    #[serde(default)]
    pub rows: u64,
    #[serde(default)]
    pub affected_rows: u64,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    /// Assigned by the store on append.
    #[serde(default)]
    pub id: String,
    pub sql: String,
    pub connection_id: String,
    #[serde(default)]
    pub connection_name: String,
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub database: Option<String>,
    /// Milliseconds since the Unix epoch.
    pub started_at: i64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub results: Vec<ResultSummary>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub cancelled: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    #[serde(default)]
    pub connection_id: Option<String>,
    /// Case-insensitive text that the statement, connection name or database must contain.
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub limit: usize,
    /// Only entries that started before this time (milliseconds since the epoch), for paging.
    #[serde(default)]
    pub before: Option<i64>,
}

#[derive(Default)]
struct State {
    /// Entries in the file, counted on first use.
    count: Option<usize>,
    sequence: u64,
}

pub struct HistoryStore {
    path: PathBuf,
    cap: usize,
    slack: usize,
    state: Mutex<State>,
}

impl HistoryStore {
    pub fn new(path: PathBuf, cap: usize) -> Self {
        // A file written by an earlier version keeps its permissions until it is rewritten; close it now.
        if let Err(error) = restrict_to_owner(&path) {
            tracing::warn!(path = %path.display(), %error, "could not restrict the history file to its owner");
        }
        Self {
            path,
            cap: cap.max(1),
            slack: (cap / 10).max(1),
            state: Mutex::new(State::default()),
        }
    }

    pub fn open_default() -> AppResult<Self> {
        Ok(Self::new(
            config::data_path_renamed(HISTORY_FILE, LEGACY_HISTORY_FILE)?,
            DEFAULT_CAP,
        ))
    }

    /// Appends `entry` with a new id and returns it as stored.
    pub fn append(&self, mut entry: HistoryEntry) -> AppResult<HistoryEntry> {
        let mut state = lock(&self.state);
        state.sequence += 1;
        entry.id = new_id(state.sequence);
        entry.sql = cut(entry.sql, MAX_SQL_CHARS);
        entry.error = entry.error.map(|error| cut(error, MAX_ERROR_CHARS));
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        if !ends_with_newline(&self.path)? {
            // A previous write was cut off; start a fresh line so this entry stays readable.
            line.insert(0, '\n');
        }
        private_options()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(line.as_bytes())?;
        let count = match state.count {
            Some(count) => count + 1,
            None => self.read_all()?.len(),
        };
        state.count = Some(count);
        if count > self.cap + self.slack {
            let mut entries = self.read_all()?;
            let excess = entries.len().saturating_sub(self.cap);
            entries.drain(..excess);
            self.write_all(&entries)?;
            state.count = Some(entries.len());
        }
        Ok(entry)
    }

    /// Matching entries, newest first.
    pub fn list(&self, query: &HistoryQuery) -> AppResult<Vec<HistoryEntry>> {
        let _state = lock(&self.state);
        let search = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_lowercase);
        let limit = if query.limit == 0 { DEFAULT_LIST_LIMIT } else { query.limit };
        let mut entries: Vec<HistoryEntry> = self
            .read_all()?
            .into_iter()
            .rev()
            .filter(|entry| query.connection_id.as_ref().is_none_or(|id| &entry.connection_id == id))
            .filter(|entry| query.before.is_none_or(|before| entry.started_at < before))
            .filter(|entry| search.as_ref().is_none_or(|text| matches(entry, text)))
            .collect();
        // Entries are written when a run ends; order by start time, keeping file order for ties.
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.started_at));
        entries.truncate(limit);
        Ok(entries)
    }

    /// Removes the entries with these ids and returns how many were removed.
    pub fn delete(&self, ids: &[String]) -> AppResult<usize> {
        let mut state = lock(&self.state);
        let entries = self.read_all()?;
        let before = entries.len();
        let kept: Vec<HistoryEntry> = entries.into_iter().filter(|entry| !ids.contains(&entry.id)).collect();
        let removed = before - kept.len();
        if removed > 0 {
            self.write_all(&kept)?;
        }
        state.count = Some(kept.len());
        Ok(removed)
    }

    pub fn clear(&self) -> AppResult<()> {
        let mut state = lock(&self.state);
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        state.count = Some(0);
        Ok(())
    }

    /// Every readable entry in file order (oldest first); damaged lines are skipped.
    fn read_all(&self) -> AppResult<Vec<HistoryEntry>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut entries = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    fn write_all(&self, entries: &[HistoryEntry]) -> AppResult<()> {
        let mut text = String::new();
        for entry in entries {
            text.push_str(&serde_json::to_string(entry)?);
            text.push('\n');
        }
        write_private(&self.path, text.as_bytes())
    }
}

fn matches(entry: &HistoryEntry, search: &str) -> bool {
    entry.sql.to_lowercase().contains(search)
        || entry.connection_name.to_lowercase().contains(search)
        || entry
            .database
            .as_deref()
            .is_some_and(|database| database.to_lowercase().contains(search))
}

fn ends_with_newline(path: &Path) -> AppResult<bool> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

fn new_id(sequence: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}-{sequence:x}")
}

fn cut(text: String, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => text[..end].to_owned(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let path = std::env::temp_dir().join(format!("fosta-studio-history-{name}-{}-{nanos}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn store(&self, cap: usize) -> HistoryStore {
            HistoryStore::new(self.0.join(HISTORY_FILE), cap)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(sql: &str, connection: &str, started_at: i64) -> HistoryEntry {
        HistoryEntry {
            sql: sql.into(),
            connection_id: connection.into(),
            connection_name: format!("{connection} name"),
            driver: "mariadb".into(),
            database: Some("shop".into()),
            started_at,
            duration_ms: 5,
            results: vec![ResultSummary {
                row_set: true,
                rows: 3,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn query(limit: usize) -> HistoryQuery {
        HistoryQuery {
            limit,
            ..Default::default()
        }
    }

    #[test]
    fn appends_and_lists_newest_first() {
        let dir = TempDir::new("append");
        let store = dir.store(100);
        assert!(store.list(&query(10)).unwrap().is_empty());
        let first = store.append(entry("SELECT 1", "a", 1_000)).unwrap();
        let second = store.append(entry("SELECT 2", "a", 2_000)).unwrap();
        assert!(!first.id.is_empty());
        assert_ne!(first.id, second.id);
        let listed = store.list(&query(10)).unwrap();
        assert_eq!(
            listed.iter().map(|item| item.sql.as_str()).collect::<Vec<_>>(),
            ["SELECT 2", "SELECT 1"]
        );
        assert_eq!(listed[1], first);
        // The file is JSON Lines with camelCase keys.
        let text = fs::read_to_string(dir.0.join(HISTORY_FILE)).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.contains("\"connectionId\":\"a\""));
    }

    #[test]
    fn filters_by_connection_search_and_before() {
        let dir = TempDir::new("filter");
        let store = dir.store(100);
        store.append(entry("SELECT * FROM products", "a", 1_000)).unwrap();
        store.append(entry("select * from orders", "b", 2_000)).unwrap();
        store.append(entry("UPDATE products SET stock = 1", "a", 3_000)).unwrap();
        let only_a = store
            .list(&HistoryQuery {
                connection_id: Some("a".into()),
                ..query(10)
            })
            .unwrap();
        assert_eq!(only_a.len(), 2);
        let search = store
            .list(&HistoryQuery {
                search: Some("  PRODUCTS ".into()),
                ..query(10)
            })
            .unwrap();
        assert_eq!(search.iter().map(|item| item.started_at).collect::<Vec<_>>(), [3_000, 1_000]);
        let by_name = store
            .list(&HistoryQuery {
                search: Some("b name".into()),
                ..query(10)
            })
            .unwrap();
        assert_eq!(by_name.len(), 1);
        let older = store
            .list(&HistoryQuery {
                before: Some(3_000),
                ..query(1)
            })
            .unwrap();
        assert_eq!(older.len(), 1);
        assert_eq!(older[0].started_at, 2_000);
    }

    #[test]
    fn compacts_to_the_newest_entries_when_over_the_cap() {
        let dir = TempDir::new("cap");
        let store = dir.store(10);
        for index in 0..12 {
            store.append(entry(&format!("SELECT {index}"), "a", index)).unwrap();
        }
        // Cap 10 + slack 1: the 12th append triggers the compaction.
        let listed = store.list(&query(100)).unwrap();
        assert_eq!(listed.len(), 10);
        assert_eq!(listed.first().unwrap().sql, "SELECT 11");
        assert_eq!(listed.last().unwrap().sql, "SELECT 2");
        let text = fs::read_to_string(dir.0.join(HISTORY_FILE)).unwrap();
        assert_eq!(text.lines().count(), 10);
        assert!(!dir.0.join("fosta-studio-history.jsonl.tmp").exists());
    }

    #[test]
    fn deletes_and_clears() {
        let dir = TempDir::new("delete");
        let store = dir.store(100);
        let first = store.append(entry("SELECT 1", "a", 1)).unwrap();
        store.append(entry("SELECT 2", "a", 2)).unwrap();
        assert_eq!(store.delete(&[first.id.clone(), "missing".into()]).unwrap(), 1);
        let listed = store.list(&query(10)).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].sql, "SELECT 2");
        store.clear().unwrap();
        assert!(store.list(&query(10)).unwrap().is_empty());
        store.append(entry("SELECT 3", "a", 3)).unwrap();
        assert_eq!(store.list(&query(10)).unwrap().len(), 1);
    }

    #[test]
    fn skips_damaged_lines_and_recovers_from_a_cut_write() {
        let dir = TempDir::new("damaged");
        let path = dir.0.join(HISTORY_FILE);
        fs::write(
            &path,
            "not json\n{\"sql\":\"SELECT 1\",\"connectionId\":\"a\",\"startedAt\":1}\n{\"sql\":\"SEL",
        )
        .unwrap();
        let store = HistoryStore::new(path, 100);
        store.append(entry("SELECT 2", "a", 2)).unwrap();
        let listed = store.list(&query(10)).unwrap();
        assert_eq!(
            listed.iter().map(|item| item.sql.as_str()).collect::<Vec<_>>(),
            ["SELECT 2", "SELECT 1"]
        );
    }

    #[test]
    fn concurrent_appends_stay_whole() {
        let dir = TempDir::new("concurrent");
        let store = Arc::new(dir.store(10_000));
        let threads: Vec<_> = (0..8)
            .map(|thread| {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    for index in 0..25 {
                        store.append(entry(&format!("SELECT {thread}, {index}"), "a", index)).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let listed = store.list(&query(1_000)).unwrap();
        assert_eq!(listed.len(), 200);
        let mut ids: Vec<&str> = listed.iter().map(|item| item.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 200);
    }

    #[test]
    fn cuts_very_long_statements() {
        let dir = TempDir::new("long");
        let store = dir.store(10);
        let stored = store.append(entry(&"x".repeat(MAX_SQL_CHARS + 10), "a", 1)).unwrap();
        assert_eq!(stored.sql.chars().count(), MAX_SQL_CHARS);
    }
}
