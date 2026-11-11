//! Where profiles are kept: one file per connection and database, `<data dir>/fosta-studio-profiles/<sha256>.json`
//! (the hash of the connection id and the database name), readable by the owner only (`data_files::write_private`).
//!
//! - **Reads.** A missing file is no profile. A file that does not parse (or has another version, or belongs to another
//!   connection) is moved aside to `<name>.json.corrupt`, replacing an older one, and reads as no profile; any other
//!   read error fails the call.
//! - **Caps.** A profile file stays under [`MAX_PROFILE_BYTES`]: when a build is larger, the details of the last objects
//!   are dropped (name and kind kept), then the last objects themselves, and the profile is marked truncated.
//! - **Refresh.** A new build replaces the structure; the user's notes and descriptions stay, and the model's
//!   descriptions and the value hints of an object stay while its fingerprint is unchanged.
//! - Profiles of a deleted connection are deleted with it (`remove_connection`).

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use super::{
    fingerprint::{self, ObjectDiff},
    model::{DatabaseProfile, Description, DescriptionSource, ProfileNote, ProfileSummary, StructureChecks, PROFILE_VERSION},
    redact,
    values::strip_value_hints,
};
use crate::{
    audit::now_millis,
    config,
    data_files::write_private,
    error::{AppError, AppResult},
    lock::lock,
};

/// The folder of profiles in the data directory.
pub const PROFILES_DIR: &str = "fosta-studio-profiles";
/// Where a profile file that does not parse is moved (the last one is kept).
pub const CORRUPT_SUFFIX: &str = ".corrupt";
/// The most one profile file may take.
pub const MAX_PROFILE_BYTES: usize = 4 * 1024 * 1024;
/// How long a stale check answer is reused.
pub const STALE_CHECK_TTL: Duration = Duration::from_secs(10 * 60);
/// The user's notes per profile, and the characters of one note or description.
pub const MAX_NOTES: usize = 500;
pub const MAX_NOTE_CHARS: usize = 4_000;
pub const MAX_DESCRIPTION_CHARS: usize = 2_000;
const MAX_NOTE_ID_CHARS: usize = 100;
const MAX_NAME_CHARS: usize = 500;

/// The fields of a profile file a status or a cleanup needs (the rest of the file is skipped).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileHeader {
    pub version: u32,
    pub engine: String,
    pub connection_id: String,
    pub database: String,
    pub learned_at: i64,
    #[serde(default)]
    pub checks: StructureChecks,
    pub total_objects: usize,
    pub detailed_objects: usize,
    #[serde(default)]
    pub data_read: bool,
}

/// What the assistant panel shows about a profile (`profile_status`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileStatus {
    pub exists: bool,
    pub learned_at: Option<i64>,
    pub objects: usize,
    pub detailed_objects: usize,
    pub data_read: bool,
    /// The structure changed since the profile was built; `None` when it could not be checked (not connected).
    pub stale: Option<bool>,
    /// When the stale answer was computed (it is reused for [`STALE_CHECK_TTL`]).
    pub checked_at: Option<i64>,
}

impl ProfileStatus {
    pub fn of(header: &ProfileHeader) -> Self {
        Self {
            exists: true,
            learned_at: Some(header.learned_at),
            objects: header.total_objects,
            detailed_objects: header.detailed_objects,
            data_read: header.data_read,
            stale: None,
            checked_at: None,
        }
    }
}

pub struct ProfileStore {
    dir: PathBuf,
    /// Serialises read-modify-write of the files.
    guard: Mutex<()>,
    /// Profiles being built now (one build per profile at a time).
    learning: Mutex<HashSet<String>>,
    /// Recent stale answers by profile key: when, and whether it was stale.
    stale: Mutex<HashMap<String, (Instant, i64, bool)>>,
}

/// Releases a profile's build slot when the build ends.
pub struct LearningGuard<'a> {
    store: &'a ProfileStore,
    key: String,
}

impl Drop for LearningGuard<'_> {
    fn drop(&mut self) {
        lock(&self.store.learning).remove(&self.key);
    }
}

/// The file name of a profile: the SHA-256 of the connection id and the database name.
pub fn profile_key(connection_id: &str, database: &str) -> String {
    fingerprint::sha256_hex(&format!("{connection_id}\n{database}"))
}

fn corrupt_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(CORRUPT_SUFFIX);
    PathBuf::from(name)
}

