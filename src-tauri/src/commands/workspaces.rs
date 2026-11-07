//! Workspaces: named sets of connections and startup tasks.

use crate::{
    config::{AppConfig, WorkspaceMeta},
    error::{AppError, AppResult},
    secrets, AppState,
};

pub fn save_workspace(state: &AppState, workspace: WorkspaceMeta) -> AppResult<AppConfig> {
    if workspace.name.trim().is_empty() {
        return Err(AppError::new("Workspace name is required"));
    }
    state.config.update(|config| {
        config.put_workspace(workspace);
        Ok(())
    })
}

/// Switches the active workspace and starts its auto-start tasks. Tasks of
/// other workspaces keep running so a port-forward in use is not cut off.
pub fn set_active_workspace(state: &AppState, id: String) -> AppResult<AppConfig> {
    let config = state.config.update(|config| config.set_active(&id))?;
    state.tasks.start_auto(&config.active().tasks);
    Ok(config)
}

pub async fn delete_workspace(state: &AppState, id: String) -> AppResult<AppConfig> {
    let doomed = state.config.snapshot().workspace(&id)?.clone();
    for task in &doomed.tasks {
        state.tasks.stop(&task.id);
    }
    for profile in &doomed.connections {
        state.db.disconnect(&profile.id).await;
        secrets::forget_password(&profile.id)?;
    }
    state.config.update(|config| config.remove_workspace(&id).map(|_| ()))
}
