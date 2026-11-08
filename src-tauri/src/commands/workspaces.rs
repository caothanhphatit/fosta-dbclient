//! Workspaces: named sets of connections and startup tasks, each with its safety rules.

use crate::{
    audit::{AuditEntry, Outcome},
    config::{AppConfig, WorkspaceMeta},
    error::{AppError, AppResult},
    gateway::{
        classify::Category,
        rules::{relaxations, SafetySettings, PRODUCTION_ICON},
    },
    secrets, AppState,
};

pub const RELAX_CONFIRMATION_REQUIRED: &str = "RELAX_CONFIRMATION_REQUIRED";

/// Creates or edits a workspace. Making its safety rules less strict is recorded in the audit
/// log; for a Production workspace it also needs `relax_confirmation` = the workspace name.
pub fn save_workspace(state: &AppState, workspace: WorkspaceMeta, relax_confirmation: Option<String>) -> AppResult<AppConfig> {
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
        state.engines.gateway.audit(AuditEntry {
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

/// The confirmation dialog's "Don't ask again": stops asking for plain confirmations of
/// `categories` on this connection's rules (its own override, else its workspace's). Typed
/// confirmations keep asking, and a Production workspace always asks. Recorded in the audit log.
pub fn skip_confirmations(state: &AppState, connection_id: String, categories: Vec<Category>) -> AppResult<AppConfig> {
    let mut relaxed = None;
    let config = state.config.update(|config| {
        let workspace = config
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.connections.iter().any(|profile| profile.id == connection_id))
            .ok_or_else(|| AppError::new(format!("Connection '{connection_id}' no longer exists")))?;
        if workspace.icon == PRODUCTION_ICON {
            return Err(AppError::new(
                "A Production workspace always asks. Change its rules in the workspace settings instead.",
            ));
        }
        let Some(index) = workspace.connections.iter().position(|profile| profile.id == connection_id) else {
            return Ok(());
        };
        let own = workspace.connections[index].safety.clone();
        let before = own.clone().unwrap_or_else(|| workspace.safety());
        let Some(after) = before.without_confirmation(&categories) else {
            return Ok(());
        };
        relaxed = Some(AuditEntry {
            caller: "settings".to_owned(),
            workspace_id: workspace.id.clone(),
            workspace: workspace.name.clone(),
            category: "rules".to_owned(),
            statement: format!("{:?} → {:?} (don't ask again)", before.preset, after.preset),
            outcome: Some(Outcome::Relaxed),
            detail: relaxations(&before.effective(), &after.effective()).join("; "),
            ..AuditEntry::default()
        });
        if own.is_some() {
            workspace.connections[index].safety = Some(after);
        } else {
            workspace.safety = Some(after);
        }
        Ok(())
    })?;
    if let Some(entry) = relaxed {
        state.engines.gateway.audit(entry);
    }
    Ok(config)
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
        state.engines.disconnect(&profile.id).await;
        secrets::delete_connection(&profile.id)?;
    }
    state.config.update(|config| config.remove_workspace(&id).map(|_| ()))
}
