//! Saved connection profiles, their passwords, and connecting.

use crate::{
    config::{AppConfig, ConnectionProfile},
    contract::{ConnectInfo, ConnectSecrets},
    error::{AppError, AppResult, PASSWORD_REQUIRED},
    runtime::ConnectionManager,
    secrets, AppState,
};

pub fn save_connection(
    state: &AppState,
    profile: ConnectionProfile,
    password: Option<String>,
    workspace_id: Option<String>,
) -> AppResult<AppConfig> {
    remember_password(&profile, password.as_deref())?;
    let workspace_id = workspace_id.unwrap_or_else(|| state.config.snapshot().active_workspace);
    state.config.update(|config| config.put_connection(&workspace_id, profile))
}

pub async fn delete_connection(state: &AppState, id: String) -> AppResult<AppConfig> {
    state.engines.disconnect(&id).await;
    secrets::forget_password(&id)?;
    state.config.update(|config| {
        config.remove_connection(&id);
        Ok(())
    })
}

pub async fn test_connection(_state: &AppState, profile: ConnectionProfile, password: Option<String>) -> AppResult<ConnectInfo> {
    let password = resolve_password(&profile.id, password)?;
    ConnectionManager::probe(profile.spec(), ConnectSecrets::password(&password)).await
}

pub async fn connect(state: &AppState, id: String, password: Option<String>) -> AppResult<ConnectInfo> {
    let profile = state.config.connection(&id)?;
    let password = resolve_password(&id, password)?;
    state
        .engines
        .connect(&id, profile.spec(), ConnectSecrets::password(&password))
        .await
}

pub async fn disconnect(state: &AppState, id: String) -> AppResult<()> {
    state.engines.disconnect(&id).await;
    Ok(())
}

/// The password typed in the dialog, else the saved one; `PASSWORD_REQUIRED` makes the UI ask.
fn resolve_password(connection_id: &str, supplied: Option<String>) -> AppResult<String> {
    match supplied {
        Some(password) => Ok(password),
        None => secrets::load_password(connection_id)?.ok_or_else(|| AppError::new(PASSWORD_REQUIRED)),
    }
}

fn remember_password(profile: &ConnectionProfile, password: Option<&str>) -> AppResult<()> {
    match (profile.save_password, password) {
        (true, Some(password)) => secrets::save_password(&profile.id, password),
        (true, None) => Ok(()),
        (false, _) => secrets::forget_password(&profile.id),
    }
}
