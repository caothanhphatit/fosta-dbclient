//! The engine contract's connection settings mapped onto the network layer's own types. The contract keeps its serde shape
//! (saved profiles, the TypeScript mirror); the network layer keeps its own, so the mapping is field by field here.

use super::spec::{HostKeyPolicy, NetSecrets, NetSpec, SshAuth, SshHop, SshSpec, TlsMode, TlsSpec};
use crate::contract::{self, ConnectSecrets, ConnectionSpec, SecretSlot};

impl From<contract::TlsMode> for TlsMode {
    fn from(mode: contract::TlsMode) -> Self {
        match mode {
            contract::TlsMode::Disable => TlsMode::Disable,
            contract::TlsMode::Prefer => TlsMode::Prefer,
            contract::TlsMode::Require => TlsMode::Require,
            contract::TlsMode::VerifyCa => TlsMode::VerifyCa,
            contract::TlsMode::VerifyFull => TlsMode::VerifyFull,
        }
    }
}

impl From<&contract::TlsConfig> for TlsSpec {
    fn from(tls: &contract::TlsConfig) -> Self {
        TlsSpec {
            mode: tls.mode.into(),
            ca_file: tls.ca_file.clone(),
            client_cert_file: tls.client_cert_file.clone(),
            client_key_file: tls.client_key_file.clone(),
            server_name: tls.server_name.clone(),
            accept_invalid_certs: tls.accept_invalid_certs,
            pinned_sha256: tls.pinned_sha256.clone(),
        }
    }
}

impl From<&contract::spec::SshAuth> for SshAuth {
    fn from(auth: &contract::spec::SshAuth) -> Self {
        match auth {
            contract::spec::SshAuth::Password => SshAuth::Password,
            contract::spec::SshAuth::PrivateKey { key_file } => SshAuth::PrivateKey {
                key_file: key_file.clone(),
            },
            contract::spec::SshAuth::Agent => SshAuth::Agent,
        }
    }
}

impl From<contract::spec::HostKeyPolicy> for HostKeyPolicy {
    fn from(policy: contract::spec::HostKeyPolicy) -> Self {
        match policy {
            contract::spec::HostKeyPolicy::AcceptNew => HostKeyPolicy::AcceptNew,
            contract::spec::HostKeyPolicy::Strict => HostKeyPolicy::Strict,
            contract::spec::HostKeyPolicy::Off => HostKeyPolicy::Off,
        }
    }
}

impl From<&contract::SshConfig> for SshSpec {
    /// A jump host has no auth of its own in the contract: it authenticates like the target (same secret slots).
    fn from(ssh: &contract::SshConfig) -> Self {
        let auth = SshAuth::from(&ssh.auth);
        SshSpec {
            enabled: ssh.enabled,
            host: ssh.host.clone(),
            port: ssh.port,
            user: ssh.user.clone(),
            auth: auth.clone(),
            jump_hosts: ssh
                .jump_hosts
                .iter()
                .map(|hop| SshHop {
                    host: hop.host.clone(),
                    port: hop.port,
                    user: hop.user.clone(),
                    auth: auth.clone(),
                })
                .collect(),
            host_key: ssh.host_key.into(),
            keepalive_secs: ssh.keepalive_secs,
        }
    }
}

impl From<&ConnectionSpec> for NetSpec {
    fn from(spec: &ConnectionSpec) -> Self {
        NetSpec {
            ssh: spec.ssh.as_ref().map(SshSpec::from),
            tls: spec.tls.as_ref().map(TlsSpec::from),
        }
    }
}

impl From<&ConnectSecrets> for NetSecrets {
    fn from(secrets: &ConnectSecrets) -> Self {
        let slot = |slot: SecretSlot| secrets.get(slot).filter(|value| !value.is_empty()).map(str::to_owned);
        NetSecrets {
            ssh_password: slot(SecretSlot::SshPassword),
            ssh_key_passphrase: slot(SecretSlot::SshKeyPassphrase),
            tls_key_passphrase: slot(SecretSlot::TlsKeyPassphrase),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{SshConfig, TlsConfig};

    #[test]
    fn contract_settings_map_field_by_field() {
        let spec = ConnectionSpec {
            engine: "mariadb".into(),
            host: "db.internal".into(),
            port: 3306,
            ssh: Some(SshConfig {
                enabled: true,
                host: "bastion".into(),
                port: 2222,
                user: "deploy".into(),
                auth: contract::spec::SshAuth::PrivateKey {
                    key_file: "~/.ssh/id_ed25519".into(),
                },
                jump_hosts: vec![contract::spec::SshHop {
                    host: "jump".into(),
                    port: 22,
                    user: "ops".into(),
                }],
                host_key: contract::spec::HostKeyPolicy::Strict,
                keepalive_secs: Some(10),
            }),
            tls: Some(TlsConfig {
                mode: contract::TlsMode::VerifyCa,
                ca_file: Some("ca.pem".into()),
                server_name: Some("db.example.com".into()),
                pinned_sha256: Some("ab:cd".into()),
                ..TlsConfig::default()
            }),
            ..ConnectionSpec::default()
        };
        let net = NetSpec::from(&spec);
        let ssh = net.ssh.unwrap();
        assert!(ssh.enabled);
        assert_eq!((ssh.host.as_str(), ssh.port, ssh.user.as_str()), ("bastion", 2222, "deploy"));
        assert_eq!(ssh.host_key, HostKeyPolicy::Strict);
        assert_eq!(ssh.keepalive_secs, Some(10));
        assert_eq!(ssh.jump_hosts[0].auth, ssh.auth, "a hop authenticates like the target");
        let tls = net.tls.unwrap();
        assert_eq!(tls.mode, TlsMode::VerifyCa);
        assert_eq!(tls.ca_file.as_deref(), Some("ca.pem"));
        assert_eq!(tls.server_name.as_deref(), Some("db.example.com"));
        assert_eq!(tls.pinned_sha256.as_deref(), Some("ab:cd"));
    }

    #[test]
    fn secrets_come_from_their_slots() {
        let mut secrets = ConnectSecrets::password("db-pass");
        secrets.values.insert(SecretSlot::SshPassword, "ssh-pass".into());
        secrets.values.insert(SecretSlot::SshKeyPassphrase, String::new());
        let net = NetSecrets::from(&secrets);
        assert_eq!(net.ssh_password.as_deref(), Some("ssh-pass"));
        assert_eq!(net.ssh_key_passphrase, None, "an empty secret counts as none");
        assert_eq!(net.tls_key_passphrase, None);
    }
}
