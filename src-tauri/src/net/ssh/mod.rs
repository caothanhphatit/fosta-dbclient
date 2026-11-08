//! SSH local port forwarding: one tunnel per connection.
//!
//! [`open_tunnel`] opens and authenticates the SSH session up front (so mistakes surface at connect time
//! with a structured error), binds `127.0.0.1:0` and forwards every accepted socket through a
//! `direct-tcpip` channel to the target. The returned [`TunnelGuard`] owns all of it: dropping the guard
//! stops the listener, the forwards and the session.
//!
//! When the session dies (network change, server restart, keepalive timeout), the next forwarded socket
//! reopens it, retrying with exponential backoff. Authentication and host key failures are not retried.
//!
//! Errors are `AppError::structured` with a `code` (the `SSH_*` constants below), a developer-facing
//! `message`, and `host`/`port`/`role` (`"SSH server"` or `"jump host"`) where it applies.

mod host_keys;
mod session;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

pub use host_keys::{forget_host_key, trust_host_key, HostKeyInfo, KnownHosts};
use session::{establish, Link, Plan};

use super::spec::{NetSecrets, SshSpec};
use super::{Endpoint, NetContext};
use crate::error::{AppError, AppResult};

/// The server rejected the credentials (`detail.serverMethods` lists what it accepts).
pub const SSH_AUTH_FAILED: &str = "SSH_AUTH_FAILED";
/// The host is not in known_hosts (strict policy, or keys of another type are recorded). `detail.hostKey` is
/// a [`HostKeyInfo`] for a trust prompt; on "trust", call [`trust_host_key`] and connect again.
pub const SSH_HOST_KEY_UNKNOWN: &str = "SSH_HOST_KEY_UNKNOWN";
/// The host presented a different key than the recorded one. Never accepted automatically.
pub const SSH_HOST_KEY_MISMATCH: &str = "SSH_HOST_KEY_MISMATCH";
/// TCP connect failed, the connection dropped during the handshake, or the jump host could not reach the
/// target.
pub const SSH_UNREACHABLE: &str = "SSH_UNREACHABLE";
/// No answer within the connect timeout, or keepalives went unanswered.
pub const SSH_TIMEOUT: &str = "SSH_TIMEOUT";
/// The private key file is missing, unreadable, malformed, or the passphrase is wrong.
pub const SSH_KEY_FILE: &str = "SSH_KEY_FILE";
/// No SSH agent is reachable, or it holds no keys.
pub const SSH_AGENT: &str = "SSH_AGENT";
/// A secret is missing; `detail.slot` names it (`sshPassword` or `sshKeyPassphrase`). The UI asks for it
/// and connects again, like `PASSWORD_REQUIRED`.
pub const SSH_SECRET_REQUIRED: &str = "SSH_SECRET_REQUIRED";
/// The SSH server refused to open a forward to the database (`AllowTcpForwarding no`, or it cannot reach
/// the database host).
pub const SSH_FORWARD_FAILED: &str = "SSH_FORWARD_FAILED";
/// SSH protocol trouble: no SSH greeting, no common algorithm, unexpected errors.
pub const SSH_PROTOCOL: &str = "SSH_PROTOCOL";
/// The tunnel settings are incomplete, or a known_hosts file could not be read or written.
pub const SSH_CONFIG: &str = "SSH_CONFIG";

const DEFAULT_KEEPALIVE_SECS: u32 = 30;
const RECONNECT_ATTEMPTS: u32 = 4;
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(8);

