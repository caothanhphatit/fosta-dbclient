use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};

use crate::{
    ai::{AiActive, AiConnection, AiSettings},
    contract::{
        spec::{SshAuth, SshHop},
        AuthMethodId, ConnectionSpec, SshConfig, TlsConfig, TlsMode,
    },
    embedding::EmbeddingSettings,
    error::{AppError, AppResult},
    gateway::{assistant::AssistantQueries, rules::SafetySettings},
    lock::lock,
    mcp::{McpAccess, McpSettings},
    saved_queries::{self, SavedQuery, MAX_SAVED_QUERIES},
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
    /// The id of a command of the same workspace that must run while this connection is connected (e.g. `kubectl
    /// port-forward`; see `crate::connection_command`).
    #[serde(rename = "commandId", default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
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
        self.command = blank(&self.command);
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

    /// Whether `other` points at the same server, through the same SSH route, command and TLS checks, as this saved
    /// profile, so this profile's saved secrets may be sent there. Name, database, options and safety rules do not
    /// matter; any change to the SSH or TLS settings does (a new bastion or a weaker certificate check could harvest
    /// the saved passwords), and so does another command (a port-forward to `svc/b` keeps 127.0.0.1:13306).
    pub fn same_target(&self, other: &ConnectionProfile) -> bool {
        let (mut saved, mut draft) = (self.clone(), other.clone());
        saved.normalize();
        draft.normalize();
        saved.engine == draft.engine
            && saved.host.trim() == draft.host.trim()
            && saved.port == draft.port
            && saved.user.trim() == draft.user.trim()
            && blank(&saved.url) == blank(&draft.url)
            && blank(&saved.file) == blank(&draft.file)
            && ssh_route(saved.ssh.as_ref()) == ssh_route(draft.ssh.as_ref())
            && tls_checks(saved.tls.as_ref()) == tls_checks(draft.tls.as_ref())
            && saved.command == draft.command
    }
}

fn forget_command(connections: &mut [ConnectionProfile], id: &str) {
    for profile in connections.iter_mut().filter(|profile| profile.command.as_deref() == Some(id)) {
        profile.command = None;
    }
}

/// A trimmed, non-empty string, else `None`.
fn blank(value: &Option<String>) -> Option<String> {
    value.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned)
}

/// The parts of an SSH tunnel that decide where the secrets go; `None` when no tunnel is used (absent or disabled).
fn ssh_route(ssh: Option<&SshConfig>) -> Option<SshConfig> {
    let ssh = ssh.filter(|ssh| ssh.enabled)?;
    let auth = match &ssh.auth {
        SshAuth::PrivateKey { key_file } => SshAuth::PrivateKey {
            key_file: key_file.trim().to_owned(),
        },
        auth => auth.clone(),
    };
    Some(SshConfig {
        enabled: true,
        host: ssh.host.trim().to_owned(),
        port: ssh.port,
        user: ssh.user.trim().to_owned(),
        auth,
        jump_hosts: ssh
            .jump_hosts
            .iter()
            .map(|hop| SshHop {
                host: hop.host.trim().to_owned(),
                port: hop.port,
                user: hop.user.trim().to_owned(),
            })
            .collect(),
        host_key: ssh.host_key,
        // Keepalive does not change where the secrets go.
        keepalive_secs: None,
    })
}

