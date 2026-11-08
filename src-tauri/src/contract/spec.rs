//! What an engine needs to connect, independent of how profiles are stored (`config.rs` converts a profile into a
//! [`ConnectionSpec`]), and what it reports back.

use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};

use super::{
    capability::CapabilitySet,
    descriptor::{AuthMethodId, LanguageSpec},
    object::NodeAction,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSpec {
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
    #[serde(default)]
    pub file: Option<String>,
    /// A connection string without its password (Mongo SRV / replica sets, ES node lists or Cloud ID).
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub options: HashMap<String, String>,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    #[serde(default)]
    pub auth: Option<AuthMethodId>,
}

impl ConnectionSpec {
    pub fn option(&self, key: &str) -> Option<&str> {
        self.options.get(key).map(String::as_str).filter(|value| !value.is_empty())
    }
}

/// Which secret a password-like field holds. Keyring accounts are namespaced by slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SecretSlot {
    Password,
    SshPassword,
    SshKeyPassphrase,
    TlsKeyPassphrase,
    ApiKey,
    Token,
}

/// Secrets for one connect call. Never serialised.
#[derive(Clone, Default)]
pub struct ConnectSecrets {
    pub values: HashMap<SecretSlot, String>,
}

impl ConnectSecrets {
    pub fn password(password: &str) -> Self {
        let mut values = HashMap::new();
        values.insert(SecretSlot::Password, password.to_owned());
        Self { values }
    }

    pub fn get(&self, slot: SecretSlot) -> Option<&str> {
        self.values.get(&slot).map(String::as_str)
    }

    /// The password, or an empty string.
    pub fn password_or_empty(&self) -> &str {
        self.get(SecretSlot::Password).unwrap_or_default()
    }
}

impl std::fmt::Debug for ConnectSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.values.keys()).finish()
    }
}

/// Where to connect after `net::prepare` (the local end of an SSH tunnel, or the spec's host and port).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug)]
pub struct ConnectRequest {
    pub spec: ConnectionSpec,
    pub secrets: ConnectSecrets,
    pub endpoint: Endpoint,
    pub connect_timeout: Duration,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// `"MariaDB"`, `"Microsoft SQL Server"`, `"MongoDB"`.
    pub product: String,
    /// The version as the server reports it (`"11.8.2-MariaDB-ubu2404"`).
    pub version: String,
    /// For the status bar: `"MariaDB 11.8.2"`.
    pub label: String,
    /// `"opensearch"`, `"azure"`, `"replicaSet"`, …
    #[serde(default)]
    pub flavor: Option<String>,
}

/// The result of `connect` and `test_connection`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectInfo {
    pub engine: String,
    pub server: ServerInfo,
    pub capabilities: CapabilitySet,
    pub languages: Vec<LanguageSpec>,
    /// Extra entries for the connection row's menu (Redis "Server info", ES "Cluster overview").
    #[serde(default)]
    pub connection_actions: Vec<NodeAction>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TlsMode {
    Disable,
    #[default]
    Prefer,
    Require,
    VerifyCa,
    VerifyFull,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsConfig {
    #[serde(default)]
    pub mode: TlsMode,
    #[serde(default)]
    pub ca_file: Option<String>,
    #[serde(default)]
    pub client_cert_file: Option<String>,
    #[serde(default)]
    pub client_key_file: Option<String>,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub accept_invalid_certs: bool,
    #[serde(default)]
    pub pinned_sha256: Option<String>,
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum SshAuth {
    Password,
    #[serde(rename_all = "camelCase")]
    PrivateKey {
        key_file: String,
    },
    Agent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshHop {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKeyPolicy {
    #[default]
    AcceptNew,
    Strict,
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    #[serde(default)]
    pub user: String,
    pub auth: SshAuth,
    #[serde(default)]
    pub jump_hosts: Vec<SshHop>,
    #[serde(default)]
    pub host_key: HostKeyPolicy,
    #[serde(default)]
    pub keepalive_secs: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print_their_values() {
        let secrets = ConnectSecrets::password("hunter2");
        assert!(!format!("{secrets:?}").contains("hunter2"));
        assert_eq!(secrets.password_or_empty(), "hunter2");
    }

    #[test]
    fn ssh_config_defaults() {
        let ssh: SshConfig = serde_json::from_value(serde_json::json!({
            "enabled": true, "host": "bastion", "user": "me", "auth": { "method": "privateKey", "keyFile": "~/.ssh/id_ed25519" }
        }))
        .unwrap();
        assert_eq!(ssh.port, 22);
        assert_eq!(ssh.host_key, HostKeyPolicy::AcceptNew);
    }
}
