use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};

use crate::{
    ai::{AiActive, AiConnection, AiSettings},
    contract::{AuthMethodId, ConnectionSpec, SshConfig, TlsConfig, TlsMode},
    embedding::EmbeddingSettings,
    error::{AppError, AppResult},
    gateway::{assistant::AssistantQueries, rules::SafetySettings},
    lock::lock,
    secrets::LEGACY_AI_CONNECTION,
    tasks::TaskDef,
};

const CONFIG_FILE: &str = "fosta-studio.json";
/// The config file's name before the app was renamed to Fosta Studio; moved on first start.
const LEGACY_CONFIG_FILE: &str = "db-studio.json";
const DEFAULT_WORKSPACE_ID: &str = "default";
const DEFAULT_WORKSPACE_NAME: &str = "Local";
const DEFAULT_WORKSPACE_COLOR: &str = "blue";
const DEFAULT_WORKSPACE_ICON: &str = "laptop";
const DEFAULT_ENGINE: &str = "mariadb";
/// The `configVersion` this build writes; a file without one is version 1.
pub const CONFIG_VERSION: u32 = 2;
/// Copy of the last version-1 file, made once before the first version-2 write.
const V1_BACKUP_SUFFIX: &str = ".v1.bak";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    /// The engine id. The JSON key stays `driver` so older versions can still read the file.
    #[serde(rename = "driver", default = "default_engine")]
    pub engine: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub database: Option<String>,
    /// SQLite database file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// A connection string without its password (Mongo SRV / replica sets, ES node lists or Cloud ID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub save_password: bool,
    /// Engine-specific settings, kept opaque here.
    #[serde(default)]
    pub options: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthMethodId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshConfig>,
    /// Safety rules for this connection only, replacing the workspace's (see `crate::gateway`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety: Option<SafetySettings>,
}

fn default_engine() -> String {
    DEFAULT_ENGINE.into()
}

impl ConnectionProfile {
    /// What the engine needs to connect (secrets are passed separately).
    pub fn spec(&self) -> ConnectionSpec {
        let non_empty = |value: &Option<String>| value.clone().filter(|value| !value.trim().is_empty());
        ConnectionSpec {
            engine: self.engine.clone(),
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            database: non_empty(&self.database),
            file: non_empty(&self.file),
            url: non_empty(&self.url),
            options: self.options.clone(),
            tls: self.tls.clone(),
            ssh: self.ssh.clone(),
            auth: self.auth,
        }
    }

    /// Moves version-1 options into the version-2 fields; running it again changes nothing.
    /// mongodb: `options.uri` → `url`. mssql: `options.encrypt` / `options.trustServerCertificate` → `tls`.
    /// Options present win over the typed fields, since the connection dialog still writes them.
    pub fn normalize(&mut self) {
        match self.engine.as_str() {
            "mongodb" => {
                if let Some(uri) = self.options.remove("uri") {
                    let uri = uri.trim();
                    if !uri.is_empty() {
                        self.url = Some(uri.to_owned());
                    }
                }
            }
            "mssql" => {
                let encrypt = self.options.remove("encrypt");
                let trust = self.options.remove("trustServerCertificate");
                if encrypt.is_some() || trust.is_some() {
                    let tls = self.tls.get_or_insert_with(TlsConfig::default);
                    tls.mode = if encrypt.as_deref() == Some("false") {
                        TlsMode::Disable
                    } else {
                        TlsMode::Require
                    };
                    tls.accept_invalid_certs = trust.as_deref() == Some("true");
                }
            }
            _ => {}
        }
    }

    /// Whether `other` points at the same server as this saved profile, so this profile's saved secrets may be
    /// sent there.
    pub fn same_target(&self, other: &ConnectionProfile) -> bool {
        let (mut saved, mut draft) = (self.clone(), other.clone());
        saved.normalize();
        draft.normalize();
        let blank = |value: &Option<String>| value.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned);
        saved.engine == draft.engine
            && saved.host.trim() == draft.host.trim()
            && saved.port == draft.port
            && saved.user.trim() == draft.user.trim()
            && blank(&saved.url) == blank(&draft.url)
            && blank(&saved.file) == blank(&draft.file)
    }
}

