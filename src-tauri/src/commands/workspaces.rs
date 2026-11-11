//! Workspaces: named sets of connections and commands, each with its safety rules.

use crate::{
    audit::{AuditEntry, Outcome},
    config::{AppConfig, Workspace, WorkspaceMeta},
    connection_command,
    error::{AppError, AppResult},
    gateway::{
        classify::Category,
        rules::{relaxations, Preset, SafetySettings, PRODUCTION_ICON},
    },
    secrets, AppState,
};

pub const RELAX_CONFIRMATION_REQUIRED: &str = "RELAX_CONFIRMATION_REQUIRED";

/// Refuses with [`RELAX_CONFIRMATION_REQUIRED`] (detail: `expected`, `changes`) unless `typed` is the Production
/// workspace name `expected`. `message` says what is being relaxed.
pub fn require_relax_confirmation(typed: Option<&str>, expected: &str, changes: &[String], message: String) -> AppResult<()> {
    if typed.map(str::trim).unwrap_or_default() == expected.trim() {
        return Ok(());
    }
    Err(AppError::structured(
        message,
        serde_json::json!({ "code": RELAX_CONFIRMATION_REQUIRED, "expected": expected, "changes": changes }),
    ))
}

/// How saving `workspace` over `previous` (None for a new workspace) makes the rules less strict, the rules before and
/// after, and whether a Production workspace is involved (then the name must be typed). Removing the Production
/// marking counts as relaxing when the rules are then looser than Read-only, since only Production pins Read-only.
fn workspace_relaxations(previous: Option<&Workspace>, workspace: &WorkspaceMeta) -> (Vec<String>, SafetySettings, SafetySettings, bool) {
    let (before, was_production) = match previous {
        Some(previous) => (previous.safety(), previous.icon == PRODUCTION_ICON),
        None => (SafetySettings::for_new_workspace(&workspace.icon), false),
    };
    let after = workspace.safety.clone().unwrap_or_else(|| before.clone());
    let mut changes = relaxations(&before.effective(), &after.effective());
    if was_production && workspace.icon != PRODUCTION_ICON {
        let unmarked = relaxations(&SafetySettings::preset(Preset::ReadOnly).effective(), &after.effective());
        if !unmarked.is_empty() {
            changes.insert(0, "no longer marked Production".to_owned());
            for change in unmarked {
                if !changes.contains(&change) {
                    changes.push(change);
                }
            }
        }
    }
    let production = was_production || workspace.icon == PRODUCTION_ICON;
    (changes, before, after, production)
}