pub(crate) fn ssh_error(code: &str, message: impl Into<String>, extra: serde_json::Value) -> AppError {
    let message = message.into();
    let mut detail = match extra {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    detail.insert("code".into(), code.into());
    detail.insert("message".into(), message.clone().into());
    AppError::structured(message, serde_json::Value::Object(detail))
}

fn secret_required(slot: &str, message: String, peer: &session::Peer) -> AppError {
    let mut detail = serde_json::json!({ "slot": slot });
    detail["host"] = peer.host.clone().into();
    detail["port"] = peer.port.into();
    detail["role"] = peer.role.into();
    ssh_error(SSH_SECRET_REQUIRED, message, detail)
}

/// Failures that a retry cannot fix.
fn is_permanent(error: &AppError) -> bool {
    matches!(
        error.code(),
        Some(SSH_AUTH_FAILED | SSH_HOST_KEY_UNKNOWN | SSH_HOST_KEY_MISMATCH | SSH_KEY_FILE | SSH_AGENT | SSH_SECRET_REQUIRED | SSH_CONFIG)
    )
}

/// A snapshot of a tunnel's health, for logs and the UI.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatus {
    /// The SSH session is up.
    pub connected: bool,
    /// Sessions reopened after the first one.
    pub reconnects: u32,
    /// Sockets forwarded so far.
    pub forwarded: u64,
    /// The latest failure (reconnect, forward refused), if any.
    pub last_error: Option<String>,
}

