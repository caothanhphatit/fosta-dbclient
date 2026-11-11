//! Saved connection profiles, their passwords, and connecting.

use std::collections::HashMap;

use crate::{
    audit::{AuditEntry, Outcome},
    commands::workspaces::require_relax_confirmation,
    config::{AppConfig, ConnectionProfile},
    connection_command,
    contract::{spec::SshAuth, AuthMethodId, ConnectInfo, ConnectSecrets, SecretSlot},
    error::{AppError, AppResult, PASSWORD_REQUIRED},
    gateway::rules::{relaxations, Preset, PRODUCTION_ICON},
    net,
    runtime::{manager, ConnectionManager},
    secrets::{self, SecretKey},
    tasks::{TaskDef, TASKS_DISABLED},
    AppState,
};

/// Saves `profile` (normalised to the version-2 shape) with its secrets: `password` (kept for older callers)
/// and `secrets` per slot. A profile without "save password" loses every saved secret, and so does a profile
/// that now points at another server or route (see [`ConnectionProfile::same_target`]): only the secrets typed
/// with this save are kept, so the old ones are never sent to the new target.
///
/// `workspace_id` moves the connection; without it an existing connection stays where it is and a new one goes to
/// the active workspace. Making the connection's rules less strict (its own `safety` override, or a move to a
/// workspace with looser rules) is recorded in the audit log, and needs `relax_confirmation` = the name of the
/// Production workspace involved.
pub fn save_connection(
    state: &AppState,
    profile: ConnectionProfile,
    password: Option<String>,
    workspace_id: Option<String>,
    secrets: Option<HashMap<SecretSlot, String>>,
    relax_confirmation: Option<String>,
) -> AppResult<AppConfig> {
    let mut profile = profile;
    profile.normalize();
    let config = state.config.snapshot();
    if profile.command.is_some()
        && !state.capabilities.tasks
        && config.connection(&profile.id).is_none_or(|saved| saved.command != profile.command)
    {
        // Where commands are off, a saved reference is kept as it is, but no new one is made.
        return Err(AppError::new(TASKS_DISABLED));
    }
    let workspace_id = workspace_id.unwrap_or_else(|| match config.connection_scope(&profile.id) {
        Some((workspace, _)) => workspace.id.clone(),
        None => config.active_workspace.clone(),
    });
    let change = rule_change(&config, &workspace_id, &profile)?;
    if let Some(RuleChange {
        changes,
        production: Some(name),
        ..
    }) = &change
    {
        require_relax_confirmation(
            relax_confirmation.as_deref(),
            name,
            changes,
            format!("This makes the rules of a connection in the Production workspace less strict: type \"{name}\" to confirm."),
        )?;
    }
    // The profile as it will be stored (a move drops the old workspace's command), and refused before any secret changes.
    let mut trial = config.clone();
    trial.put_connection(&workspace_id, profile.clone())?;
    let stored = trial.connection(&profile.id).cloned().unwrap_or_else(|| profile.clone());
    remember_secrets(config.connection(&profile.id), &stored, password, secrets)?;
    let connection_id = profile.id.clone();
    let connection_name = profile.name.clone();
    let engine = profile.engine.clone();
    let saved = state.config.update(|config| config.put_connection(&workspace_id, profile))?;
    let command = saved.connection(&connection_id).and_then(|profile| profile.command.clone());
    if state.tasks.held_by(&connection_id).is_some_and(|held| Some(held) != command) {
        // Another command (or none, or a move): let go of the old one; the next connect starts the new one.
        connection_command::release_now(&state.tasks, &connection_id);
    }
    if let Some(change) = change {
        state.engines.gateway.audit(AuditEntry {
            caller: "settings".to_owned(),
            workspace_id: change.workspace_id,
            workspace: change.workspace,
            connection_id,
            connection: connection_name,
            engine,
            category: "rules".to_owned(),
            statement: format!("{:?} → {:?}", change.before, change.after),
            outcome: Some(Outcome::Relaxed),
            detail: change.changes.join("; "),
            ..AuditEntry::default()
        });
    }
    Ok(saved)
}