/// The editable identity of a workspace, without its contents.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub icon: String,
    /// The workspace's safety rules; `None` keeps the current ones (or the default for a new workspace).
    #[serde(default)]
    pub safety: Option<SafetySettings>,
    /// Whether the assistant may run read-only queries here; `None` keeps the current choice.
    #[serde(default)]
    pub assistant_queries: Option<AssistantQueries>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub icon: String,
    /// Statement rules (see `crate::gateway`). Always set after loading: workspaces saved before
    /// the gateway existed get the default for their icon (Production → Read-only, else Open).
    #[serde(default)]
    pub safety: Option<SafetySettings>,
    /// Overrides the global "Allow the assistant to run read-only queries" setting for this workspace.
    #[serde(default)]
    pub assistant_queries: AssistantQueries,
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
    #[serde(default)]
    pub tasks: Vec<TaskDef>,
}

impl Workspace {
    fn new(meta: WorkspaceMeta) -> Self {
        let safety = meta.safety.unwrap_or_else(|| SafetySettings::for_new_workspace(&meta.icon));
        Self {
            id: meta.id,
            name: meta.name,
            color: meta.color,
            icon: meta.icon,
            safety: Some(safety),
            assistant_queries: meta.assistant_queries.unwrap_or_default(),
            connections: Vec::new(),
            tasks: Vec::new(),
        }
    }

    fn apply(&mut self, meta: WorkspaceMeta) {
        self.name = meta.name;
        self.color = meta.color;
        self.icon = meta.icon;
        if let Some(safety) = meta.safety {
            self.safety = Some(safety);
        }
        if let Some(choice) = meta.assistant_queries {
            self.assistant_queries = choice;
        }
    }

    /// The rules in force for this workspace.
    pub fn safety(&self) -> SafetySettings {
        self.safety.clone().unwrap_or_else(|| SafetySettings::default_for_icon(&self.icon))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "StoredConfig")]
pub struct AppConfig {
    /// Always [`CONFIG_VERSION`] once loaded.
    pub config_version: u32,
    pub workspaces: Vec<Workspace>,
    pub active_workspace: String,
    pub embedding: Option<EmbeddingSettings>,
    pub ai_connections: Vec<AiConnection>,
    /// Always set while there is at least one connection.
    pub ai_active: Option<AiActive>,
    /// Record query-tab runs in the local history file (on unless turned off).
    pub query_history: bool,
    /// Write debug-level entries to the application log (Settings → Debug logging).
    pub debug_logging: bool,
    /// Settings → "Allow the assistant to run read-only queries" (on unless turned off; each workspace can override it).
    pub assistant_read_queries: bool,
    /// Shown once by the UI after startup (a corrupt config file was set aside). Never written to disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_notice: Option<String>,
}

/// On-disk shape. Accepts both the current layout (`workspaces`) and the
/// legacy flat layout (`connections` + `tasks` at the top level), which is
/// migrated into a single workspace on load.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    /// Missing in version-1 files.
    #[serde(default)]
    config_version: Option<u32>,
    #[serde(default)]
    workspaces: Vec<Workspace>,
    #[serde(default)]
    active_workspace: Option<String>,
    #[serde(default)]
    embedding: Option<EmbeddingSettings>,
    #[serde(default)]
    ai_connections: Vec<AiConnection>,
    #[serde(default)]
    ai_active: Option<AiActive>,
    #[serde(default)]
    query_history: Option<bool>,
    #[serde(default)]
    debug_logging: bool,
    #[serde(default)]
    assistant_read_queries: Option<bool>,
    /// Legacy single assistant setting, migrated into one connection.
    #[serde(default)]
    ai: Option<AiSettings>,
    #[serde(default)]
    connections: Vec<ConnectionProfile>,
    #[serde(default)]
    tasks: Vec<TaskDef>,
}

impl From<StoredConfig> for AppConfig {
    fn from(stored: StoredConfig) -> Self {
        let mut workspaces = stored.workspaces;
        let legacy_items = !stored.connections.is_empty() || !stored.tasks.is_empty();
        match workspaces.first_mut() {
            Some(first) if legacy_items => {
                first.connections.extend(stored.connections);
                first.tasks.extend(stored.tasks);
            }
            Some(_) => {}
            None => workspaces.push(Workspace {
                id: DEFAULT_WORKSPACE_ID.into(),
                name: DEFAULT_WORKSPACE_NAME.into(),
                color: DEFAULT_WORKSPACE_COLOR.into(),
                icon: DEFAULT_WORKSPACE_ICON.into(),
                safety: None,
                assistant_queries: AssistantQueries::Default,
                connections: stored.connections,
                tasks: stored.tasks,
            }),
        }
        for workspace in &mut workspaces {
            if workspace.safety.is_none() {
                workspace.safety = Some(SafetySettings::default_for_icon(&workspace.icon));
            }
            // Version 1 → 2; also applied to version-2 files, where it changes nothing.
            workspace.connections.iter_mut().for_each(ConnectionProfile::normalize);
        }
        let active_workspace = stored
            .active_workspace
            .filter(|id| workspaces.iter().any(|workspace| &workspace.id == id))
            .unwrap_or_else(|| workspaces[0].id.clone());
        let mut ai_connections = stored.ai_connections;
        let mut ai_active = stored.ai_active;
        if let (true, Some(legacy)) = (ai_connections.is_empty(), stored.ai) {
            ai_connections.push(AiConnection {
                id: LEGACY_AI_CONNECTION.into(),
                name: legacy.provider.display_name().into(),
                provider: legacy.provider,
                base_url: legacy.base_url,
                default_model: legacy.model,
            });
            ai_active = None;
        }
        let mut config = Self {
            config_version: CONFIG_VERSION,
            workspaces,
            active_workspace,
            embedding: stored.embedding,
            ai_connections,
            ai_active,
            query_history: stored.query_history.unwrap_or(true),
            debug_logging: stored.debug_logging,
            assistant_read_queries: stored.assistant_read_queries.unwrap_or(true),
            startup_notice: None,
        };
        config.repair_ai_active();
        config
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        StoredConfig::default().into()
    }
}

