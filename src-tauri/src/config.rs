use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};

use crate::{
    ai::AiSettings,
    embedding::EmbeddingSettings,
    error::{AppError, AppResult},
    lock::lock,
    tasks::TaskDef,
};

const CONFIG_FILE: &str = "db-studio.json";
const DEFAULT_WORKSPACE_ID: &str = "default";
const DEFAULT_WORKSPACE_NAME: &str = "Local";
const DEFAULT_WORKSPACE_COLOR: &str = "blue";
const DEFAULT_WORKSPACE_ICON: &str = "laptop";

/// Database engines a profile can point at. Only the MySQL-protocol ones have
/// a driver today; the others can be saved and are refused at connect time.
pub const SUPPORTED_DRIVERS: [&str; 2] = ["mariadb", "mysql"];
const DEFAULT_DRIVER: &str = "mariadb";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    #[serde(default = "default_driver")]
    pub driver: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub save_password: bool,
    /// Driver-specific settings (TLS flags, a MongoDB URI, …), kept opaque here.
    #[serde(default)]
    pub options: HashMap<String, String>,
}

impl ConnectionProfile {
    pub fn driver_label(&self) -> &str {
        match self.driver.as_str() {
            "mysql" => "MySQL",
            "mssql" => "SQL Server",
            "mongodb" => "MongoDB",
            "postgres" => "PostgreSQL",
            _ => "MariaDB",
        }
    }

    pub fn ensure_supported(&self) -> AppResult<()> {
        if SUPPORTED_DRIVERS.contains(&self.driver.as_str()) {
            return Ok(());
        }
        Err(AppError::new(format!(
            "{} connections are not available yet. Only MariaDB and MySQL can connect in this build.",
            self.driver_label()
        )))
    }
}

fn default_driver() -> String {
    DEFAULT_DRIVER.into()
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
    #[serde(default)]
    pub connections: Vec<ConnectionProfile>,
    #[serde(default)]
    pub tasks: Vec<TaskDef>,
}

impl Workspace {
    fn new(meta: WorkspaceMeta) -> Self {
        Self {
            id: meta.id,
            name: meta.name,
            color: meta.color,
            icon: meta.icon,
            connections: Vec::new(),
            tasks: Vec::new(),
        }
    }

    fn apply(&mut self, meta: WorkspaceMeta) {
        self.name = meta.name;
        self.color = meta.color;
        self.icon = meta.icon;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", from = "StoredConfig")]
pub struct AppConfig {
    pub workspaces: Vec<Workspace>,
    pub active_workspace: String,
    pub embedding: Option<EmbeddingSettings>,
    pub ai: Option<AiSettings>,
}

/// On-disk shape. Accepts both the current layout (`workspaces`) and the
/// legacy flat layout (`connections` + `tasks` at the top level), which is
/// migrated into a single workspace on load.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredConfig {
    #[serde(default)]
    workspaces: Vec<Workspace>,
    #[serde(default)]
    active_workspace: Option<String>,
    #[serde(default)]
    embedding: Option<EmbeddingSettings>,
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
                connections: stored.connections,
                tasks: stored.tasks,
            }),
        }
        let active_workspace = stored
            .active_workspace
            .filter(|id| workspaces.iter().any(|workspace| &workspace.id == id))
            .unwrap_or_else(|| workspaces[0].id.clone());
        Self {
            workspaces,
            active_workspace,
            embedding: stored.embedding,
            ai: stored.ai,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        StoredConfig::default().into()
    }
}

impl AppConfig {
    fn starter() -> Self {
        let mut config = Self::default();
        config.workspaces[0].connections.push(ConnectionProfile {
            id: "local-mariadb".into(),
            name: "Local MariaDB".into(),
            driver: DEFAULT_DRIVER.into(),
            host: "127.0.0.1".into(),
            port: 3306,
            user: "root".into(),
            database: None,
            save_password: false,
            options: HashMap::new(),
        });
        config
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
    pub fn put_connection(&mut self, workspace_id: &str, profile: ConnectionProfile) -> AppResult<()> {
        self.workspace(workspace_id)?;
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
        let current = if path.exists() {
            serde_json::from_str(&fs::read_to_string(&path)?)?
        } else {
            let starter = AppConfig::starter();
            write_config(&path, &starter)?;
            starter
        };
        Ok(Self {
            path,
            current: Mutex::new(current),
        })
    }

    pub fn snapshot(&self) -> AppConfig {
        self.current().clone()
    }

    pub fn connection(&self, id: &str) -> AppResult<ConnectionProfile> {
        self.current()
            .connection(id)
            .cloned()
            .ok_or_else(|| AppError::new(format!("Connection '{id}' no longer exists")))
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
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| AppError::new("Cannot resolve the application directory"))?;
    Ok(directory.join(CONFIG_FILE))
}

fn write_config(path: &Path, config: &AppConfig) -> AppResult<()> {
    fs::write(path, serde_json::to_string_pretty(config)?)?;
    Ok(())
}