/// How a save makes a connection's rules less strict.
#[derive(Debug)]
struct RuleChange {
    changes: Vec<String>,
    /// The name of the Production workspace whose name must be typed, if one is involved.
    production: Option<String>,
    /// The workspace the connection ends up in.
    workspace_id: String,
    workspace: String,
    before: Preset,
    after: Preset,
}

/// The rules `profile` gets by being saved into `workspace_id`, compared with the rules it has now (or, for a new
/// connection, the workspace's). A connection that moves is compared with its old rules, and also with the rules
/// of a Production workspace it enters (an override looser than that workspace's rules). None when nothing gets looser.
fn rule_change(config: &AppConfig, workspace_id: &str, profile: &ConnectionProfile) -> AppResult<Option<RuleChange>> {
    let target = config.workspace(workspace_id)?;
    let current = config.connection_scope(&profile.id);
    // Saving without an override keeps the saved one (see `AppConfig::put_connection`).
    let after = profile
        .safety
        .clone()
        .or_else(|| current.and_then(|(_, saved)| saved.safety.clone()))
        .unwrap_or_else(|| target.safety());
    let (before, mut changes, mut production) = match current {
        Some((source, saved)) => {
            let before = saved.safety.clone().unwrap_or_else(|| source.safety());
            let production = (source.icon == PRODUCTION_ICON).then(|| source.name.clone());
            (before.clone(), relaxations(&before.effective(), &after.effective()), production)
        }
        None => (target.safety(), Vec::new(), None),
    };
    let entering = current.is_none_or(|(source, _)| source.id != target.id);
    if entering {
        let looser = relaxations(&target.safety().effective(), &after.effective());
        if !looser.is_empty() && production.is_none() && target.icon == PRODUCTION_ICON {
            production = Some(target.name.clone());
        }
        for change in looser {
            if !changes.contains(&change) {
                changes.push(change);
            }
        }
    }
    if changes.is_empty() {
        return Ok(None);
    }
    Ok(Some(RuleChange {
        changes,
        production,
        workspace_id: target.id.clone(),
        workspace: target.name.clone(),
        before: before.preset,
        after: after.preset,
    }))
}

pub async fn delete_connection(state: &AppState, id: String) -> AppResult<AppConfig> {
    state.engines.disconnect(&id).await;
    connection_command::release(&state.tasks, &id).await;
    secrets::delete_connection(&id)?;
    super::profile::forget_connection(state, &id);
    state.config.update(|config| {
        config.remove_connection(&id);
        Ok(())
    })
}

/// Tries a profile from the connection dialog. Without a typed password, the saved secrets are used only when
/// the profile still points at the saved connection's target (`ConnectionProfile::same_target`). `secrets` are the
/// other slots typed in the dialog (SSH password or key passphrase, TLS key passphrase); they win over saved ones.
pub async fn test_connection(
    state: &AppState,
    profile: ConnectionProfile,
    password: Option<String>,
    secrets: Option<HashMap<SecretSlot, String>>,
) -> AppResult<ConnectInfo> {
    let mut profile = profile;
    profile.normalize();
    let saved = state.config.read(|config| config.connection(&profile.id).cloned());
    let bound = saved.as_ref().is_some_and(|saved| saved.same_target(&profile));
    let typed = secrets;
    let mut secrets = match password {
        Some(password) => with_password(bound.then_some(&profile), password)?,
        None if bound => saved_secrets(&profile)?,
        None => return Err(password_required()),
    };
    for (slot, value) in typed.unwrap_or_default() {
        if slot != SecretSlot::Password && !value.is_empty() {
            secrets.values.insert(slot, value);
        }
    }
    let probe = match command_of(state, &profile)? {
        Some(def) => {
            let ready = connection_command::ready_check(&profile.host, profile.port);
            Some(connection_command::probe(&state.tasks, &def, ready).await?)
        }
        None => None,
    };
    let probed = ConnectionManager::probe(profile.spec(), secrets).await;
    if let Some(probe) = probe {
        probe.finish(&state.tasks).await;
    }
    probed
}

