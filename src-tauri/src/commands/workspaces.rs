//! Workspaces: named sets of connections and startup tasks, each with its safety rules.

use tauri::State;

use crate::{
    audit::{AuditEntry, Outcome},
    config::{AppConfig, WorkspaceMeta},
    error::{AppError, AppResult},
    gateway::rules::{relaxations, SafetySettings, PRODUCTION_ICON},
    secrets, AppState,
};

pub const RELAX_CONFIRMATION_REQUIRED: &str = "RELAX_CONFIRMATION_REQUIRED";

/// Creates or edits a workspace. Making its safety rules less strict is recorded in the audit
/// log; for a Production workspace it also needs `relax_confirmation` = the workspace name.
#[tauri::command]
pub fn save_workspace(state: State<'_, AppState>, workspace: WorkspaceMeta, relax_confirmation: Option<String>) -> AppResult<AppConfig> {
    if workspace.name.trim().is_empty() {
        return Err(AppError::new("Workspace name is required"));
    }
    let previous = state.config.read(|config| config.workspace(&workspace.id).ok().cloned());
    let (before, name, was_production) = match &previous {
        Some(previous) => (previous.safety(), previous.name.clone(), previous.icon == PRODUCTION_ICON),
        None => (SafetySettings::for_new_workspace(&workspace.icon), workspace.name.clone(), false),
    };
    let after = workspace.safety.clone().unwrap_or_else(|| before.clone());
    let changes = relaxations(&before.effective(), &after.effective());
    let production = was_production || workspace.icon == PRODUCTION_ICON;
    if !changes.is_empty() && production {
        let typed = relax_confirmation.as_deref().map(str::trim).unwrap_or_default();
        if typed != name.trim() {
            return Err(AppError::structured(
                format!("Relaxing the rules of the Production workspace needs its name typed: \"{name}\"."),
                serde_json::json!({ "code": RELAX_CONFIRMATION_REQUIRED, "expected": name, "changes": changes }),
            ));
        }
    }
    let workspace_id = workspace.id.clone();
    let workspace_name = workspace.name.clone();
    let config = state.config.update(|config| {
        config.put_workspace(workspace);
        Ok(())
    })?;
    if !changes.is_empty() {
        state.db.gateway.audit(AuditEntry {
            caller: "settings".to_owned(),
            workspace_id,
            workspace: workspace_name,
            category: "rules".to_owned(),
            statement: format!("{:?} → {:?}", before.preset, after.preset),
            outcome: Some(Outcome::Relaxed),
            detail: changes.join("; "),
            ..AuditEntry::default()
        });
    }
    Ok(config)
}

/// Switches the active workspace and starts its auto-start tasks. Tasks of
/// other workspaces keep running so a port-forward in use is not cut off.
#[tauri::command]
pub fn set_active_workspace(state: State<'_, AppState>, id: String) -> AppResult<AppConfig> {
    let config = state.config.update(|config| config.set_active(&id))?;
    state.tasks.start_auto(&config.active().tasks);
    Ok(config)
}

#[tauri::command]
pub async fn delete_workspace(state: State<'_, AppState>, id: String) -> AppResult<AppConfig> {
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