impl AppConfig {
    /// The config of a first start: one workspace, plus a local MariaDB connection when `local_connection`.
    fn starter(local_connection: bool) -> Self {
        let mut config = Self::default();
        if !local_connection {
            return config;
        }
        config.workspaces[0].connections.push(ConnectionProfile {
            id: "local-mariadb".into(),
            name: "Local MariaDB".into(),
            engine: DEFAULT_ENGINE.into(),
            host: "127.0.0.1".into(),
            port: 3306,
            user: "root".into(),
            ..Default::default()
        });
        config
    }

    /// The workspace that holds a saved connection, with the connection.
    pub fn connection_scope(&self, connection_id: &str) -> Option<(&Workspace, &ConnectionProfile)> {
        self.workspaces.iter().find_map(|workspace| {
            workspace
                .connections
                .iter()
                .find(|profile| profile.id == connection_id)
                .map(|profile| (workspace, profile))
        })
    }

    pub fn active(&self) -> &Workspace {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == self.active_workspace)
            .unwrap_or(&self.workspaces[0])
    }

    pub fn workspace(&self, id: &str) -> AppResult<&Workspace> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .ok_or_else(|| missing_workspace(id))
    }

    fn workspace_mut(&mut self, id: &str) -> AppResult<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == id)
            .ok_or_else(|| missing_workspace(id))
    }

    pub fn connection(&self, id: &str) -> Option<&ConnectionProfile> {
        self.workspaces
            .iter()
            .flat_map(|workspace| &workspace.connections)
            .find(|profile| profile.id == id)
    }

    pub fn task(&self, id: &str) -> Option<&TaskDef> {
        self.workspaces
            .iter()
            .flat_map(|workspace| &workspace.tasks)
            .find(|task| task.id == id)
    }

    pub fn all_tasks(&self) -> impl Iterator<Item = &TaskDef> {
        self.workspaces.iter().flat_map(|workspace| &workspace.tasks)
    }

    /// Places `profile` in `workspace_id`, replacing it in place when it is
    /// already there and removing it from any other workspace first.
    pub fn put_connection(&mut self, workspace_id: &str, mut profile: ConnectionProfile) -> AppResult<()> {
        self.workspace(workspace_id)?;
        profile.normalize();
        if profile.safety.is_none() {
            // The connection dialog does not edit per-connection rules; saving keeps them.
            profile.safety = self.connection(&profile.id).and_then(|existing| existing.safety.clone());
        }
        for workspace in self.workspaces.iter_mut().filter(|workspace| workspace.id != workspace_id) {
            workspace.connections.retain(|item| item.id != profile.id);
        }
        let target = self.workspace_mut(workspace_id)?;
        upsert(&mut target.connections, profile, |item| item.id.as_str());
        Ok(())
    }

    pub fn remove_connection(&mut self, id: &str) {
        for workspace in &mut self.workspaces {
            workspace.connections.retain(|item| item.id != id);
        }
    }

    pub fn put_task(&mut self, workspace_id: &str, task: TaskDef) -> AppResult<()> {
        self.workspace(workspace_id)?;
        for workspace in self.workspaces.iter_mut().filter(|workspace| workspace.id != workspace_id) {
            workspace.tasks.retain(|item| item.id != task.id);
        }
        let target = self.workspace_mut(workspace_id)?;
        upsert(&mut target.tasks, task, |item| item.id.as_str());
        Ok(())
    }

    pub fn remove_task(&mut self, id: &str) {
        for workspace in &mut self.workspaces {
            workspace.tasks.retain(|item| item.id != id);
        }
    }

    pub fn put_workspace(&mut self, meta: WorkspaceMeta) {
        match self.workspaces.iter_mut().find(|workspace| workspace.id == meta.id) {
            Some(existing) => existing.apply(meta),
            None => self.workspaces.push(Workspace::new(meta)),
        }
    }

    pub fn remove_workspace(&mut self, id: &str) -> AppResult<Workspace> {
        if self.workspaces.len() <= 1 {
            return Err(AppError::new("The last workspace cannot be deleted"));
        }
        let index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == id)
            .ok_or_else(|| missing_workspace(id))?;
        let removed = self.workspaces.remove(index);
        if self.active_workspace == id {
            self.active_workspace = self.workspaces[0].id.clone();
        }
        Ok(removed)
    }

    pub fn set_active(&mut self, id: &str) -> AppResult<()> {
        self.workspace(id)?;
        self.active_workspace = id.to_owned();
        Ok(())
    }

    pub fn ai_connection(&self, id: &str) -> AppResult<&AiConnection> {
        self.ai_connections
            .iter()
            .find(|connection| connection.id == id)
            .ok_or_else(|| AppError::new(format!("LLM provider '{id}' no longer exists")))
    }

    /// Adds or replaces `connection`; the first one saved becomes active. When
    /// the active connection is edited, its model follows the new default if it
    /// was the old default or the provider/endpoint changed.
    pub fn put_ai_connection(&mut self, connection: AiConnection) {
        let previous = self.ai_connections.iter().find(|item| item.id == connection.id);
        if let (Some(previous), Some(active)) = (previous, self.ai_active.as_mut()) {
            let moved = previous.provider != connection.provider || previous.base_url != connection.base_url;
            if active.connection_id == connection.id && (moved || active.model == previous.default_model) {
                active.model = connection.default_model.clone();
            }
        }
        upsert(&mut self.ai_connections, connection, |item| item.id.as_str());
        self.repair_ai_active();
    }

    pub fn remove_ai_connection(&mut self, id: &str) {
        self.ai_connections.retain(|connection| connection.id != id);
        self.repair_ai_active();
    }

    pub fn set_ai_active(&mut self, connection_id: &str, model: &str) -> AppResult<()> {
        let model = model.trim();
        if model.is_empty() {
            return Err(AppError::new("Model is required"));
        }
        self.ai_connection(connection_id)?;
        self.ai_active = Some(AiActive {
            connection_id: connection_id.to_owned(),
            model: model.to_owned(),
        });
        Ok(())
    }

    /// Points `ai_active` at an existing connection (the first one, with its
    /// default model, when the active one is gone), or clears it when none is left.
    fn repair_ai_active(&mut self) {
        let valid = self
            .ai_active
            .as_ref()
            .is_some_and(|active| self.ai_connections.iter().any(|connection| connection.id == active.connection_id));
        if !valid {
            self.ai_active = self.ai_connections.first().map(|connection| AiActive {
                connection_id: connection.id.clone(),
                model: connection.default_model.clone(),
            });
        }
    }
}