/// The command `profile` uses, if any.
fn command_of(state: &AppState, profile: &ConnectionProfile) -> AppResult<Option<TaskDef>> {
    let Some(id) = &profile.command else {
        return Ok(None);
    };
    match state.config.read(|config| config.task(id).cloned()) {
        Some(def) => Ok(Some(def)),
        None => Err(AppError::new(format!(
            "The command of connection '{}' no longer exists: edit the connection to pick another.",
            profile.name
        ))),
    }
}

/// Connects a saved connection, after starting its command (or reusing the running one) when it uses one.
pub async fn connect(state: &AppState, id: String, password: Option<String>) -> AppResult<ConnectInfo> {
    let profile = state.config.connection(&id)?;
    let secrets = match password {
        Some(password) => with_password(Some(&profile), password)?,
        None => saved_secrets(&profile)?,
    };
    match command_of(state, &profile)? {
        Some(def) => {
            let ready = connection_command::ready_check(&profile.host, profile.port);
            connection_command::acquire(&state.tasks, &id, &def, ready).await?;
        }
        None => connection_command::release(&state.tasks, &id).await,
    }
    connect_with(state, &id, &profile, secrets).await
}

/// Connects a saved connection with its saved secrets for an MCP client, which never starts a command: a connection
/// whose command is not running fails with `COMMAND_NOT_RUNNING` (checked before the secrets are read).
pub async fn connect_for_mcp(state: &AppState, id: &str) -> AppResult<ConnectInfo> {
    let profile = state.config.connection(id)?;
    let command = command_of(state, &profile)?;
    if let Some(def) = &command {
        if !state.tasks.is_active(&def.id) {
            return Err(AppError::coded(
                connection_command::COMMAND_NOT_RUNNING,
                format!("The command \"{}\" this connection needs is not running.", def.name),
            ));
        }
    }
    let secrets = saved_secrets(&profile)?;
    match command {
        Some(def) => {
            let ready = connection_command::ready_check(&profile.host, profile.port);
            connection_command::acquire_running(&state.tasks, id, &def, ready).await?;
        }
        None => connection_command::release(&state.tasks, id).await,
    }
    connect_with(state, id, &profile, secrets).await
}

async fn connect_with(state: &AppState, id: &str, profile: &ConnectionProfile, secrets: ConnectSecrets) -> AppResult<ConnectInfo> {
    let connected = state.engines.connect(id, profile.spec(), secrets).await;
    if connected.is_err() && state.engines.handle(id).is_err() {
        connection_command::release(&state.tasks, id).await;
    }
    connected
}

/// Records an SSH server's host key in the app's known_hosts file, after the user accepted the fingerprint an
/// `SSH_HOST_KEY_UNKNOWN` error showed (its detail carries `host`, `port` and `hostKey.publicKey`).
pub fn trust_host_key(_state: &AppState, host: String, port: u16, public_key: String) -> AppResult<()> {
    net::ssh::trust_host_key(&manager::known_hosts_file()?, &host, port, &public_key)
}