/// The TLS settings with blank paths as unset; no settings compare as the default ("prefer", nothing else).
fn tls_checks(tls: Option<&TlsConfig>) -> TlsConfig {
    let tls = tls.cloned().unwrap_or_default();
    TlsConfig {
        mode: tls.mode,
        ca_file: blank(&tls.ca_file),
        client_cert_file: blank(&tls.client_cert_file),
        client_key_file: blank(&tls.client_key_file),
        server_name: blank(&tls.server_name),
        accept_invalid_certs: tls.accept_invalid_certs,
        pinned_sha256: blank(&tls.pinned_sha256),
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
    /// What MCP clients may do here; `None` keeps the current choice (Off for a new workspace).
    #[serde(default)]
    pub mcp: Option<McpAccess>,
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
    /// What MCP clients may do with this workspace's connections (off unless chosen; see `crate::mcp`).
    #[serde(default)]
    pub mcp: McpAccess,
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
    #[serde(default)]
    pub tasks: Vec<TaskDef>,
    /// Named query-bar states of the engine workspaces (`crate::saved_queries`); left out of the file while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saved_queries: Vec<SavedQuery>,
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
            mcp: meta.mcp.unwrap_or_default(),
            connections: Vec::new(),
            tasks: Vec::new(),
            saved_queries: Vec::new(),
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
        if let Some(choice) = meta.mcp {
            self.mcp = choice;
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
    /// Settings → "Check for updates", plus what the last check found (`update.rs`).
    pub update_check: crate::update::UpdateCheckSettings,
    /// Settings → MCP server (off unless turned on).
    pub mcp: McpSettings,
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
    #[serde(default)]
    update_check: crate::update::UpdateCheckSettings,
    #[serde(default)]
    mcp: McpSettings,
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
                mcp: McpAccess::Off,
                connections: stored.connections,
                tasks: stored.tasks,
                saved_queries: Vec::new(),
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
            update_check: stored.update_check,
            mcp: stored.mcp,
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
    /// Its command must be one of that workspace's; moving to another workspace drops the old workspace's command.
    pub fn put_connection(&mut self, workspace_id: &str, mut profile: ConnectionProfile) -> AppResult<()> {
        let target = self.workspace(workspace_id)?;
        profile.normalize();
        if let Some(command) = profile.command.clone() {
            if !target.tasks.iter().any(|task| task.id == command) {
                let left_behind = self
                    .connection_scope(&profile.id)
                    .is_some_and(|(source, _)| source.id != workspace_id && source.tasks.iter().any(|task| task.id == command));
                if !left_behind {
                    return Err(AppError::new(format!(
                        "The command of connection '{}' is not a command of workspace '{}'",
                        profile.name, target.name
                    )));
                }
                profile.command = None;
            }
        }
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
            if workspace.tasks.iter().any(|item| item.id == task.id) {
                // A command that moves away leaves the connections of its old workspace.
                workspace.tasks.retain(|item| item.id != task.id);
                forget_command(&mut workspace.connections, &task.id);
            }
        }
        let target = self.workspace_mut(workspace_id)?;
        upsert(&mut target.tasks, task, |item| item.id.as_str());
        Ok(())
    }

    /// Removes the command `id`; the connections that used it no longer use a command.
    pub fn remove_task(&mut self, id: &str) {
        for workspace in &mut self.workspaces {
            workspace.tasks.retain(|item| item.id != id);
            forget_command(&mut workspace.connections, id);
        }
    }

    /// The workspace saved queries are kept in: the one holding `connection_id` (when it is saved), else
    /// `workspace_id`, else the active one.
    pub fn saved_query_scope(&self, connection_id: Option<&str>, workspace_id: Option<&str>) -> AppResult<&Workspace> {
        if let Some((workspace, _)) = connection_id.and_then(|id| self.connection_scope(id)) {
            return Ok(workspace);
        }
        match workspace_id {
            Some(id) => self.workspace(id),
            None => Ok(self.active()),
        }
    }

    /// Adds or replaces the saved query `query.id` in `workspace_id` (moving it there from another workspace).
    pub fn put_saved_query(&mut self, workspace_id: &str, query: SavedQuery) -> AppResult<()> {
        let query = query.validated()?;
        let target = self.workspace(workspace_id)?;
        if target.saved_queries.len() >= MAX_SAVED_QUERIES && !target.saved_queries.iter().any(|item| item.id == query.id) {
            return Err(AppError::new(format!(
                "Workspace '{}' already keeps {MAX_SAVED_QUERIES} saved queries; delete some first",
                target.name
            )));
        }
        for workspace in self.workspaces.iter_mut().filter(|workspace| workspace.id != workspace_id) {
            workspace.saved_queries.retain(|item| item.id != query.id);
        }
        let target = self.workspace_mut(workspace_id)?;
        upsert(&mut target.saved_queries, query, |item| item.id.as_str());
        // The whole workspace stays bounded too (a failed check leaves the config file as it was).
        saved_queries::check_workspace_size(&target.saved_queries)
    }

    /// Renames the saved query `id`; returns the id of its workspace.
    pub fn rename_saved_query(&mut self, id: &str, name: &str, updated_at: i64) -> AppResult<String> {
        let name = saved_queries::valid_name(name)?;
        for workspace in &mut self.workspaces {
            if let Some(query) = workspace.saved_queries.iter_mut().find(|item| item.id == id) {
                query.name = name;
                query.updated_at = updated_at;
                return Ok(workspace.id.clone());
            }
        }
        Err(AppError::new("The saved query no longer exists"))
    }

    /// Deletes the saved query `id`; returns the id of the workspace that held it.
    pub fn remove_saved_query(&mut self, id: &str) -> Option<String> {
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.saved_queries.iter().any(|item| item.id == id))?;
        workspace.saved_queries.retain(|item| item.id != id);
        Some(workspace.id.clone())
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
    pub fn load() -> AppResult<Self> {
        let path = config_path()?;
        let current = load_file(&path)?;
        Ok(Self {
            path,
            current: Mutex::new(current),
        })
    }

    /// A store that writes `config` to `path` (tests).
    #[cfg(test)]
    pub(crate) fn at(path: PathBuf, config: AppConfig) -> Self {
        Self {
            path,
            current: Mutex::new(config),
        }
    }

    /// The file this store writes (tests).
    #[cfg(test)]
    pub(crate) fn path_for_tests(&self) -> &Path {
        &self.path
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
            .ok_or_else(|| AppError::new(format!("Command '{id}' no longer exists")))
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

/// The saved settings, read without creating, migrating or repairing the file (the MCP stdio bridge runs beside the app).
pub fn read_saved() -> AppResult<AppConfig> {
    let path = data_path(CONFIG_FILE)?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let stored: StoredConfig = serde_json::from_slice(&fs::read(path)?)?;
    Ok(stored.into())
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

/// Reads the config at `path`: a missing file becomes the default config (written), a version-1 file is migrated
/// (after a one-time `.v1.bak` copy), and an unreadable one is moved aside to `.corrupt-<unix ts>` and replaced by
/// the default config, with a `startup_notice` for the UI.
fn load_file(path: &Path) -> AppResult<AppConfig> {
    if !path.exists() {
        let config = AppConfig::default();
        write_config(path, &config)?;
        return Ok(config);
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
            let mut config = AppConfig::default();
            write_config(path, &config)?;
            config.startup_notice = Some(format!(
                "The settings file could not be read ({error}). It was moved to {} and Fosta Studio started with new settings.",
                aside.display()
            ));
            Ok(config)
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
    let mut file = crate::data_files::private_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
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

    fn saved_query(id: &str) -> SavedQuery {
        SavedQuery {
            id: id.into(),
            name: format!("Query {id}"),
            engine: "mongodb".into(),
            connection_id: Some("g1".into()),
            database: "app".into(),
            collection: "orders".into(),
            query: crate::saved_queries::SavedQueryState {
                filter: "{ status: 'paid', at: ISODate('2026-01-01') }".into(),
                project: "{ total: 1 }".into(),
                sort: "{ total: -1 }".into(),
                collation: "{ locale: 'en' }".into(),
                skip: 5,
                limit: 50,
            },
            pipeline: Some(vec!["{ $match: { status: 'paid' } }".into(), "{ $limit: 5 }".into()]),
            updated_at: 1,
        }
    }

    #[test]
    fn files_without_saved_queries_load_and_leave_them_out() {
        let config = migrate(V1_FIXTURE);
        assert!(config.workspaces[0].saved_queries.is_empty());
        let written = serde_json::to_value(&config).unwrap();
        assert!(written["workspaces"][0].get("savedQueries").is_none(), "no empty list in the file");
    }

    #[test]
    fn saved_queries_round_trip_through_the_file() {
        let directory = scratch("saved-queries");
        let path = directory.join(CONFIG_FILE);
        let mut config = migrate(V1_FIXTURE);
        config.put_saved_query("default", saved_query("a")).unwrap();
        config.put_saved_query("default", saved_query("b")).unwrap();
        let mut replaced = saved_query("a");
        replaced.name = "Paid orders".into();
        config.put_saved_query("default", replaced).unwrap();
        write_config(&path, &config).unwrap();
        let reloaded = load_file(&path).unwrap();
        let saved = &reloaded.workspaces[0].saved_queries;
        assert_eq!(saved.len(), 2, "same id replaces");
        assert_eq!(saved[0].name, "Paid orders");
        assert_eq!(saved[1], saved_query("b"));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"savedQueries\""));
        assert!(text.contains("\"pipeline\""));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn saved_query_scope_rename_and_remove() {
        let mut config = migrate(V1_FIXTURE);
        config.put_workspace(WorkspaceMeta {
            id: "w2".into(),
            name: "Two".into(),
            color: String::new(),
            icon: String::new(),
            safety: None,
            assistant_queries: None,
            mcp: None,
        });
        assert_eq!(
            config.saved_query_scope(Some("g1"), Some("w2")).unwrap().id,
            "default",
            "the connection's workspace wins"
        );
        assert_eq!(config.saved_query_scope(Some("gone"), Some("w2")).unwrap().id, "w2");
        assert_eq!(config.saved_query_scope(None, None).unwrap().id, "default");
        assert!(config.saved_query_scope(None, Some("missing")).is_err());

        config.put_saved_query("default", saved_query("a")).unwrap();
        config.put_saved_query("w2", saved_query("a")).unwrap();
        assert!(config.workspace("default").unwrap().saved_queries.is_empty(), "moved, not copied");
        assert_eq!(config.rename_saved_query("a", " New ", 7).unwrap(), "w2");
        let renamed = &config.workspace("w2").unwrap().saved_queries[0];
        assert_eq!((renamed.name.as_str(), renamed.updated_at), ("New", 7));
        assert!(config.rename_saved_query("a", "", 8).is_err());
        assert_eq!(config.remove_saved_query("a").as_deref(), Some("w2"));
        assert_eq!(config.remove_saved_query("a"), None);
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

    fn tunnelled() -> ConnectionProfile {
        ConnectionProfile {
            id: "t".into(),
            engine: "postgres".into(),
            host: "db.internal".into(),
            port: 5432,
            user: "app".into(),
            ssh: Some(SshConfig {
                enabled: true,
                host: "bastion".into(),
                port: 22,
                user: "me".into(),
                auth: SshAuth::PrivateKey {
                    key_file: "~/.ssh/id_ed25519".into(),
                },
                jump_hosts: vec![SshHop {
                    host: "jump".into(),
                    port: 22,
                    user: "hop".into(),
                }],
                host_key: Default::default(),
                keepalive_secs: None,
            }),
            tls: Some(TlsConfig {
                mode: TlsMode::VerifyFull,
                ca_file: Some("ca.pem".into()),
                server_name: Some("db.example.com".into()),
                pinned_sha256: Some("ab:cd".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn saved_secrets_are_bound_to_the_ssh_route_and_tls_checks() {
        let saved = tunnelled();
        let mut same = saved.clone();
        same.name = "renamed".into();
        same.ssh.as_mut().unwrap().keepalive_secs = Some(30);
        same.ssh.as_mut().unwrap().host = " bastion ".into();
        same.tls.as_mut().unwrap().client_cert_file = Some(String::new());
        assert!(saved.same_target(&same), "keepalive, blanks and whitespace do not matter");
        type Change = fn(&mut ConnectionProfile);
        let changes: [Change; 15] = [
            |p| p.ssh.as_mut().unwrap().enabled = false,
            |p| p.ssh = None,
            |p| p.ssh.as_mut().unwrap().host = "evil".into(),
            |p| p.ssh.as_mut().unwrap().port = 2222,
            |p| p.ssh.as_mut().unwrap().user = "root".into(),
            |p| p.ssh.as_mut().unwrap().auth = SshAuth::Password,
            |p| p.ssh.as_mut().unwrap().auth = SshAuth::PrivateKey { key_file: "other".into() },
            |p| p.ssh.as_mut().unwrap().jump_hosts[0].host = "evil".into(),
            |p| p.ssh.as_mut().unwrap().jump_hosts[0].port = 2,
            |p| p.ssh.as_mut().unwrap().jump_hosts[0].user = "x".into(),
            |p| p.ssh.as_mut().unwrap().host_key = crate::contract::spec::HostKeyPolicy::Off,
            |p| p.tls.as_mut().unwrap().mode = TlsMode::Disable,
            |p| p.tls.as_mut().unwrap().accept_invalid_certs = true,
            |p| p.tls.as_mut().unwrap().ca_file = None,
            |p| p.tls.as_mut().unwrap().pinned_sha256 = Some("ef:01".into()),
        ];
        for change in changes {
            let mut moved = saved.clone();
            change(&mut moved);
            assert!(!saved.same_target(&moved), "{moved:?}");
        }
        let mut renamed_sni = saved.clone();
        renamed_sni.tls.as_mut().unwrap().server_name = None;
        assert!(!saved.same_target(&renamed_sni));
        let mut no_tls = saved.clone();
        no_tls.tls = None;
        assert!(!saved.same_target(&no_tls));
    }

    #[test]
    fn absent_and_default_ssh_and_tls_compare_equal() {
        // A version-1 profile (no ssh, no tls) re-sent by the dialog with a disabled tunnel and "prefer" TLS.
        let saved = migrate(V1_FIXTURE).connection("g1").unwrap().clone();
        let mut draft = saved.clone();
        draft.ssh = Some(SshConfig {
            enabled: false,
            host: "ignored".into(),
            port: 22,
            user: String::new(),
            auth: SshAuth::Agent,
            jump_hosts: Vec::new(),
            host_key: Default::default(),
            keepalive_secs: None,
        });
        draft.tls = Some(TlsConfig::default());
        assert!(saved.same_target(&draft));
        draft.tls = None;
        draft.ssh = None;
        assert!(saved.same_target(&draft));
    }

    #[test]
    fn a_v1_file_is_backed_up_once() {
        let directory = scratch("v1bak");
        let path = directory.join(CONFIG_FILE);
        fs::write(&path, V1_FIXTURE).unwrap();
        let config = load_file(&path).unwrap();
        assert!(config.startup_notice.is_none());
        let backup = directory.join("fosta-studio.json.v1.bak");
        assert_eq!(fs::read_to_string(&backup).unwrap(), V1_FIXTURE);
        write_config(&path, &config).unwrap();
        fs::write(&backup, "older backup").unwrap();
        fs::write(&path, V1_FIXTURE).unwrap();
        load_file(&path).unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), "older backup", "an existing backup is kept");
        fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();
        fs::remove_file(&backup).unwrap();
        load_file(&path).unwrap();
        assert!(!backup.exists(), "version-2 files are not backed up");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn config_writes_are_atomic_and_leave_out_the_notice() {
        let directory = scratch("atomic");
        let path = directory.join(CONFIG_FILE);
        fs::write(&path, "old").unwrap();
        let config = AppConfig {
            startup_notice: Some("hello".into()),
            ..Default::default()
        };
        write_config(&path, &config).unwrap();
        assert!(!directory.join("fosta-studio.json.tmp").exists());
        let written: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written.get("startupNotice").is_none());
        assert_eq!(written["configVersion"], 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_corrupt_file_is_set_aside_and_replaced_by_the_default() {
        let directory = scratch("corrupt");
        let path = directory.join(CONFIG_FILE);
        fs::write(&path, "{ not json").unwrap();
        let config = load_file(&path).unwrap();
        let notice = config.startup_notice.as_deref().unwrap();
        assert!(notice.contains("fosta-studio.json.corrupt-"), "{notice}");
        assert!(config.workspaces[0].connections.is_empty());
        let aside: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("fosta-studio.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(fs::read_to_string(directory.join(&aside[0])).unwrap(), "{ not json");
        let reloaded = load_file(&path).unwrap();
        assert!(reloaded.startup_notice.is_none(), "the notice is not persisted");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_first_start_seeds_no_connection() {
        let directory = scratch("first-start");
        let path = directory.join(CONFIG_FILE);
        let config = load_file(&path).unwrap();
        assert_eq!(config.workspaces.len(), 1);
        assert!(config.workspaces[0].connections.is_empty());
        assert!(path.exists(), "the default config is written");
        fs::remove_dir_all(directory).unwrap();
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
            mcp: None,
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

    fn port_forward(id: &str) -> TaskDef {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": "MariaDB port-forward",
            "program": "kubectl",
            "args": ["port-forward", "-n", "shop", "svc/mariadb", "13306:3306"],
            "readyPattern": "Forwarding from",
        }))
        .unwrap()
    }

    /// A config with workspaces "a" (command "pf-a") and "b" (command "pf-b").
    fn two_workspaces() -> AppConfig {
        let mut config = AppConfig::default();
        for id in ["a", "b"] {
            config.put_workspace(WorkspaceMeta {
                id: id.into(),
                name: id.to_uppercase(),
                color: String::new(),
                icon: "box".into(),
                safety: None,
                assistant_queries: None,
                mcp: None,
            });
            config.put_task(id, port_forward(&format!("pf-{id}"))).unwrap();
        }
        config
    }

    fn using(id: &str, command: Option<&str>) -> ConnectionProfile {
        ConnectionProfile {
            id: id.into(),
            name: id.into(),
            host: "127.0.0.1".into(),
            port: 13306,
            command: command.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn a_command_reference_round_trips_as_command_id() {
        let json = serde_json::to_value(using("k", Some("pf-a"))).unwrap();
        assert_eq!(json["commandId"], "pf-a");
        let parsed: ConnectionProfile = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.command.as_deref(), Some("pf-a"));
        // Absent stays absent, and an older file without it still loads.
        assert!(serde_json::to_value(ConnectionProfile::default())
            .unwrap()
            .get("commandId")
            .is_none());
        let old: ConnectionProfile = serde_json::from_str(r#"{"id":"o","name":"Old","driver":"mysql"}"#).unwrap();
        assert!(old.command.is_none());
        // The 0.1.0 task shape loads with the default timeout.
        assert_eq!(port_forward("x").ready_timeout_secs, 30);
    }

    #[test]
    fn a_connection_uses_only_a_command_of_its_workspace() {
        let mut config = two_workspaces();
        config.put_connection("a", using("k", Some(" pf-a "))).unwrap();
        assert_eq!(config.connection("k").unwrap().command.as_deref(), Some("pf-a"));
        let error = config.put_connection("a", using("k", Some("pf-b"))).unwrap_err();
        assert!(error.to_string().contains("not a command of workspace 'A'"), "{error}");
        let error = config.put_connection("a", using("new", Some("missing"))).unwrap_err();
        assert!(error.to_string().contains("not a command"), "{error}");
        config.put_connection("a", using("blank", Some("  "))).unwrap();
        assert_eq!(config.connection("blank").unwrap().command, None);

        // Moving to workspace b leaves workspace a's command behind; b's own may be picked in the same save.
        config.put_connection("b", using("k", Some("pf-a"))).unwrap();
        assert_eq!(config.connection("k").unwrap().command, None);
        config.put_connection("a", using("k2", Some("pf-a"))).unwrap();
        config.put_connection("b", using("k2", Some("pf-b"))).unwrap();
        assert_eq!(config.connection("k2").unwrap().command.as_deref(), Some("pf-b"));
    }

    #[test]
    fn deleting_or_moving_a_command_clears_it_from_its_connections() {
        let mut config = two_workspaces();
        config.put_connection("a", using("one", Some("pf-a"))).unwrap();
        config.put_connection("a", using("two", Some("pf-a"))).unwrap();
        config.put_connection("b", using("three", Some("pf-b"))).unwrap();
        config.remove_task("pf-a");
        assert_eq!(config.connection("one").unwrap().command, None);
        assert_eq!(config.connection("two").unwrap().command, None);
        assert_eq!(config.connection("three").unwrap().command.as_deref(), Some("pf-b"));
        config.put_task("a", port_forward("pf-b")).unwrap();
        assert_eq!(config.connection("three").unwrap().command, None);
        config.put_task("a", port_forward("pf-b")).unwrap();
        assert_eq!(config.workspace("a").unwrap().tasks.len(), 1);
    }

    #[test]
    fn another_command_is_another_target() {
        let saved = using("k", Some("pf-a"));
        let mut renamed = saved.clone();
        renamed.name = "renamed".into();
        assert!(saved.same_target(&renamed));
        assert!(
            !saved.same_target(&using("k", Some("pf-b"))),
            "another command may reach another server"
        );
        assert!(!saved.same_target(&using("k", None)));
        assert!(using("k", None).same_target(&using("k", Some(" "))));
    }
}
