//! The network layer shared by all engines: SSH tunnels ([`ssh`]) and TLS configuration ([`tls`]).
//!
//! The connection manager calls [`prepare`] before an engine connects. Without SSH it returns the
//! configured endpoint; with SSH it opens a tunnel and returns `127.0.0.1:<port>` plus a [`TunnelGuard`]
//! that the manager keeps next to the connection and drops when the connection closes. Engines build
//! their TLS settings with [`tls::rustls_client_config`] (or [`tls::native_tls_options`]) and use
//! [`PreparedEndpoint::tls_server_name`] as the SNI / verification name, so TLS through a tunnel still
//! checks the database's real host name.
//!
//! See `docs/engines/ssh-tls.md`.

mod contract;
pub mod spec;
pub mod ssh;
pub mod tls;

#[cfg(test)]
mod live_tests;

use std::path::PathBuf;
use std::time::Duration;

pub use spec::{HostKeyPolicy, NetSecrets, NetSpec, SshAuth, SshHop, SshSpec, TlsMode, TlsSpec};
pub use ssh::{HostKeyInfo, TunnelGuard, TunnelStatus};

use crate::error::AppResult;

/// A host and port to connect to (the contract's type, so the manager hands it to engines as is).
pub use crate::contract::Endpoint;

/// Where the engine should connect, after the tunnel (if any) is up.
#[derive(Debug)]
pub struct PreparedEndpoint {
    /// The configured endpoint, or `127.0.0.1:<tunnel port>`.
    pub endpoint: Endpoint,
    /// The name TLS must send and verify: `tls.server_name` when set, else the configured database host
    /// (never the tunnel's `127.0.0.1`).
    pub tls_server_name: String,
    /// Keep it as long as the connection lives; dropping it closes the tunnel.
    pub tunnel: Option<TunnelGuard>,
}

/// Host-side settings of the network layer, built once by the app.
#[derive(Debug, Clone)]
pub struct NetContext {
    /// The app's known_hosts file (read and written), e.g. `<data dir>/known_hosts`.
    pub known_hosts: PathBuf,
    /// Read-only extra known_hosts file, normally `~/.ssh/known_hosts`.
    pub user_known_hosts: Option<PathBuf>,
    /// Limit for TCP connect + SSH handshake + authentication (per hop chain, per attempt).
    pub connect_timeout: Duration,
}

impl NetContext {
    /// A context with the app's known_hosts file, the user's `~/.ssh/known_hosts` and a 15 s timeout.
    pub fn new(known_hosts: impl Into<PathBuf>) -> Self {
        Self {
            known_hosts: known_hosts.into(),
            user_known_hosts: home_dir().map(|home| home.join(".ssh").join("known_hosts")),
            connect_timeout: Duration::from_secs(15),
        }
    }
}

/// The user's home directory (`HOME`, else `USERPROFILE`).
pub(crate) fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Opens the SSH tunnel of `net` (when enabled) towards `target` and says where the engine connects.
/// `target` is the database as seen from the SSH server (often `127.0.0.1:3306` on the server itself).
pub async fn prepare(net: &NetSpec, target: &Endpoint, secrets: &NetSecrets, ctx: &NetContext) -> AppResult<PreparedEndpoint> {
    let tls_server_name = net
        .tls
        .as_ref()
        .and_then(|tls| tls.server_name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&target.host)
        .to_string();
    match net.ssh.as_ref().filter(|ssh| ssh.enabled) {
        None => Ok(PreparedEndpoint {
            endpoint: target.clone(),
            tls_server_name,
            tunnel: None,
        }),
        Some(ssh) => {
            let tunnel = ssh::open_tunnel(ssh, target, secrets, ctx).await?;
            Ok(PreparedEndpoint {
                endpoint: Endpoint {
                    host: "127.0.0.1".into(),
                    port: tunnel.local_port(),
                },
                tls_server_name,
                tunnel: Some(tunnel),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn without_ssh_the_endpoint_is_unchanged() {
        let target = Endpoint {
            host: "db.internal".into(),
            port: 5432,
        };
        let net = NetSpec {
            ssh: Some(SshSpec {
                enabled: false,
                host: "bastion".into(),
                port: 22,
                user: "u".into(),
                auth: SshAuth::Agent,
                jump_hosts: Vec::new(),
                host_key: HostKeyPolicy::AcceptNew,
                keepalive_secs: None,
            }),
            tls: Some(TlsSpec {
                mode: TlsMode::Require,
                server_name: Some("db.example.com".into()),
                ..TlsSpec::default()
            }),
        };
        let prepared = prepare(&net, &target, &NetSecrets::default(), &NetContext::new("unused"))
            .await
            .unwrap();
        assert_eq!(prepared.endpoint, target);
        assert_eq!(prepared.tls_server_name, "db.example.com");
        assert!(prepared.tunnel.is_none());
        let plain = prepare(&NetSpec::default(), &target, &NetSecrets::default(), &NetContext::new("unused"))
            .await
            .unwrap();
        assert_eq!(plain.tls_server_name, "db.internal");
    }
}