fn missing_workspace(id: &str) -> AppError {
    AppError::new(format!("Workspace '{id}' no longer exists"))
}

pub struct ConfigStore {
    path: PathBuf,
    current: Mutex<AppConfig>,
}

impl ConfigStore {
    pub fn load(local_connection: bool) -> AppResult<Self> {
        let path = config_path()?;
        let current = load_file(&path, local_connection)?;
        Ok(Self {
            path,
            current: Mutex::new(current),
        })
    }

    pub fn snapshot(&self) -> AppConfig {
        self.current().clone()
    }

    /// Reads from the current config without cloning all of it.
    pub fn read<T>(&self, reader: impl FnOnce(&AppConfig) -> T) -> T {
        reader(&self.current())
    }

    pub fn connection(&self, id: &str) -> AppResult<ConnectionProfile> {
        self.current()
            .connection(id)
            .cloned()
            .ok_or_else(|| AppError::new(format!("Connection '{id}' no longer exists")))
    }

    pub fn query_history_enabled(&self) -> bool {
        self.current().query_history
    }

    pub fn task(&self, id: &str) -> AppResult<TaskDef> {
        self.current()
            .task(id)
            .cloned()
            .ok_or_else(|| AppError::new(format!("Task '{id}' no longer exists")))
    }

    /// Applies `change` and persists the result. A change that fails leaves
    /// both the in-memory and on-disk config untouched.
    pub fn update(&self, change: impl FnOnce(&mut AppConfig) -> AppResult<()>) -> AppResult<AppConfig> {
        let mut current = self.current();
        let mut next = current.clone();
        next.startup_notice = None;
        change(&mut next)?;
        write_config(&self.path, &next)?;
        *current = next.clone();
        Ok(next)
    }

