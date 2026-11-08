//! The network part of a connection profile: an optional SSH tunnel and optional TLS settings.
//!
//! Field names match `TlsConfig` / `SshConfig` of the engine contract (`contract/spec.rs`), so the contract
//! types can become aliases of these (or convert field by field) when the two are wired together.
//!
//! Nothing here holds a secret. Passwords and passphrases live in the secret store and reach the network
//! layer at connect time through [`NetSecrets`]; the spec only says which secret slot a method needs.

use std::fmt;

use serde::{Deserialize, Serialize};

/// SSH tunnel and TLS settings of one connection. Both parts are optional and independent: TLS runs end to
/// end through the tunnel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsSpec>,
}

// ---- TLS ----

/// How strictly TLS is used. Fosta is secure by default: `require` verifies the certificate chain and the
/// host name, unlike libpq's `require`. Skipping verification takes an explicit
/// [`TlsSpec::accept_invalid_certs`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TlsMode {
    /// Plain TCP.
    #[default]
    Disable,
    /// TLS when the server offers it, else plain TCP (engines with a STARTTLS-style negotiation only).
    /// When TLS is used it is verified like `require`.
    Prefer,
    /// TLS is mandatory; chain and host name are verified.
    Require,
    /// TLS is mandatory; the chain is verified, the host name is not.
    VerifyCa,
    /// TLS is mandatory; chain and host name are verified (same checks as `require`; kept for libpq parity).
    VerifyFull,
}

/// TLS settings. File fields are paths to PEM files on the machine running the backend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsSpec {
    #[serde(default)]
    pub mode: TlsMode,
    /// CA certificate(s) to trust instead of the system and Mozilla roots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<String>,
    /// Client certificate chain for mutual TLS (needs `client_key_file`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert_file: Option<String>,
    /// Unencrypted PKCS#8, PKCS#1 or SEC1 private key of the client certificate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key_file: Option<String>,
    /// Name sent as SNI and checked against the certificate instead of the connect host. Needed when the
    /// connect host is an IP or a tunnel's `127.0.0.1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    /// Accept any certificate (handshake signatures are still checked). For local and test servers only.
    #[serde(default)]
    pub accept_invalid_certs: bool,
    /// SHA-256 fingerprint of a certificate in the server's chain (hex, colons optional, or base64). When set
    /// it replaces chain verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_sha256: Option<String>,
}

impl TlsSpec {
    /// Whether the connection uses TLS at all.
    pub fn is_enabled(&self) -> bool {
        self.mode != TlsMode::Disable
    }
}

// ---- SSH ----

fn default_ssh_port() -> u16 {
    22
}

fn yes() -> bool {
    true
}

/// An SSH tunnel: the engine connects to a local port that is forwarded through `host` to the database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSpec {
    /// Saved but switched off tunnels are ignored.
    #[serde(default = "yes")]
    pub enabled: bool,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    /// Bastions to go through first (`ProxyJump`). One hop is supported.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jump_hosts: Vec<SshHop>,
    #[serde(default)]
    pub host_key: HostKeyPolicy,
    /// Seconds between keepalives when the server is silent (default 30, 0 turns them off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keepalive_secs: Option<u32>,
}

/// One `ProxyJump` hop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshHop {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
}

/// How to authenticate to an SSH server. Secrets come from [`NetSecrets`]:
/// `password` uses the `sshPassword` slot, `privateKey` the `sshKeyPassphrase` slot when the key is
/// encrypted. A jump host uses the same slots as the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SshAuth {
    Password,
    /// An OpenSSH or PEM private key file (`~` is expanded).
    PrivateKey {
        key_file: String,
    },
    /// The running SSH agent: `SSH_AUTH_SOCK` on Unix; on Windows the OpenSSH agent pipe, then Pageant.
    Agent,
}

/// What to do with the SSH server's host key. Known keys come from the app's known_hosts file and, read
/// only, from `~/.ssh/known_hosts`. A key that differs from the recorded one is always refused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKeyPolicy {
    /// Trust on first use: a host seen for the first time is recorded and accepted (OpenSSH
    /// `StrictHostKeyChecking=accept-new`); the tunnel reports the recorded fingerprint.
    #[default]
    AcceptNew,
    /// An unknown host fails with `SSH_HOST_KEY_UNKNOWN` carrying the fingerprint, so the UI can ask the user
    /// and call [`crate::net::ssh::trust_host_key`].
    Strict,
    /// No check at all. For throwaway test servers only.
    Off,
}

// ---- Secrets ----

/// The secrets the network layer may need, read from the secret store by the caller (slots
/// `sshPassword`, `sshKeyPassphrase`, `tlsKeyPassphrase`). Never serialised; `Debug` redacts them.
#[derive(Clone, Default)]
pub struct NetSecrets {
    pub ssh_password: Option<String>,
    pub ssh_key_passphrase: Option<String>,
    pub tls_key_passphrase: Option<String>,
}

impl fmt::Debug for NetSecrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = |value: &Option<String>| if value.is_some() { "<set>" } else { "<none>" };
        formatter
            .debug_struct("NetSecrets")
            .field("ssh_password", &mark(&self.ssh_password))
            .field("ssh_key_passphrase", &mark(&self.ssh_key_passphrase))
            .field("tls_key_passphrase", &mark(&self.tls_key_passphrase))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_round_trip_with_camel_case_names() {
        let spec = NetSpec {
            ssh: Some(SshSpec {
                enabled: true,
                host: "bastion.example.com".into(),
                port: 2222,
                user: "deploy".into(),
                auth: SshAuth::PrivateKey {
                    key_file: "~/.ssh/id_ed25519".into(),
                },
                jump_hosts: vec![SshHop {
                    host: "jump.example.com".into(),
                    port: 22,
                    user: "ops".into(),
                    auth: SshAuth::Agent,
                }],
                host_key: HostKeyPolicy::Strict,
                keepalive_secs: Some(15),
            }),
            tls: Some(TlsSpec {
                mode: TlsMode::VerifyCa,
                ca_file: Some("ca.pem".into()),
                accept_invalid_certs: false,
                pinned_sha256: Some("ab:cd".into()),
                ..TlsSpec::default()
            }),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["ssh"]["auth"]["kind"], "privateKey");
        assert_eq!(json["ssh"]["auth"]["keyFile"], "~/.ssh/id_ed25519");
        assert_eq!(json["ssh"]["jumpHosts"][0]["auth"]["kind"], "agent");
        assert_eq!(json["ssh"]["hostKey"], "strict");
        assert_eq!(json["tls"]["mode"], "verifyCa");
        assert_eq!(json["tls"]["pinnedSha256"], "ab:cd");
        assert!(json["tls"].get("clientCertFile").is_none());
        let back: NetSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn minimal_ssh_spec_gets_defaults() {
        let spec: SshSpec = serde_json::from_str(r#"{"host":"h","user":"u","auth":{"kind":"password"}}"#).unwrap();
        assert!(spec.enabled);
        assert_eq!(spec.port, 22);
        assert_eq!(spec.host_key, HostKeyPolicy::AcceptNew);
        assert!(spec.jump_hosts.is_empty());
        let tls: TlsSpec = serde_json::from_str("{}").unwrap();
        assert!(!tls.is_enabled());
    }

    #[test]
    fn secrets_never_show_in_debug_output() {
        let secrets = NetSecrets {
            ssh_password: Some("hunter2".into()),
            ..NetSecrets::default()
        };
        let text = format!("{secrets:?}");
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("<set>"));
    }
}