impl ProfileStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            guard: Mutex::new(()),
            learning: Mutex::new(HashSet::new()),
            stale: Mutex::new(HashMap::new()),
        }
    }

    pub fn open_default() -> AppResult<Self> {
        Ok(Self::new(config::data_path(PROFILES_DIR)?))
    }

    pub fn path(&self, connection_id: &str, database: &str) -> PathBuf {
        self.dir.join(format!("{}.json", profile_key(connection_id, database)))
    }

    /// Reads and parses one profile file as `T`. Missing: `None`. Unparsable, of another version or of another
    /// connection: moved aside, `None`.
    fn read<T: for<'de> Deserialize<'de>>(&self, path: &Path, connection_id: &str, database: &str) -> AppResult<Option<T>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AppError::new(format!("Could not read what the assistant learned: {error}"))),
        };
        let header = serde_json::from_slice::<ProfileHeader>(&bytes);
        let problem = match &header {
            Err(error) => Some(error.to_string()),
            Ok(header) if header.version != PROFILE_VERSION => Some(format!("version {}", header.version)),
            Ok(header) if header.connection_id != connection_id || header.database != database => {
                Some("written for another connection".to_owned())
            }
            Ok(_) => None,
        };
        let parsed = match problem {
            None => serde_json::from_slice::<T>(&bytes).map_err(|error| error.to_string()),
            Some(problem) => Err(problem),
        };
        match parsed {
            Ok(value) => Ok(Some(value)),
            Err(problem) => {
                fs::rename(path, corrupt_path(path))
                    .map_err(|error| AppError::new(format!("A profile file is damaged and could not be moved aside: {error}")))?;
                tracing::warn!(%problem, path = %path.display(), "a profile file did not parse; it was moved aside");
                Ok(None)
            }
        }
    }

    pub fn load(&self, connection_id: &str, database: &str) -> AppResult<Option<DatabaseProfile>> {
        let _guard = lock(&self.guard);
        self.read(&self.path(connection_id, database), connection_id, database)
    }

    pub fn header(&self, connection_id: &str, database: &str) -> AppResult<Option<ProfileHeader>> {
        let _guard = lock(&self.guard);
        self.read(&self.path(connection_id, database), connection_id, database)
    }

    /// Claims the build slot of a profile; a second build of the same profile is refused while one runs.
    pub fn begin_learning(&self, connection_id: &str, database: &str) -> AppResult<LearningGuard<'_>> {
        let key = profile_key(connection_id, database);
        if !lock(&self.learning).insert(key.clone()) {
            return Err(AppError::new("The assistant is already learning this database"));
        }
        Ok(LearningGuard { store: self, key })
    }

    /// Stores a fresh build: merged into the stored profile (notes, descriptions, unchanged objects' hints), capped,
    /// written. Returns the stored profile and how its objects changed.
    pub fn refresh(&self, fresh: DatabaseProfile) -> AppResult<(DatabaseProfile, ObjectDiff)> {
        self.refresh_with(fresh, true)
    }

    /// [`Self::refresh`]; without `keep_hints` (the user opted out of value sampling) the stored value hints are dropped
    /// instead of carried over, and the profile no longer says data was read.
    pub fn refresh_with(&self, fresh: DatabaseProfile, keep_hints: bool) -> AppResult<(DatabaseProfile, ObjectDiff)> {
        let _guard = lock(&self.guard);
        let path = self.path(&fresh.connection_id, &fresh.database);
        let previous: Option<DatabaseProfile> = self.read(&path, &fresh.connection_id, &fresh.database)?;
        let (mut merged, diff) = merge_with(previous, fresh, keep_hints);
        self.write(&path, &mut merged)?;
        Ok((merged, diff))
    }

    /// Replaces the user's notes. Ids must be distinct; creation times of kept notes stay.
    pub fn update_notes(&self, connection_id: &str, database: &str, notes: Vec<ProfileNote>) -> AppResult<DatabaseProfile> {
        if notes.len() > MAX_NOTES {
            return Err(AppError::new(format!("A profile keeps at most {MAX_NOTES} notes")));
        }
        self.update(connection_id, database, |profile| {
            let now = now_millis();
            let mut ids = HashSet::new();
            let mut kept = Vec::with_capacity(notes.len());
            for note in notes {
                let id = note.id.trim().to_owned();
                let text = note.text.trim().to_owned();
                if id.is_empty() || id.chars().count() > MAX_NOTE_ID_CHARS || !ids.insert(id.clone()) {
                    return Err(AppError::new("Every note needs its own id"));
                }
                if text.is_empty() {
                    continue;
                }
                if text.chars().count() > MAX_NOTE_CHARS {
                    return Err(AppError::new(format!("A note is at most {MAX_NOTE_CHARS} characters")));
                }
                let object = note
                    .object
                    .map(|object| object.trim().to_owned())
                    .filter(|object| !object.is_empty())
                    .map(|object| redact::cut(&object, MAX_NAME_CHARS));
                let before = profile.notes.iter().find(|old| old.id == id);
                let unchanged = before.is_some_and(|old| old.text == text && old.object == object);
                kept.push(ProfileNote {
                    created_at: before.map_or(now, |old| old.created_at),
                    updated_at: if unchanged { before.map_or(now, |old| old.updated_at) } else { now },
                    id,
                    text,
                    object,
                });
            }
            profile.notes = kept;
            Ok(())
        })
    }

    /// Sets (or, with none, clears) the user's description of one object.
    pub fn update_description(
        &self,
        connection_id: &str,
        database: &str,
        object: &str,
        description: Option<String>,
    ) -> AppResult<DatabaseProfile> {
        let text = description.map(|text| text.trim().to_owned()).filter(|text| !text.is_empty());
        if text.as_ref().is_some_and(|text| text.chars().count() > MAX_DESCRIPTION_CHARS) {
            return Err(AppError::new(format!(
                "A description is at most {MAX_DESCRIPTION_CHARS} characters"
            )));
        }
        self.update(connection_id, database, |profile| {
            let target = profile
                .object_mut(object)
                .ok_or_else(|| AppError::new(format!("The assistant has not learned an object called {object}")))?;
            target.description = text.map(|text| Description {
                text,
                source: DescriptionSource::User,
                updated_at: now_millis(),
                fingerprint: None,
            });
            Ok(())
        })
    }

    fn update(
        &self,
        connection_id: &str,
        database: &str,
        change: impl FnOnce(&mut DatabaseProfile) -> AppResult<()>,
    ) -> AppResult<DatabaseProfile> {
        let _guard = lock(&self.guard);
        let path = self.path(connection_id, database);
        let mut profile: DatabaseProfile = self
            .read(&path, connection_id, database)?
            .ok_or_else(|| AppError::new("The assistant has not learned this database yet"))?;
        change(&mut profile)?;
        self.write(&path, &mut profile)?;
        Ok(profile)
    }

    /// Stores what a summary run wrote: the model's descriptions of the named objects (never over the user's, unknown names
    /// ignored), each with the object's fingerprint now, the database summary when there is one, the model and the tokens
    /// (added to the counters). Returns the stored profile and how many descriptions were written.
    pub fn apply_summary(&self, connection_id: &str, database: &str, write: &SummaryWrite) -> AppResult<(DatabaseProfile, usize)> {
        let mut written = 0;
        let profile = self.update(connection_id, database, |profile| {
            let now = now_millis();
            for (name, text) in &write.descriptions {
                let Some(object) = profile.object_mut(name) else {
                    continue;
                };
                if object
                    .description
                    .as_ref()
                    .is_some_and(|description| description.source == DescriptionSource::User)
                {
                    continue;
                }
                object.description = Some(Description {
                    text: redact::cut(text, MAX_DESCRIPTION_CHARS),
                    source: DescriptionSource::Llm,
                    updated_at: now,
                    fingerprint: Some(object.fingerprint.clone()),
                });
                written += 1;
            }
            if let Some(text) = write.summary.as_deref().filter(|text| !text.trim().is_empty()) {
                profile.summary = Some(ProfileSummary {
                    text: redact::cut(text, MAX_DESCRIPTION_CHARS),
                    model: write.model.clone(),
                    created_at: now,
                });
            }
            profile.model = Some(write.model.clone());
            profile.input_tokens = profile.input_tokens.saturating_add(write.input_tokens);
            profile.output_tokens = profile.output_tokens.saturating_add(write.output_tokens);
            Ok(())
        })?;
        Ok((profile, written))
    }

    /// Deletes one profile; false when there was none.
    pub fn delete(&self, connection_id: &str, database: &str) -> AppResult<bool> {
        let _guard = lock(&self.guard);
        self.forget_stale(&profile_key(connection_id, database));
        remove(&self.path(connection_id, database))
    }

    /// Deletes every profile of a connection (it was deleted); returns how many.
    pub fn remove_connection(&self, connection_id: &str) -> AppResult<usize> {
        let _guard = lock(&self.guard);
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        let mut removed = 0;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else { continue };
            let Ok(header) = serde_json::from_slice::<ProfileHeader>(&bytes) else {
                continue;
            };
            if header.connection_id == connection_id && remove(&path)? {
                self.forget_stale(&profile_key(&header.connection_id, &header.database));
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// The stale answer computed for this profile within [`STALE_CHECK_TTL`], with when it was computed.
    pub fn cached_stale(&self, connection_id: &str, database: &str) -> Option<(i64, bool)> {
        let key = profile_key(connection_id, database);
        let mut cache = lock(&self.stale);
        match cache.get(&key) {
            Some((at, checked_at, stale)) if at.elapsed() < STALE_CHECK_TTL => Some((*checked_at, *stale)),
            Some(_) => {
                cache.remove(&key);
                None
            }
            None => None,
        }
    }

    pub fn remember_stale(&self, connection_id: &str, database: &str, stale: bool) -> i64 {
        let checked_at = now_millis();
        lock(&self.stale).insert(profile_key(connection_id, database), (Instant::now(), checked_at, stale));
        checked_at
    }

    fn forget_stale(&self, key: &str) {
        lock(&self.stale).remove(key);
    }

    /// Caps and writes a profile (owner only), and forgets its stale answer.
    fn write(&self, path: &Path, profile: &mut DatabaseProfile) -> AppResult<()> {
        let bytes = fit(profile)?;
        fs::create_dir_all(&self.dir)?;
        write_private(path, &bytes)?;
        self.forget_stale(&profile_key(&profile.connection_id, &profile.database));
        Ok(())
    }
}

fn remove(path: &Path) -> AppResult<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// The profile as JSON within [`MAX_PROFILE_BYTES`]: the last objects' details are dropped, then the last objects.
fn fit(profile: &mut DatabaseProfile) -> AppResult<Vec<u8>> {
    let mut bytes = serde_json::to_vec(profile)?;
    while bytes.len() > MAX_PROFILE_BYTES {
        let detailed: Vec<usize> = (0..profile.objects.len())
            .filter(|&index| profile.objects[index].detailed)
            .collect();
        if detailed.is_empty() {
            if profile.objects.is_empty() {
                return Err(AppError::new("This profile is too large to save"));
            }
            let keep = profile.objects.len() - profile.objects.len().div_ceil(10);
            profile.objects.truncate(keep);
        } else {
            for &index in detailed.iter().rev().take(detailed.len().div_ceil(10)) {
                let object = &mut profile.objects[index];
                object.detailed = false;
                object.comment = None;
                object.columns.clear();
                object.primary_key.clear();
                object.unique_keys.clear();
                object.indexes.clear();
                object.extra.clear();
                object.value_hints.clear();
            }
        }
        profile.truncated = true;
        profile.detailed_objects = profile.objects.iter().filter(|object| object.detailed).count();
        bytes = serde_json::to_vec(profile)?;
    }
    Ok(bytes)
}

/// `fresh` with what `previous` knew that a build does not recompute: the notes, the first build time, the summary and
/// the model's counters, the extras, the user's descriptions (of objects still there), and the model's descriptions and
/// value hints of objects whose fingerprint did not change.
pub fn merge(previous: Option<DatabaseProfile>, fresh: DatabaseProfile) -> (DatabaseProfile, ObjectDiff) {
    merge_with(previous, fresh, true)
}

/// [`merge`]; without `keep_hints` no value hint is carried over and the fresh build's own hints are dropped too.
pub fn merge_with(previous: Option<DatabaseProfile>, mut fresh: DatabaseProfile, keep_hints: bool) -> (DatabaseProfile, ObjectDiff) {
    if !keep_hints {
        strip_value_hints(&mut fresh);
        fresh.data_read = false;
    }
    let Some(previous) = previous else {
        let diff = ObjectDiff {
            added: fresh.objects.iter().map(|object| object.qualified()).collect(),
            ..ObjectDiff::default()
        };
        return (fresh, diff);
    };
    let diff = fingerprint::diff(&previous, &fresh);
    let mut old: HashMap<(String, String), _> = previous
        .objects
        .into_iter()
        .map(|object| ((object.qualified(), object.kind.to_string()), object))
        .collect();
    for object in &mut fresh.objects {
        let Some(before) = old.remove(&(object.qualified(), object.kind.to_string())) else {
            continue;
        };
        let unchanged = before.fingerprint == object.fingerprint;
        object.description = match before.description {
            Some(description) if description.source == DescriptionSource::User || unchanged => Some(description),
            _ => object.description.take(),
        };
        if keep_hints && unchanged && object.value_hints.is_empty() {
            object.value_hints = before.value_hints;
        }
    }
    fresh.notes = previous.notes;
    fresh.first_learned_at = previous.first_learned_at;
    if fresh.summary.is_none() {
        fresh.summary = previous.summary;
        fresh.model = fresh.model.or(previous.model);
        fresh.input_tokens += previous.input_tokens;
        fresh.output_tokens += previous.output_tokens;
    }
    if fresh.extras.is_empty() {
        fresh.extras = previous.extras;
    }
    fresh.data_read = fresh.data_read || fresh.objects.iter().any(|object| !object.value_hints.is_empty());
    (fresh, diff)
}

/// What a summary run hands to [`ProfileStore::apply_summary`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SummaryWrite {
    /// `(object, description)`: the object as its qualified name.
    pub descriptions: Vec<(String, String)>,
    pub summary: Option<String>,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}