    fn current(&self) -> MutexGuard<'_, AppConfig> {
        lock(&self.current)
    }
}

pub fn upsert<T>(items: &mut Vec<T>, item: T, id_of: impl Fn(&T) -> &str) {
    match items.iter().position(|existing| id_of(existing) == id_of(&item)) {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

fn config_path() -> AppResult<PathBuf> {
    data_path_renamed(CONFIG_FILE, LEGACY_CONFIG_FILE)
}

/// A file in the data directory, where `fosta-studio.json` lives.
pub fn data_path(name: &str) -> AppResult<PathBuf> {
    Ok(data_dir()?.join(name))
}

/// Environment variable naming the data directory (the web server's `/data` volume).
pub const DATA_DIR_ENV: &str = "FOSTA_STUDIO_DATA";

/// `FOSTA_STUDIO_DATA` when set, otherwise the executable's folder (the desktop app is portable).
pub fn data_dir() -> AppResult<PathBuf> {
    if let Some(directory) = std::env::var_os(DATA_DIR_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(directory));
    }
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| AppError::new("Cannot resolve the application directory"))?;
    Ok(directory.to_owned())
}

/// Like [`data_path`], but first moves a file left under its pre-rename name (`legacy`) to `name`.
pub fn data_path_renamed(name: &str, legacy: &str) -> AppResult<PathBuf> {
    let path = data_path(name)?;
    move_legacy_file(&path, &data_path(legacy)?)?;
    Ok(path)
}

/// Moves `legacy` to `path` when only the legacy file exists; never overwrites `path`.
fn move_legacy_file(path: &Path, legacy: &Path) -> AppResult<()> {
    if path.exists() || !legacy.exists() {
        return Ok(());
    }
    if fs::rename(legacy, path).is_err() {
        // Another volume or a locked file: copy instead and leave the old file in place.
        fs::copy(legacy, path)?;
    }
    Ok(())
}

/// Reads the config at `path`: a missing file becomes the starter config (written), a version-1 file is migrated
/// (after a one-time `.v1.bak` copy), and an unreadable one is moved aside to `.corrupt-<unix ts>` and replaced by
/// the starter config, with a `startup_notice` for the UI.
fn load_file(path: &Path, local_connection: bool) -> AppResult<AppConfig> {
    if !path.exists() {
        let starter = AppConfig::starter(local_connection);
        write_config(path, &starter)?;
        return Ok(starter);
    }
    let bytes = fs::read(path)?;
    match serde_json::from_slice::<StoredConfig>(&bytes) {
        Ok(stored) => {
            if stored.config_version.unwrap_or(1) < CONFIG_VERSION {
                backup_v1(path)?;
            }
            Ok(stored.into())
        }
        Err(error) => {
            let aside = sibling(path, &format!(".corrupt-{}", unix_seconds()));
            fs::rename(path, &aside)?;
            let mut starter = AppConfig::starter(local_connection);
            write_config(path, &starter)?;
            starter.startup_notice = Some(format!(
                "The settings file could not be read ({error}). It was moved to {} and Fosta Studio started with new settings.",
                aside.display()
            ));
            Ok(starter)
        }
    }
}

/// Copies a version-1 file to `<name>.v1.bak`, once: an existing backup is kept.
fn backup_v1(path: &Path) -> AppResult<()> {
    let backup = sibling(path, V1_BACKUP_SUFFIX);
    if !backup.exists() {
        fs::copy(path, &backup)?;
    }
    Ok(())
}