/// Creates or edits a workspace. Making its safety rules less strict is recorded in the audit
/// log; for a Production workspace it also needs `relax_confirmation` = the workspace name.
/// Removing the Production marking (shield icon) counts as relaxing when the rules are then looser than Read-only.
pub fn save_workspace(state: &AppState, workspace: WorkspaceMeta, relax_confirmation: Option<String>) -> AppResult<AppConfig> {
    if workspace.name.trim().is_empty() {
        return Err(AppError::new("Workspace name is required"));
    }
    let previous = state.config.read(|config| config.workspace(&workspace.id).ok().cloned());
    let name = previous
        .as_ref()
        .map_or_else(|| workspace.name.clone(), |previous| previous.name.clone());
    let (changes, before, after, production) = workspace_relaxations(previous.as_ref(), &workspace);
    if !changes.is_empty() && production {
        require_relax_confirmation(
            relax_confirmation.as_deref(),
            &name,
            &changes,
            format!("Relaxing the rules of the Production workspace needs its name typed: \"{name}\"."),
        )?;
    }
    // Pin the rules: a workspace without saved rules would otherwise get the default of its new icon.
    let mut workspace = workspace;
    if previous.is_some() {
        workspace.safety = Some(after.clone());
    }
    // MCP access: Production and Read-only rules allow at most read-only access.
    let mcp_before = previous.as_ref().map(|previous| previous.mcp).unwrap_or_default();
    let mcp_after = crate::mcp::capped(workspace.mcp.unwrap_or(mcp_before), &workspace.icon, &after);
    workspace.mcp = Some(mcp_after);
    let workspace_id = workspace.id.clone();
    let workspace_name = workspace.name.clone();
    let config = state.config.update(|config| {
        config.put_workspace(workspace);
        Ok(())
    })?;
    if !changes.is_empty() {
        state.engines.gateway.audit(AuditEntry {
            caller: "settings".to_owned(),
            workspace_id: workspace_id.clone(),
            workspace: workspace_name,
            category: "rules".to_owned(),
            statement: format!("{:?} → {:?}", before.preset, after.preset),
            outcome: Some(Outcome::Relaxed),
            detail: changes.join("; "),
            ..AuditEntry::default()
        });
    }
    if mcp_after > mcp_before {
        let workspace = config.workspace(&workspace_id).ok();
        state.engines.gateway.audit(AuditEntry {
            caller: "settings".to_owned(),
            workspace_id: workspace_id.clone(),
            workspace: workspace.map(|workspace| workspace.name.clone()).unwrap_or_default(),
            category: "mcp".to_owned(),
            statement: format!("MCP clients: {} → {}", mcp_before.label(), mcp_after.label()),
            outcome: Some(Outcome::Relaxed),
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
        connection_command::release(&state.tasks, &profile.id).await;
        secrets::delete_connection(&profile.id)?;
        super::profile::forget_connection(state, &profile.id);
    }
    state.config.update(|config| config.remove_workspace(&id).map(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(icon: &str, preset: Preset) -> Workspace {
        Workspace {
            id: "w".into(),
            name: "Prod".into(),
            color: String::new(),
            icon: icon.into(),
            safety: Some(SafetySettings::preset(preset)),
            assistant_queries: Default::default(),
            assistant_learn: Default::default(),
            mcp: Default::default(),
            connections: Vec::new(),
            tasks: Vec::new(),
            saved_queries: Vec::new(),
        }
    }

    fn meta(icon: &str, preset: Option<Preset>) -> WorkspaceMeta {
        WorkspaceMeta {
            id: "w".into(),
            name: "Prod".into(),
            color: String::new(),
            icon: icon.into(),
            safety: preset.map(SafetySettings::preset),
            assistant_queries: None,
            assistant_learn: None,
            mcp: None,
        }
    }

    #[test]
    fn removing_the_production_marking_counts_as_relaxing_unless_read_only() {
        let production = workspace(PRODUCTION_ICON, Preset::ReadOnly);
        // Same rules, shield removed, still Read-only: nothing is looser.
        let (changes, _, _, guarded) = workspace_relaxations(Some(&production), &meta("laptop", None));
        assert!(changes.is_empty(), "{changes:?}");
        assert!(guarded);
        // A Production workspace on Guarded rules (relaxed earlier, with the name) loses the shield: relaxing.
        let relaxed = workspace(PRODUCTION_ICON, Preset::Guarded);
        let (changes, _, _, guarded) = workspace_relaxations(Some(&relaxed), &meta("laptop", None));
        assert_eq!(changes[0], "no longer marked Production");
        assert!(changes.len() > 1 && guarded);
        // Shield removed and rules opened in the same save.
        let (changes, _, after, _) = workspace_relaxations(Some(&production), &meta("laptop", Some(Preset::Open)));
        assert_eq!(after.preset, Preset::Open);
        assert_eq!(changes.iter().filter(|change| *change == "no longer marked Production").count(), 1);
        let unique: std::collections::HashSet<_> = changes.iter().collect();
        assert_eq!(unique.len(), changes.len(), "no duplicates: {changes:?}");
        // Keeping the shield and the rules changes nothing; a non-Production workspace is not guarded.
        assert!(workspace_relaxations(Some(&production), &meta(PRODUCTION_ICON, None)).0.is_empty());
        let local = workspace("laptop", Preset::Guarded);
        let (changes, _, _, guarded) = workspace_relaxations(Some(&local), &meta("server", Some(Preset::Open)));
        assert!(!changes.is_empty() && !guarded);
    }

    #[test]
    fn the_typed_name_must_match() {
        let changes = vec!["x".to_owned()];
        assert!(require_relax_confirmation(Some(" Prod "), "Prod", &changes, String::new()).is_ok());
        for typed in [None, Some(""), Some("prod")] {
            let error = require_relax_confirmation(typed, "Prod", &changes, "typed".into()).unwrap_err();
            let detail = serde_json::to_value(&error).unwrap();
            assert!(detail.to_string().contains(RELAX_CONFIRMATION_REQUIRED), "{detail}");
            assert!(detail.to_string().contains("\"expected\":\"Prod\""), "{detail}");
        }
    }
}