pub async fn disconnect(state: &AppState, id: String) -> AppResult<()> {
    state.engines.disconnect(&id).await;
    connection_command::release(&state.tasks, &id).await;
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

/// Stores the secrets of a save. Without "save password" every saved secret goes. When the profile no longer points at
/// the `saved` profile's target, the saved secrets go too and only the ones typed now are kept, so the user enters
/// them again (connecting then asks for the password as usual).
fn remember_secrets(
    saved: Option<&ConnectionProfile>,
    profile: &ConnectionProfile,
    password: Option<String>,
    values: Option<HashMap<SecretSlot, String>>,
) -> AppResult<()> {
    if !profile.save_password || saved.is_some_and(|saved| !saved.same_target(profile)) {
        secrets::delete_connection(&profile.id)?;
    }
    if !profile.save_password {
        return Ok(());
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
    use crate::{
        config::{Workspace, WorkspaceMeta},
        contract::{SshConfig, TlsConfig},
        gateway::rules::SafetySettings,
        secrets::testing::memory_store,
    };

    fn profile(host: &str) -> ConnectionProfile {
        ConnectionProfile {
            id: "c".into(),
            name: "Orders".into(),
            engine: "postgres".into(),
            host: host.into(),
            port: 5432,
            user: "app".into(),
            save_password: true,
            ..Default::default()
        }
    }

    const PASSWORD: &str = "conn:c:password";
    const SSH_PASSWORD: &str = "conn:c:sshPassword";

    #[test]
    fn saving_the_same_target_keeps_the_saved_secrets() {
        let store = memory_store(&[(PASSWORD, "pw"), (SSH_PASSWORD, "ssh")]);
        let saved = profile("db");
        let mut renamed = saved.clone();
        renamed.name = "Renamed".into();
        remember_secrets(Some(&saved), &renamed, None, None).unwrap();
        assert_eq!(store.accounts(), [PASSWORD, SSH_PASSWORD]);
    }

    #[test]
    fn a_new_target_forgets_the_secrets_that_were_not_typed_again() {
        let store = memory_store(&[(PASSWORD, "pw"), (SSH_PASSWORD, "ssh")]);
        remember_secrets(Some(&profile("db")), &profile("evil"), None, None).unwrap();
        assert!(store.accounts().is_empty());
        // Connecting then asks for the password as usual.
        let error = saved_secrets(&profile("evil")).unwrap_err();
        assert_eq!(serde_json::to_value(&error).unwrap()["code"], PASSWORD_REQUIRED);

        let store = memory_store(&[(PASSWORD, "pw"), (SSH_PASSWORD, "ssh")]);
        remember_secrets(Some(&profile("db")), &profile("other"), Some("new".into()), None).unwrap();
        assert_eq!(store.accounts(), [PASSWORD]);
        assert_eq!(store.secrets.lock().unwrap()[PASSWORD], "new");

        // A changed tunnel counts as a new target too.
        let store = memory_store(&[(PASSWORD, "pw")]);
        let mut tunnelled = profile("db");
        tunnelled.ssh = Some(SshConfig {
            enabled: true,
            host: "bastion".into(),
            port: 22,
            user: "me".into(),
            auth: SshAuth::Password,
            jump_hosts: Vec::new(),
            host_key: Default::default(),
            keepalive_secs: None,
        });
        let typed = HashMap::from([(SecretSlot::SshPassword, "ssh".to_owned())]);
        remember_secrets(Some(&profile("db")), &tunnelled, None, Some(typed)).unwrap();
        assert_eq!(store.accounts(), [SSH_PASSWORD]);
    }

    #[test]
    fn a_new_connection_or_no_save_password() {
        let store = memory_store(&[]);
        remember_secrets(None, &profile("db"), Some("pw".into()), None).unwrap();
        assert_eq!(store.accounts(), [PASSWORD]);
        let mut unsaved = profile("db");
        unsaved.save_password = false;
        remember_secrets(Some(&profile("db")), &unsaved, Some("pw".into()), None).unwrap();
        assert!(store.accounts().is_empty());
    }

    fn workspace(id: &str, icon: &str, preset: Preset) -> Workspace {
        let mut config = AppConfig::default();
        config.put_workspace(WorkspaceMeta {
            id: id.into(),
            name: format!("{id} name"),
            color: String::new(),
            icon: icon.into(),
            safety: Some(SafetySettings::preset(preset)),
            assistant_queries: None,
            mcp: None,
        });
        config.workspace(id).unwrap().clone()
    }

    /// `prod` (Production, Read-only) holding connection "c", `local` (Open) and `guarded` (Guarded).
    fn config() -> AppConfig {
        let mut config = AppConfig::default();
        let mut prod = workspace("prod", PRODUCTION_ICON, Preset::ReadOnly);
        prod.connections.push(profile("db"));
        config.workspaces = vec![
            prod,
            workspace("local", "laptop", Preset::Open),
            workspace("guarded", "server", Preset::Guarded),
        ];
        config
    }

    #[test]
    fn a_looser_override_in_production_needs_the_name() {
        let config = config();
        let mut open = profile("db");
        open.safety = Some(SafetySettings::preset(Preset::Open));
        let change = rule_change(&config, "prod", &open).unwrap().unwrap();
        assert_eq!(change.production.as_deref(), Some("prod name"));
        assert_eq!((change.before, change.after), (Preset::ReadOnly, Preset::Open));
        // Unchanged re-saves (with or without the override sent back) do not ask.
        assert!(rule_change(&config, "prod", &profile("db")).unwrap().is_none());
        let mut config = config;
        config.workspaces[0].connections[0].safety = Some(SafetySettings::preset(Preset::Open));
        assert!(rule_change(&config, "prod", &open).unwrap().is_none());
        assert!(rule_change(&config, "prod", &profile("db")).unwrap().is_none());
        // Tightening does not ask.
        let mut guarded = profile("db");
        guarded.safety = Some(SafetySettings::preset(Preset::Guarded));
        assert!(rule_change(&config, "prod", &guarded).unwrap().is_none());
    }

    #[test]
    fn a_looser_override_elsewhere_is_only_audited() {
        let mut config = config();
        let mut fresh = profile("db");
        fresh.id = "new".into();
        fresh.safety = Some(SafetySettings::preset(Preset::Open));
        let change = rule_change(&config, "guarded", &fresh).unwrap().unwrap();
        assert!(change.production.is_none() && !change.changes.is_empty());
        // A new connection with a looser override in Production asks for the name.
        let change = rule_change(&config, "prod", &fresh).unwrap().unwrap();
        assert_eq!(change.production.as_deref(), Some("prod name"));
        // A new connection without an override changes nothing.
        fresh.safety = None;
        assert!(rule_change(&config, "prod", &fresh).unwrap().is_none());
        config.workspaces[0].connections.clear();
        assert!(rule_change(&config, "local", &fresh).unwrap().is_none());
    }

    #[test]
    fn moving_out_of_production_to_looser_rules_needs_the_name() {
        let config = config();
        let change = rule_change(&config, "local", &profile("db")).unwrap().unwrap();
        assert_eq!(change.production.as_deref(), Some("prod name"), "the source workspace's name");
        assert_eq!(change.workspace_id, "local");
        // Moving a Production connection with its own Read-only override keeps its rules: nothing to confirm.
        let mut pinned = profile("db");
        pinned.safety = Some(SafetySettings::preset(Preset::ReadOnly));
        assert!(rule_change(&config, "local", &pinned).unwrap().is_none());
    }

    #[test]
    fn moving_a_loose_connection_into_production_needs_the_name() {
        let mut config = config();
        let mut loose = profile("db");
        loose.id = "l".into();
        loose.safety = Some(SafetySettings::preset(Preset::Open));
        config.workspaces[1].connections.push(loose.clone());
        let change = rule_change(&config, "prod", &loose).unwrap().unwrap();
        assert_eq!(change.production.as_deref(), Some("prod name"));
        // Without an override it takes Production's rules: nothing gets looser.
        loose.safety = None;
        config.workspaces[1].connections[0].safety = None;
        assert!(rule_change(&config, "prod", &loose).unwrap().is_none());
        // Moving between non-Production workspaces to looser rules is audited only.
        let change = rule_change(&config, "guarded", &loose).unwrap();
        assert!(change.is_none(), "Open → Guarded is stricter");
        config.workspaces[2].connections.push({
            let mut guarded = profile("db");
            guarded.id = "g".into();
            guarded
        });
        let mut moving = profile("db");
        moving.id = "g".into();
        let change = rule_change(&config, "local", &moving).unwrap().unwrap();
        assert!(change.production.is_none() && !change.changes.is_empty());
    }

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

    use crate::connection_command::{testing, COMMAND_FAILED};
    use crate::tasks::{ReadyCheck, TaskState};

    fn using(id: &str, command: Option<&str>) -> ConnectionProfile {
        ConnectionProfile {
            id: id.into(),
            name: id.to_uppercase(),
            engine: "postgres".into(),
            host: "127.0.0.1".into(),
            port: 5432,
            user: "postgres".into(),
            database: Some("shop".into()),
            command: command.map(str::to_owned),
            ..Default::default()
        }
    }

    /// A state whose only workspace has the command `def` and the connections `profiles`.
    fn state_with(name: &str, capabilities: crate::Capabilities, def: &TaskDef, profiles: &[ConnectionProfile]) -> AppState {
        let state = AppState::for_test(name, capabilities, AppConfig::default());
        let workspace = state.config.snapshot().workspaces[0].id.clone();
        state
            .config
            .update(|config| {
                config.put_task(&workspace, def.clone())?;
                profiles
                    .iter()
                    .try_for_each(|profile| config.put_connection(&workspace, profile.clone()))
            })
            .unwrap();
        state
    }

    /// What `connect` does before the database connects (the unit tests have no database).
    async fn hold(state: &AppState, connection_id: &str, def: &TaskDef) -> bool {
        connection_command::acquire(&state.tasks, connection_id, def, ReadyCheck::Spawned)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn commands_are_refused_where_tasks_are_off() {
        let _store = memory_store(&[]);
        let (def, _) = testing::long_running("pf");
        let profile = using("off", Some("pf"));
        let state = state_with(
            "command-off",
            crate::Capabilities::web(false, true),
            &def,
            std::slice::from_ref(&profile),
        );
        // A saved reference is kept as it is (not dropped), but no new one is made.
        let mut renamed = profile.clone();
        renamed.name = "Renamed".into();
        save_connection(&state, renamed, None, None, None, None).unwrap();
        assert_eq!(state.config.connection("off").unwrap().command.as_deref(), Some("pf"));
        let error = save_connection(&state, using("new", Some("pf")), None, None, None, None).unwrap_err();
        assert_eq!(error.to_string(), TASKS_DISABLED);
        let error = connect(&state, "off".into(), Some("pw".into())).await.unwrap_err();
        assert_eq!(error.to_string(), TASKS_DISABLED);
        save_connection(&state, using("off", None), None, None, None, None).unwrap();
        let error = save_connection(&state, profile, None, None, None, None).unwrap_err();
        assert_eq!(error.to_string(), TASKS_DISABLED);
    }

    #[tokio::test]
    async fn a_parsed_connection_string_never_sets_a_command() {
        let state = AppState::for_test("command-parse", crate::Capabilities::desktop(), AppConfig::default());
        for (engine, text) in [
            ("postgres", "postgres://app:pw@127.0.0.1:13306/shop?sslmode=disable"),
            ("mariadb", "mysql://root:pw@127.0.0.1:13306/shop"),
            ("mongodb", "mongodb://root:pw@127.0.0.1:27017/?authSource=admin"),
        ] {
            let parsed = crate::commands::engines::parse_connection_string(&state, engine.into(), text.into()).unwrap();
            let json = serde_json::to_value(&parsed).unwrap();
            assert!(json["spec"].get("commandId").is_none(), "{engine}: {json}");
            assert!(json.get("commandId").is_none(), "{engine}: {json}");
        }
    }

    #[tokio::test]
    async fn a_command_that_exits_fails_the_connect_with_its_log() {
        let _store = memory_store(&[]);
        let def = testing::exits_early("pf");
        let state = state_with("command-early", crate::Capabilities::desktop(), &def, &[using("early", Some("pf"))]);
        let error = connect(&state, "early".into(), Some("pw".into())).await.unwrap_err();
        assert_eq!(error.field("code").and_then(|code| code.as_str()), Some(COMMAND_FAILED));
        assert!(error.to_string().contains("boom: port is in use"), "{error}");
        assert!(!state.tasks.is_active("pf"));
        assert_eq!(state.tasks.held_by("early"), None);
    }

    #[tokio::test]
    async fn a_missing_command_fails_clearly() {
        let _store = memory_store(&[]);
        let state = AppState::for_test("command-missing", crate::Capabilities::desktop(), AppConfig::default());
        let error = test_connection(&state, using("gone", Some("deleted")), Some("pw".into()), None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no longer exists"), "{error}");
    }

    #[tokio::test]
    async fn a_refused_command_or_a_move_keeps_the_secrets_rule() {
        let _store = memory_store(&[("conn:k:password", "pw")]);
        let (def, _) = testing::long_running("pf");
        let mut saved = using("k", Some("pf"));
        saved.save_password = true;
        let state = state_with(
            "command-secrets",
            crate::Capabilities::desktop(),
            &def,
            std::slice::from_ref(&saved),
        );
        let stored = || secrets::get(&SecretKey::connection("k", SecretSlot::Password)).unwrap();

        // A command that is not one of the workspace's (deleted in another window): refused, the password stays.
        let stale = ConnectionProfile {
            command: Some("deleted".into()),
            ..saved.clone()
        };
        let error = save_connection(&state, stale, None, None, None, None).unwrap_err();
        assert!(error.to_string().contains("not a command"), "{error}");
        assert_eq!(stored().as_deref(), Some("pw"));

        // A move drops the old workspace's command: another target, so the saved password is forgotten.
        state
            .config
            .update(|config| {
                config.put_workspace(WorkspaceMeta {
                    id: "other".into(),
                    name: "Other".into(),
                    color: String::new(),
                    icon: "box".into(),
                    safety: None,
                    assistant_queries: None,
                    mcp: None,
                });
                Ok(())
            })
            .unwrap();
        let moved = save_connection(&state, saved, None, Some("other".into()), None, None).unwrap();
        assert_eq!(moved.connection("k").unwrap().command, None);
        assert_eq!(stored(), None);
    }

    #[tokio::test]
    async fn a_shared_command_stops_with_its_last_connection() {
        let _store = memory_store(&[]);
        let (def, _) = testing::long_running("pf");
        let profiles = [using("a", Some("pf")), using("b", Some("pf"))];
        let state = state_with("command-shared", crate::Capabilities::desktop(), &def, &profiles);

        assert!(hold(&state, "a", &def).await);
        assert!(!hold(&state, "b", &def).await, "b reuses a's command");
        disconnect(&state, "a".into()).await.unwrap();
        assert!(state.tasks.is_active("pf"), "b still uses it");
        disconnect(&state, "b".into()).await.unwrap();
        assert!(!state.tasks.is_active("pf"), "the last connection stops it");

        // A command started by hand (or with the workspace) keeps running.
        crate::commands::tasks::start_task(&state, "pf".into()).unwrap();
        assert!(!hold(&state, "a", &def).await);
        disconnect(&state, "a".into()).await.unwrap();
        assert!(state.tasks.is_active("pf"));
        crate::commands::tasks::stop_task(&state, "pf".into());
        state.tasks.wait_until_stopped("pf").await.unwrap();
    }

    #[tokio::test]
    async fn the_command_stops_on_switch_delete_and_workspace_delete() {
        let _store = memory_store(&[]);
        let (def, _) = testing::long_running("pf");
        let state = state_with("command-life", crate::Capabilities::desktop(), &def, &[using("life", Some("pf"))]);

        hold(&state, "life", &def).await;
        save_connection(&state, using("life", Some("pf")), None, None, None, None).unwrap();
        assert!(state.tasks.is_active("pf"), "an edit that keeps the command keeps it running");
        save_connection(&state, using("life", None), None, None, None, None).unwrap();
        state.tasks.wait_until_stopped("pf").await.unwrap();
        assert!(!state.tasks.is_active("pf"), "switching to no command stops it");

        save_connection(&state, using("life", Some("pf")), None, None, None, None).unwrap();
        hold(&state, "life", &def).await;
        delete_connection(&state, "life".into()).await.unwrap();
        assert!(!state.tasks.is_active("pf"), "delete stops it");

        let doomed = WorkspaceMeta {
            id: "doomed".into(),
            name: "Doomed".into(),
            color: "red".into(),
            icon: "box".into(),
            safety: None,
            assistant_queries: None,
            mcp: None,
        };
        let other = TaskDef {
            id: "pf2".into(),
            ..def.clone()
        };
        state
            .config
            .update(|config| {
                config.put_workspace(doomed);
                config.put_task("doomed", other.clone())?;
                config.put_connection("doomed", using("life", Some("pf2")))
            })
            .unwrap();
        hold(&state, "life", &other).await;
        crate::commands::workspaces::delete_workspace(&state, "doomed".into())
            .await
            .unwrap();
        assert!(!state.tasks.is_active("pf2"), "deleting its workspace stops it");
        assert_eq!(state.tasks.held_by("life"), None);
    }

    #[tokio::test]
    async fn deleting_a_command_clears_it_from_its_connections() {
        let _store = memory_store(&[]);
        let (def, _) = testing::long_running("pf");
        let profiles = [using("a", Some("pf")), using("b", Some("pf"))];
        let state = state_with("command-delete", crate::Capabilities::desktop(), &def, &profiles);
        hold(&state, "a", &def).await;
        let config = crate::commands::tasks::delete_task(&state, "pf".into()).unwrap();
        assert!(config.all_tasks().next().is_none());
        assert_eq!(config.connection("a").unwrap().command, None);
        assert_eq!(config.connection("b").unwrap().command, None);
        state.tasks.wait_until_stopped("pf").await.unwrap();
        disconnect(&state, "a".into()).await.unwrap();
    }

    /// The whole path against the dev Postgres: the command only waits (the port is already open), so this checks
    /// test (temporary) → start → ready (port) → connect → shared by a second connection → stopped with the last one.
    #[tokio::test]
    #[ignore = "needs the dev Postgres on 127.0.0.1:5432 (dev/compose)"]
    async fn live_a_command_runs_while_postgres_connections_use_it() {
        let _store = memory_store(&[]);
        let (def, _) = testing::long_running("pf");
        let profiles = [using("live-a", Some("pf")), using("live-b", Some("pf"))];
        let state = state_with("command-live", crate::Capabilities::desktop(), &def, &profiles);

        let info = test_connection(&state, profiles[0].clone(), Some("studio-root".into()), None)
            .await
            .unwrap();
        assert_eq!(info.engine, "postgres");
        assert!(!state.tasks.is_active("pf"), "a test stops the command it started");

        connect(&state, "live-a".into(), Some("studio-root".into())).await.unwrap();
        let status = state.tasks.status("pf");
        assert_eq!(status.state, TaskState::Running);
        assert!(status.for_connections);
        assert!(state.engines.handle("live-a").is_ok());
        assert!(state.tasks.logs("pf").first().is_some_and(|line| line.starts_with('▶')));

        connect(&state, "live-b".into(), Some("studio-root".into())).await.unwrap();
        assert_eq!(state.tasks.status("pf").pid, status.pid, "the second connection shares the command");
        test_connection(&state, profiles[1].clone(), Some("studio-root".into()), None)
            .await
            .unwrap();
        assert!(state.tasks.is_active("pf"), "a test keeps a command in use running");

        disconnect(&state, "live-a".into()).await.unwrap();
        assert!(state.tasks.is_active("pf"), "live-b still uses it");
        disconnect(&state, "live-b".into()).await.unwrap();
        assert!(!state.tasks.is_active("pf"));
        assert_eq!(state.tasks.status("pf").state, TaskState::Stopped);
        assert!(state.engines.handle("live-b").is_err());
    }
}