/// `path` with `suffix` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Writes `<name>.tmp`, syncs it, then renames it over `path`, so a crash never leaves a half-written config.
fn write_config(path: &Path, config: &AppConfig) -> AppResult<()> {
    use std::io::Write;
    let mut value = serde_json::to_value(config)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("startupNotice");
    }
    let temporary = sibling(path, ".tmp");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(serde_json::to_string_pretty(&value)?.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version-1 file as 0.1.x wrote it: all four engines, the mssql TLS flags and a MongoDB URI in `options`.
    const V1_FIXTURE: &str = r#"{
      "workspaces": [{
        "id": "default", "name": "Local", "color": "blue", "icon": "laptop",
        "connections": [
          { "id": "m1", "name": "MariaDB", "driver": "mariadb", "host": "127.0.0.1", "port": 3306, "user": "root",
            "database": "shop", "savePassword": true, "options": {} },
          { "id": "y1", "name": "MySQL", "driver": "mysql", "host": "db", "port": 3307, "user": "app", "database": null,
            "savePassword": false, "options": { "ssl": "true" } },
          { "id": "s1", "name": "SQL Server", "driver": "mssql", "host": "sql", "port": 1433, "user": "sa", "database": "ERP",
            "savePassword": true, "options": { "encrypt": "true", "trustServerCertificate": "true", "appName": "x" } },
          { "id": "s2", "name": "SQL Server plain", "driver": "mssql", "host": "sql", "port": 1433, "user": "sa",
            "savePassword": true, "options": { "encrypt": "false" } },
          { "id": "g1", "name": "Mongo", "driver": "mongodb", "host": "rs0", "port": 27017, "user": "admin",
            "savePassword": true, "options": { "uri": "mongodb+srv://admin@cluster.example.net/app", "connectionMode": "string" } },
          { "id": "d1", "name": "No driver", "host": "h", "port": 1, "user": "u" },
          { "id": "x1", "name": "Future", "driver": "postgres", "host": "pg", "port": 5432, "user": "pg", "options": { "uri": "keep" } }
        ]
      }]
    }"#;

    fn migrate(text: &str) -> AppConfig {
        serde_json::from_str(text).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("fosta-config-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn v1_profiles_migrate_to_v2() {
        let config = migrate(V1_FIXTURE);
        assert_eq!(config.config_version, CONFIG_VERSION);
        let profile = |id: &str| config.connection(id).unwrap();

        let mariadb = profile("m1");
        assert_eq!(
            (mariadb.engine.as_str(), mariadb.tls.as_ref(), mariadb.url.as_ref()),
            ("mariadb", None, None)
        );
        assert_eq!(
            profile("y1").options.get("ssl").map(String::as_str),
            Some("true"),
            "other ids are untouched"
        );

        let mssql = profile("s1");
        let tls = mssql.tls.as_ref().unwrap();
        assert_eq!((tls.mode, tls.accept_invalid_certs), (TlsMode::Require, true));
        assert_eq!(mssql.options.keys().collect::<Vec<_>>(), ["appName"]);
        let plain = profile("s2").tls.as_ref().unwrap();
        assert_eq!((plain.mode, plain.accept_invalid_certs), (TlsMode::Disable, false));

        let mongo = profile("g1");
        assert_eq!(mongo.url.as_deref(), Some("mongodb+srv://admin@cluster.example.net/app"));
        assert!(!mongo.options.contains_key("uri"));
        assert_eq!(mongo.spec().url, mongo.url);

        assert_eq!(profile("d1").engine, "mariadb", "a missing driver still means mariadb");
        let future = profile("x1");
        assert_eq!(
            (future.engine.as_str(), future.options.get("uri").map(String::as_str)),
            ("postgres", Some("keep"))
        );
    }

    #[test]
    fn migration_round_trips_and_is_idempotent() {
        let once = serde_json::to_value(migrate(V1_FIXTURE)).unwrap();
        let twice = serde_json::to_value(migrate(&once.to_string())).unwrap();
        assert_eq!(once, twice);
        assert_eq!(once["configVersion"], 2);
        let parsed: AppConfig = serde_json::from_value(once.clone()).unwrap();
        assert_eq!(
            parsed.connection("s1").unwrap().tls,
            migrate(V1_FIXTURE).connection("s1").unwrap().tls
        );
    }

    #[test]
    fn v2_output_stays_readable_by_older_versions() {
        let saved = serde_json::to_value(migrate(V1_FIXTURE)).unwrap();
        for connection in saved["workspaces"][0]["connections"].as_array().unwrap() {
            for key in ["id", "name", "driver", "host", "port", "user"] {
                assert!(connection.get(key).is_some(), "{key} missing in {connection}");
            }
            assert!(connection.get("engine").is_none());
            assert!(
                connection.get("ssh").is_none() && connection.get("auth").is_none(),
                "unset fields are left out"
            );
        }
        assert!(saved.get("startupNotice").is_none());
    }

    #[test]
    fn saving_normalizes_profiles_from_the_dialog() {
        let mut config = AppConfig::default();
        let mut profile = ConnectionProfile {
            id: "s".into(),
            name: "s".into(),
            engine: "mssql".into(),
            ..Default::default()
        };
        profile.options.insert("trustServerCertificate".into(), "true".into());
        config.put_connection("default", profile).unwrap();
        let saved = config.connection("s").unwrap();
        assert!(saved.options.is_empty());
        let tls = saved.tls.as_ref().unwrap();
        assert_eq!(
            (tls.mode, tls.accept_invalid_certs),
            (TlsMode::Require, true),
            "missing encrypt means on"
        );
    }

    #[test]
    fn saved_secrets_are_bound_to_the_target() {
        let saved = migrate(V1_FIXTURE).connection("g1").unwrap().clone();
        let mut draft = saved.clone();
        draft.name = "renamed".into();
        draft.database = Some("other".into());
        assert!(saved.same_target(&draft), "name and database do not matter");
        let mut v1_draft = draft.clone();
        v1_draft.url = None;
        v1_draft
            .options
            .insert("uri".into(), "mongodb+srv://admin@cluster.example.net/app".into());
        assert!(saved.same_target(&v1_draft), "the dialog's options.uri compares as url");
        let changes: [fn(&mut ConnectionProfile); 6] = [
            |p: &mut ConnectionProfile| p.host = "evil".into(),
            |p: &mut ConnectionProfile| p.port = 1,
            |p: &mut ConnectionProfile| p.user = "root".into(),
            |p: &mut ConnectionProfile| p.engine = "mysql".into(),
            |p: &mut ConnectionProfile| p.url = Some("mongodb://evil".into()),
            |p: &mut ConnectionProfile| p.file = Some("x.db".into()),
        ];
        for change in changes {
            let mut moved = saved.clone();
            change(&mut moved);
            assert!(!saved.same_target(&moved), "{moved:?}");
        }
    }

    #[test]
    fn a_v1_file_is_backed_up_once() {
        let directory = scratch("v1bak");
        let path = directory.join(CONFIG_FILE);
        fs::write(&path, V1_FIXTURE).unwrap();
        let config = load_file(&path, true).unwrap();
        assert!(config.startup_notice.is_none());
        let backup = directory.join("fosta-studio.json.v1.bak");
        assert_eq!(fs::read_to_string(&backup).unwrap(), V1_FIXTURE);
        write_config(&path, &config).unwrap();
        fs::write(&backup, "older backup").unwrap();
        fs::write(&path, V1_FIXTURE).unwrap();
        load_file(&path, true).unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), "older backup", "an existing backup is kept");
        fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();
        fs::remove_file(&backup).unwrap();
        load_file(&path, true).unwrap();
        assert!(!backup.exists(), "version-2 files are not backed up");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn config_writes_are_atomic_and_leave_out_the_notice() {
        let directory = scratch("atomic");
        let path = directory.join(CONFIG_FILE);
        fs::write(&path, "old").unwrap();
        let mut config = AppConfig::starter(false);
        config.startup_notice = Some("hello".into());
        write_config(&path, &config).unwrap();
        assert!(!directory.join("fosta-studio.json.tmp").exists());
        let written: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written.get("startupNotice").is_none());
        assert_eq!(written["configVersion"], 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_corrupt_file_is_set_aside_and_replaced_by_the_starter() {
        let directory = scratch("corrupt");
        let path = directory.join(CONFIG_FILE);
        fs::write(&path, "{ not json").unwrap();
        let config = load_file(&path, true).unwrap();
        let notice = config.startup_notice.as_deref().unwrap();
        assert!(notice.contains("fosta-studio.json.corrupt-"), "{notice}");
        assert_eq!(config.workspaces[0].connections[0].id, "local-mariadb");
        let aside: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("fosta-studio.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(fs::read_to_string(directory.join(&aside[0])).unwrap(), "{ not json");
        let reloaded = load_file(&path, true).unwrap();
        assert!(reloaded.startup_notice.is_none(), "the notice is not persisted");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn containers_start_without_a_localhost_connection() {
        let desktop = AppConfig::starter(true);
        assert_eq!(desktop.workspaces[0].connections[0].host, "127.0.0.1");
        let container = AppConfig::starter(false);
        assert!(container.workspaces.iter().all(|workspace| workspace.connections.is_empty()));
    }

    #[test]
    fn legacy_files_are_moved_once_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("fosta-rename-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (new, old) = (dir.join("fosta-studio.json"), dir.join("db-studio.json"));
        fs::write(&old, "old").unwrap();
        move_legacy_file(&new, &old).unwrap();
        assert_eq!(fs::read_to_string(&new).unwrap(), "old");
        assert!(!old.exists());
        fs::write(&old, "stale").unwrap();
        move_legacy_file(&new, &old).unwrap();
        assert_eq!(fs::read_to_string(&new).unwrap(), "old", "an existing new file wins");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn migrates_legacy_ai_setting_into_an_active_connection() {
        let config: AppConfig =
            serde_json::from_str(r#"{ "ai": { "provider": "anthropic", "baseUrl": "", "model": "claude-x" } }"#).unwrap();
        assert_eq!(config.ai_connections.len(), 1);
        let connection = &config.ai_connections[0];
        assert_eq!(
            (connection.id.as_str(), connection.name.as_str(), connection.default_model.as_str()),
            ("default", "Claude", "claude-x")
        );
        let active = config.ai_active.as_ref().unwrap();
        assert_eq!((active.connection_id.as_str(), active.model.as_str()), ("default", "claude-x"));
        let saved = serde_json::to_value(&config).unwrap();
        assert!(saved.get("ai").is_none());
        assert_eq!(saved["aiActive"]["connectionId"], "default");
    }

    #[test]
    fn workspaces_get_safety_defaults() {
        use crate::gateway::rules::Preset;
        let mut config: AppConfig = serde_json::from_str(
            r#"{ "workspaces": [ { "id": "a", "name": "Dev", "icon": "laptop" }, { "id": "p", "name": "Prod", "icon": "shield" } ] }"#,
        )
        .unwrap();
        assert_eq!(
            config.workspaces[0].safety().preset,
            Preset::Open,
            "existing workspaces keep working as before"
        );
        assert_eq!(
            config.workspaces[1].safety().preset,
            Preset::ReadOnly,
            "Production defaults to Read-only"
        );
        assert_eq!(
            serde_json::to_value(&config).unwrap()["workspaces"][1]["safety"]["preset"],
            "readOnly"
        );
        let meta = |id: &str, icon: &str| WorkspaceMeta {
            id: id.into(),
            name: id.into(),
            color: String::new(),
            icon: icon.into(),
            safety: None,
            assistant_queries: None,
        };
        config.put_workspace(meta("n", "cloud"));
        assert_eq!(
            config.workspace("n").unwrap().safety().preset,
            Preset::Guarded,
            "new workspaces start Guarded"
        );
        config.put_workspace(meta("p", "shield"));
        assert_eq!(
            config.workspace("p").unwrap().safety().preset,
            Preset::ReadOnly,
            "saving without safety keeps it"
        );
        let (workspace, _) = config.connection_scope("missing").map_or((None, None), |(w, c)| (Some(w), Some(c)));
        assert!(workspace.is_none());
    }

    #[test]
    fn query_history_is_on_unless_turned_off() {
        assert!(AppConfig::default().query_history);
        let off: AppConfig = serde_json::from_str(r#"{ "queryHistory": false }"#).unwrap();
        assert!(!off.query_history);
        assert_eq!(serde_json::to_value(&off).unwrap()["queryHistory"], false);
    }

    #[test]
    fn removing_the_active_connection_activates_the_next() {
        let mut config = AppConfig::default();
        assert!(config.ai_active.is_none());
        for id in ["a", "b"] {
            config.put_ai_connection(AiConnection {
                id: id.into(),
                name: id.into(),
                provider: Default::default(),
                base_url: String::new(),
                default_model: format!("{id}-model"),
            });
        }
        assert_eq!(config.ai_active.as_ref().unwrap().connection_id, "a");
        config.set_ai_active("a", "other").unwrap();
        assert!(config.set_ai_active("missing", "x").is_err());
        config.remove_ai_connection("a");
        let active = config.ai_active.as_ref().unwrap();
        assert_eq!((active.connection_id.as_str(), active.model.as_str()), ("b", "b-model"));
        config.remove_ai_connection("b");
        assert!(config.ai_active.is_none());
    }

    #[test]
    fn editing_the_active_connection_follows_its_new_default() {
        let mut config = AppConfig::default();
        let connection = AiConnection {
            id: "a".into(),
            name: "A".into(),
            provider: Default::default(),
            base_url: String::new(),
            default_model: "m1".into(),
        };
        config.put_ai_connection(connection.clone());
        config.put_ai_connection(AiConnection {
            default_model: "m2".into(),
            ..connection.clone()
        });
        assert_eq!(config.ai_active.as_ref().unwrap().model, "m2");
        config.set_ai_active("a", "picked").unwrap();
        config.put_ai_connection(AiConnection {
            name: "Renamed".into(),
            default_model: "m2".into(),
            ..connection.clone()
        });
        assert_eq!(config.ai_active.as_ref().unwrap().model, "picked");
        config.put_ai_connection(AiConnection {
            provider: crate::ai::AiProvider::Openai,
            default_model: "gpt".into(),
            ..connection
        });
        assert_eq!(config.ai_active.as_ref().unwrap().model, "gpt");
    }
}