struct Shared {
    plan: Plan,
    target: Endpoint,
    link: tokio::sync::Mutex<Option<Arc<Link>>>,
    status: Mutex<TunnelStatus>,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut TunnelStatus)) {
        change(&mut self.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
    }

    /// The live session, reopened with backoff when it died.
    async fn link(&self) -> AppResult<Arc<Link>> {
        let mut slot = self.link.lock().await;
        if let Some(link) = slot.as_ref() {
            if !link.session.is_closed() {
                return Ok(link.clone());
            }
        }
        *slot = None;
        self.update(|status| status.connected = false);
        let mut delay = INITIAL_BACKOFF;
        let mut attempt = 0;
        loop {
            attempt += 1;
            match establish(&self.plan).await {
                Ok(link) => {
                    let link = Arc::new(link);
                    *slot = Some(link.clone());
                    self.update(|status| {
                        status.connected = true;
                        status.reconnects += 1;
                    });
                    return Ok(link);
                }
                Err(error) => {
                    self.update(|status| status.last_error = Some(error.to_string()));
                    if is_permanent(&error) || attempt >= RECONNECT_ATTEMPTS {
                        return Err(error);
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    async fn open_channel(&self, peer: SocketAddr) -> AppResult<russh::Channel<russh::client::Msg>> {
        let mut retried = false;
        loop {
            let link = self.link().await?;
            let opened = link
                .session
                .channel_open_direct_tcpip(
                    self.target.host.clone(),
                    self.target.port.into(),
                    peer.ip().to_string(),
                    peer.port().into(),
                )
                .await;
            match opened {
                Ok(channel) => return Ok(channel),
                Err(russh::Error::ChannelOpenFailure(reason)) => {
                    return Err(ssh_error(
                        SSH_FORWARD_FAILED,
                        format!(
                            "The SSH server {}:{} refused to forward to {}:{} ({reason:?}). Check that the database \
                             host is reachable from the SSH server and that sshd allows TCP forwarding \
                             (AllowTcpForwarding).",
                            self.plan.ssh.host, self.plan.ssh.port, self.target.host, self.target.port
                        ),
                        serde_json::json!({ "host": self.target.host, "port": self.target.port }),
                    ))
                }
                // The session died between the liveness check and the request: reopen it once.
                Err(_) if !retried => {
                    retried = true;
                    let mut slot = self.link.lock().await;
                    if slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, &link)) {
                        *slot = None;
                    }
                }
                Err(error) => {
                    return Err(ssh_error(
                        SSH_UNREACHABLE,
                        format!("The SSH session to {}:{} broke: {error}", self.plan.ssh.host, self.plan.ssh.port),
                        serde_json::json!({ "host": self.plan.ssh.host, "port": self.plan.ssh.port }),
                    ))
                }
            }
        }
    }

    async fn forward(&self, mut socket: TcpStream, peer: SocketAddr) -> AppResult<()> {
        let channel = self.open_channel(peer).await?;
        self.update(|status| status.forwarded += 1);
        let _ = socket.set_nodelay(true);
        let mut stream = channel.into_stream();
        // Resets and EOFs are how database connections end; they are not tunnel errors.
        let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
        Ok(())
    }
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    // Dropping the set (when this task is aborted) aborts every forward.
    let mut forwards = JoinSet::new();
    loop {
        match listener.accept().await {
            Ok((socket, peer)) => {
                while forwards.try_join_next().is_some() {}
                let shared = shared.clone();
                forwards.spawn(async move {
                    if let Err(error) = shared.forward(socket, peer).await {
                        shared.update(|status| status.last_error = Some(error.to_string()));
                    }
                });
            }
            Err(error) => {
                shared.update(|status| status.last_error = Some(format!("Local tunnel listener: {error}")));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// A running tunnel. Connect the engine to [`TunnelGuard::local_addr`]; drop the guard (or call
/// [`TunnelGuard::close`]) when the connection closes.
pub struct TunnelGuard {
    local_addr: SocketAddr,
    shared: Arc<Shared>,
    accept: JoinHandle<()>,
    learned: Vec<HostKeyInfo>,
}

impl std::fmt::Debug for TunnelGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TunnelGuard")
            .field("local_addr", &self.local_addr)
            .field(
                "ssh",
                &format_args!(
                    "{}@{}:{}",
                    self.shared.plan.ssh.user, self.shared.plan.ssh.host, self.shared.plan.ssh.port
                ),
            )
            .field("target", &self.shared.target)
            .finish()
    }
}

impl TunnelGuard {
    /// `127.0.0.1:<ephemeral port>`.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn local_port(&self) -> u16 {
        self.local_addr.port()
    }

    /// Host keys recorded on first use while opening the tunnel; show them so the user can compare
    /// fingerprints ("Added SHA256:… for bastion:22 to known hosts").
    pub fn learned_host_keys(&self) -> &[HostKeyInfo] {
        &self.learned
    }

    pub fn status(&self) -> TunnelStatus {
        self.shared.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// The latest tunnel failure. When an engine's connect fails with a generic "connection closed", this
    /// usually says why (e.g. `SSH_FORWARD_FAILED` text).
    pub fn last_error(&self) -> Option<String> {
        self.status().last_error
    }

    /// Disconnects the SSH session under the tunnel, as a network drop would.
    #[cfg(test)]
    pub(crate) async fn break_session_for_test(&self) {
        let link = self.shared.link.lock().await.clone();
        if let Some(link) = link {
            let _ = link.session.disconnect(russh::Disconnect::ByApplication, "test", "en").await;
            for _ in 0..100 {
                if link.session.is_closed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    /// Stops forwarding and disconnects the SSH session politely.
    pub async fn close(self) {
        self.accept.abort();
        let link = self.shared.link.lock().await.take();
        if let Some(link) = link {
            let _ = link
                .session
                .disconnect(russh::Disconnect::ByApplication, "connection closed", "en")
                .await;
        }
    }
}

impl Drop for TunnelGuard {
    fn drop(&mut self) {
        self.accept.abort();
        // Releasing the last handle ends the session task, which closes the TCP connection.
        if let Ok(mut slot) = self.shared.link.try_lock() {
            slot.take();
        }
    }
}

/// Opens a tunnel through `ssh` to `target` (host and port as seen from the SSH server). Fails with a
/// structured `SSH_*` error when the session cannot be opened or authenticated.
pub async fn open_tunnel(ssh: &SshSpec, target: &Endpoint, secrets: &NetSecrets, ctx: &NetContext) -> AppResult<TunnelGuard> {
    let plan = Plan {
        ssh: ssh.clone(),
        secrets: secrets.clone(),
        known: Arc::new(KnownHosts {
            app_file: ctx.known_hosts.clone(),
            user_file: ctx.user_known_hosts.clone(),
        }),
        connect_timeout: ctx.connect_timeout,
    };
    let link = establish(&plan).await?;
    let learned = link.learned.clone();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|error| {
        ssh_error(
            SSH_CONFIG,
            format!("Could not open a local port for the SSH tunnel: {error}"),
            serde_json::json!({}),
        )
    })?;
    let local_addr = listener.local_addr().map_err(|error| {
        ssh_error(
            SSH_CONFIG,
            format!("Could not read the tunnel's local port: {error}"),
            serde_json::json!({}),
        )
    })?;
    let shared = Arc::new(Shared {
        plan,
        target: target.clone(),
        link: tokio::sync::Mutex::new(Some(Arc::new(link))),
        status: Mutex::new(TunnelStatus {
            connected: true,
            ..TunnelStatus::default()
        }),
    });
    let accept = tokio::spawn(accept_loop(listener, shared.clone()));
    Ok(TunnelGuard {
        local_addr,
        shared,
        accept,
        learned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::spec::{HostKeyPolicy, SshAuth};

    fn spec(host: &str, port: u16) -> SshSpec {
        SshSpec {
            enabled: true,
            host: host.into(),
            port,
            user: "tester".into(),
            auth: SshAuth::Password,
            jump_hosts: Vec::new(),
            host_key: HostKeyPolicy::Strict,
            keepalive_secs: None,
        }
    }

    fn ctx() -> NetContext {
        NetContext {
            known_hosts: std::env::temp_dir().join("fosta-net-unit-known_hosts"),
            user_known_hosts: None,
            connect_timeout: Duration::from_secs(5),
        }
    }

    fn target() -> Endpoint {
        Endpoint {
            host: "db".into(),
            port: 3306,
        }
    }

    #[tokio::test]
    async fn a_closed_port_is_reported_as_unreachable() {
        // Bind and drop to find a port nobody listens on.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let error = open_tunnel(&spec("127.0.0.1", port), &target(), &NetSecrets::default(), &ctx())
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some(SSH_UNREACHABLE), "{error}");
        assert_eq!(error.payload().unwrap()["port"], port);
        assert!(error.to_string().contains("SSH server 127.0.0.1"), "{error}");
    }

    #[tokio::test]
    async fn a_non_ssh_server_is_a_protocol_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut socket, _)) = listener.accept().await {
                let _ = socket.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            }
        });
        let error = open_tunnel(&spec("127.0.0.1", port), &target(), &NetSecrets::default(), &ctx())
            .await
            .unwrap_err();
        assert!(
            matches!(error.code(), Some(SSH_PROTOCOL) | Some(SSH_UNREACHABLE)),
            "{:?}: {error}",
            error.code()
        );
    }

    #[tokio::test]
    async fn a_silent_server_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _keep = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let context = NetContext {
            connect_timeout: Duration::from_millis(300),
            ..ctx()
        };
        let error = open_tunnel(&spec("127.0.0.1", port), &target(), &NetSecrets::default(), &context)
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some(SSH_TIMEOUT), "{error}");
    }

    #[tokio::test]
    async fn incomplete_settings_are_rejected_before_connecting() {
        let mut two_hops = spec("bastion", 22);
        let hop = crate::net::spec::SshHop {
            host: "j".into(),
            port: 22,
            user: "u".into(),
            auth: SshAuth::Agent,
        };
        two_hops.jump_hosts = vec![hop.clone(), hop];
        let error = open_tunnel(&two_hops, &target(), &NetSecrets::default(), &ctx()).await.unwrap_err();
        assert_eq!(error.code(), Some(SSH_CONFIG));
        let error = open_tunnel(&spec(" ", 22), &target(), &NetSecrets::default(), &ctx())
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some(SSH_CONFIG));
    }

    #[test]
    fn permanent_failures_are_not_retried() {
        let error = |code| ssh_error(code, "x", serde_json::json!({}));
        assert!(is_permanent(&error(SSH_AUTH_FAILED)));
        assert!(is_permanent(&error(SSH_HOST_KEY_MISMATCH)));
        assert!(!is_permanent(&error(SSH_UNREACHABLE)));
        assert!(!is_permanent(&error(SSH_TIMEOUT)));
        let detailed = ssh_error(SSH_UNREACHABLE, "boom", serde_json::json!({ "host": "h" }));
        assert_eq!(detailed.payload().unwrap()["host"], "h");
        assert_eq!(detailed.message(), "boom");
    }
}
