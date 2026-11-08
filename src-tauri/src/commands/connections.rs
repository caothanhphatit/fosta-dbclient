//! Saved connection profiles, their passwords, and connecting.

use std::collections::HashMap;

use crate::{
    config::{AppConfig, ConnectionProfile},
    contract::{spec::SshAuth, AuthMethodId, ConnectInfo, ConnectSecrets, SecretSlot},
    error::{AppError, AppResult, PASSWORD_REQUIRED},
    runtime::ConnectionManager,
    secrets::{self, SecretKey},
    AppState,
};

/// Saves `profile` (normalised to the version-2 shape) with its secrets: `password` (kept for older callers)
/// and `secrets` per slot. A profile without "save password" loses every saved secret.
pub fn save_connection(
    state: &AppState,
    profile: ConnectionProfile,
    password: Option<String>,
    workspace_id: Option<String>,
    secrets: Option<HashMap<SecretSlot, String>>,
) -> AppResult<AppConfig> {
    remember_secrets(&profile, password, secrets)?;
    let workspace_id = workspace_id.unwrap_or_else(|| state.config.snapshot().active_workspace);
    state.config.update(|config| config.put_connection(&workspace_id, profile))
}

pub async fn delete_connection(state: &AppState, id: String) -> AppResult<AppConfig> {
    state.engines.disconnect(&id).await;
    secrets::delete_connection(&id)?;
    state.config.update(|config| {
        config.remove_connection(&id);
        Ok(())
    })
}

/// Tries a profile from the connection dialog. Without a typed password, the saved secrets are used only when
/// the profile still points at the saved connection's server (engine, host, port, user, url, file).
pub async fn test_connection(state: &AppState, profile: ConnectionProfile, password: Option<String>) -> AppResult<ConnectInfo> {
    let mut profile = profile;
    profile.normalize();
    let saved = state.config.read(|config| config.connection(&profile.id).cloned());
    let bound = saved.as_ref().is_some_and(|saved| saved.same_target(&profile));
    let secrets = match password {
        Some(password) => with_password(bound.then_some(&profile), password)?,
        None if bound => saved_secrets(&profile)?,
        None => return Err(password_required()),
    };
    ConnectionManager::probe(profile.spec(), secrets).await
}

pub async fn connect(state: &AppState, id: String, password: Option<String>) -> AppResult<ConnectInfo> {
    let profile = state.config.connection(&id)?;
    let secrets = match password {
        Some(password) => with_password(Some(&profile), password)?,
        None => saved_secrets(&profile)?,
    };
    state.engines.connect(&id, profile.spec(), secrets).await
}

pub async fn disconnect(state: &AppState, id: String) -> AppResult<()> {
    state.engines.disconnect(&id).await;
    Ok(())
}

/// Makes the UI ask for the password.
fn password_required() -> AppError {
    AppError::coded(PASSWORD_REQUIRED, "A password is required")
}

/// The saved secrets of a profile; the password is required, the other slots are read when the profile uses them.
fn saved_secrets(profile: &ConnectionProfile) -> AppResult<ConnectSecrets> {
    let password = secrets::get(&SecretKey::connection(&profile.id, SecretSlot::Password))?.ok_or_else(password_required)?;
    with_password(Some(profile), password)
}

/// `password` plus, when `saved` is given, that profile's other saved secrets.
fn with_password(saved: Option<&ConnectionProfile>, password: String) -> AppResult<ConnectSecrets> {
    let mut connect = ConnectSecrets::password(&password);
    if let Some(profile) = saved {
        for slot in extra_slots(profile) {
            if let Some(secret) = secrets::get(&SecretKey::connection(&profile.id, slot))? {
                connect.values.insert(slot, secret);
            }
        }
    }
    Ok(connect)
}

/// Slots besides the password that the profile's settings can use.
fn extra_slots(profile: &ConnectionProfile) -> Vec<SecretSlot> {
    let mut slots = Vec::new();
    if let Some(ssh) = profile.ssh.as_ref().filter(|ssh| ssh.enabled) {
        slots.push(match ssh.auth {
            SshAuth::Password => SecretSlot::SshPassword,
            SshAuth::PrivateKey { .. } | SshAuth::Agent => SecretSlot::SshKeyPassphrase,
        });
    }
    if profile.tls.as_ref().is_some_and(|tls| tls.client_key_file.is_some()) {
        slots.push(SecretSlot::TlsKeyPassphrase);
    }
    match profile.auth {
        Some(AuthMethodId::ApiKey) => slots.push(SecretSlot::ApiKey),
        Some(AuthMethodId::Bearer) => slots.push(SecretSlot::Token),
        _ => {}
    }
    slots
}

fn remember_secrets(profile: &ConnectionProfile, password: Option<String>, values: Option<HashMap<SecretSlot, String>>) -> AppResult<()> {
    if !profile.save_password {
        return secrets::delete_connection(&profile.id);
    }
    let mut values = values.unwrap_or_default();
    if let Some(password) = password {
        values.entry(SecretSlot::Password).or_insert(password);
    }
    for (slot, secret) in values {
        secrets::set(&SecretKey::connection(&profile.id, slot), &secret)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{SshConfig, TlsConfig};

    #[test]
    fn extra_slots_follow_the_profile() {
        let mut profile = ConnectionProfile {
            id: "c".into(),
            engine: "postgres".into(),
            ..Default::default()
        };
        assert!(extra_slots(&profile).is_empty());
        profile.ssh = Some(SshConfig {
            enabled: true,
            host: "bastion".into(),
            port: 22,
            user: "me".into(),
            auth: SshAuth::Password,
            jump_hosts: Vec::new(),
            host_key: Default::default(),
            keepalive_secs: None,
        });
        profile.tls = Some(TlsConfig {
            client_key_file: Some("key.pem".into()),
            ..Default::default()
        });
        profile.auth = Some(AuthMethodId::Bearer);
        assert_eq!(
            extra_slots(&profile),
            [SecretSlot::SshPassword, SecretSlot::TlsKeyPassphrase, SecretSlot::Token]
        );
    }
}
