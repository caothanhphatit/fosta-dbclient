//! Network helpers shared by the engines: the SSH tunnel in front of a connection and rustls client configs built from a
//! profile's [`TlsConfig`](crate::contract::TlsConfig). The signatures here are fixed; the SSH implementation and the
//! per-engine TLS mapping are filled in by the SSH/TLS work (`net/**`, `engines/{mysql,mssql,mongo}/connect.rs`).

pub mod ssh;
pub mod tls;

use crate::{
    contract::{ConnectSecrets, ConnectionSpec, Endpoint},
    error::AppResult,
};

/// Keeps an SSH tunnel open; dropping it closes the tunnel.
pub struct TunnelGuard {
    _private: (),
}

/// Where the engine connects after any tunnel is set up.
pub struct PreparedEndpoint {
    pub endpoint: Endpoint,
    pub tunnel: Option<TunnelGuard>,
}

/// Opens the SSH tunnel when the spec asks for one; otherwise the spec's own host and port.
pub async fn prepare(spec: &ConnectionSpec, secrets: &ConnectSecrets) -> AppResult<PreparedEndpoint> {
    if let Some(ssh) = spec.ssh.as_ref().filter(|ssh| ssh.enabled) {
        return ssh::open_tunnel(ssh, spec, secrets).await;
    }
    Ok(PreparedEndpoint {
        endpoint: Endpoint {
            host: spec.host.clone(),
            port: spec.port,
        },
        tunnel: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{spec::SshAuth, SshConfig};

    #[tokio::test]
    async fn without_ssh_the_endpoint_is_the_spec_and_ssh_is_refused_for_now() {
        let mut spec = ConnectionSpec {
            engine: "mariadb".into(),
            host: "db".into(),
            port: 3307,
            ..ConnectionSpec::default()
        };
        let prepared = prepare(&spec, &ConnectSecrets::default()).await.unwrap();
        assert_eq!(
            prepared.endpoint,
            Endpoint {
                host: "db".into(),
                port: 3307
            }
        );
        spec.ssh = Some(SshConfig {
            enabled: true,
            host: "bastion".into(),
            port: 22,
            user: "me".into(),
            auth: SshAuth::Agent,
            jump_hosts: Vec::new(),
            host_key: Default::default(),
            keepalive_secs: None,
        });
        assert!(prepare(&spec, &ConnectSecrets::default()).await.is_err());
    }
}
