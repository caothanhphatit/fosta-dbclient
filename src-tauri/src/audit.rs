//! Local safety audit log: blocked, confirmed and relaxed operations, kept in
//! `fosta-studio-audit.jsonl` next to `fosta-studio.json`.
//!
//! Same layout as the query history (`history.rs`): append-only JSON Lines under one mutex,
//! rewritten with the newest `cap` entries when it grows past the cap plus some slack. The log
//! never leaves this machine; it holds statement excerpts, never passwords.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{config, error::AppResult, lock::lock};

pub const AUDIT_FILE: &str = "fosta-studio-audit.jsonl";
const DEFAULT_CAP: usize = 20_000;
const MAX_STATEMENT_CHARS: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    /// Refused by the rules or by the filter validator.
    Blocked,
    /// Ran after the user confirmed it.
    Confirmed,
    /// A workspace's rules were made less strict.
    Relaxed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// Milliseconds since the Unix epoch.
    pub time: i64,
    /// `queryTab`, `gridFilter`, `gridSave`, `import`, `createDatabase`, `assistant`, `settings`…
    pub caller: String,
    pub workspace_id: String,
    pub workspace: String,
    #[serde(default)]
    pub connection_id: String,
    #[serde(default)]
    pub connection: String,
    /// Statement category (`write`, `destructive`, …), `filter`, or `rules` for relaxations.
    pub category: String,
    pub statement: String,
    pub outcome: Option<Outcome>,
    #[serde(default)]
    pub detail: String,
}

pub struct AuditLog {
    path: PathBuf,
    cap: usize,
    state: Mutex<Option<usize>>,
}

impl AuditLog {
    pub fn new(path: PathBuf, cap: usize) -> Self {
        Self {
            path,
            cap: cap.max(1),
            state: Mutex::new(None),
        }
    }

    pub fn open_default() -> AppResult<Self> {
        Ok(Self::new(config::data_path(AUDIT_FILE)?, DEFAULT_CAP))
    }

    pub fn append(&self, mut entry: AuditEntry) -> AppResult<()> {
        let mut count = lock(&self.state);
        if entry.time == 0 {
            entry.time = now_millis();
        }
        if entry.statement.chars().count() > MAX_STATEMENT_CHARS {
            entry.statement = entry.statement.chars().take(MAX_STATEMENT_CHARS).collect::<String>() + "…";
        }
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(line.as_bytes())?;
        let next = match *count {
            Some(count) => count + 1,
            None => self.read_all()?.len(),
        };
        *count = Some(next);
        if next > self.cap + self.cap / 10 {
            let mut entries = self.read_all()?;
            let excess = entries.len().saturating_sub(self.cap);
            entries.drain(..excess);
            let temporary = self.path.with_extension("jsonl.tmp");
            let mut text = String::new();
            for entry in &entries {
                text.push_str(&serde_json::to_string(entry)?);
                text.push('\n');
            }
            fs::write(&temporary, text)?;
            fs::rename(&temporary, &self.path)?;
            *count = Some(entries.len());
        }
        Ok(())
    }

    /// Every readable entry, oldest first.
    pub fn read_all(&self) -> AppResult<Vec<AuditEntry>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut entries = Vec::new();
        for line in BufReader::new(file).lines() {
            if let Ok(entry) = serde_json::from_str::<AuditEntry>(&line?) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }
}

pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_compacts() {
        let path = std::env::temp_dir().join(format!("fosta-audit-{}.jsonl", std::process::id()));
        let _ = fs::remove_file(&path);
        let log = AuditLog::new(path.clone(), 10);
        for index in 0..15 {
            log.append(AuditEntry {
                caller: "queryTab".into(),
                category: "write".into(),
                statement: format!("DELETE FROM t WHERE id = {index}"),
                outcome: Some(Outcome::Blocked),
                ..AuditEntry::default()
            })
            .unwrap();
        }
        let entries = log.read_all().unwrap();
        assert!(entries.len() <= 11, "{} entries", entries.len());
        assert!(entries.last().unwrap().statement.ends_with("= 14"));
        assert!(entries[0].time > 0);
        fs::remove_file(path).unwrap();
    }
}
