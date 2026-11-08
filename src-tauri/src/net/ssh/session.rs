//! Opening one authenticated SSH session, directly or through a jump host.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, AuthResult, Handle};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use tokio::io::{AsyncRead, AsyncWrite};

use super::host_keys::{HostKeyInfo, KnownHosts, Verdict};
use super::*;
use crate::error::{AppError, AppResult};
use crate::net::spec::{HostKeyPolicy, NetSecrets, SshAuth, SshSpec};

/// Everything needed to (re)open the session; owned by the tunnel.
pub(crate) struct Plan {
    pub ssh: SshSpec,
    pub secrets: NetSecrets,
    pub known: Arc<KnownHosts>,
    pub connect_timeout: Duration,
}

/// An authenticated session. Field order matters: the target session is dropped before the jump session
/// that carries it.
pub(crate) struct Link {
    pub session: Handle<ClientHandler>,
    _jump: Option<Handle<ClientHandler>>,
    /// Host keys recorded while opening this link (trust on first use).
    pub learned: Vec<HostKeyInfo>,
}

/// Which server a message is about.
#[derive(Debug, Clone)]
pub(crate) struct Peer {
    pub role: &'static str,
    pub host: String,
    pub port: u16,
}

impl Peer {
    fn label(&self) -> String {
        format!("{} {}:{}", self.role, self.host, self.port)
    }

    fn detail(&self) -> serde_json::Value {
        serde_json::json!({ "host": self.host, "port": self.port, "role": self.role })
    }
}

pub(crate) struct ClientHandler {
    peer: Peer,
    policy: HostKeyPolicy,
    known: Arc<KnownHosts>,
    verdict: Arc<Mutex<Option<Verdict>>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            // Host certificates are checked by their key; certificate authorities are not supported.
            PublicKeyOrCertificate::Certificate(certificate) => PublicKey::new(certificate.public_key().clone(), ""),
        };
        let verdict = match self.policy {
            HostKeyPolicy::Off => Verdict::Unchecked,
            policy => {
                let known = self.known.clone();
                let (host, port) = (self.peer.host.clone(), self.peer.port);
                let accept_new = policy == HostKeyPolicy::AcceptNew;
                tokio::task::spawn_blocking(move || host_keys::verify(&known, &host, port, &key, accept_new))
                    .await
                    .unwrap_or_else(|error| Verdict::Failed(error.to_string()))
            }
        };
        let accepted = verdict.accepted();
        *self.verdict.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(verdict);
        Ok(accepted)
    }
}

/// Opens and authenticates the session described by `plan`, within its connect timeout.
pub(crate) async fn establish(plan: &Plan) -> AppResult<Link> {
    let ssh = &plan.ssh;
    if ssh.host.trim().is_empty() || ssh.user.trim().is_empty() {
        return Err(ssh_error(
            SSH_CONFIG,
            "The SSH tunnel needs a host and a user name.",
            serde_json::json!({}),
        ));
    }
    if ssh.jump_hosts.len() > 1 {
        return Err(ssh_error(
            SSH_CONFIG,
            "Only one jump host is supported; chain further hops on the bastion itself.",
            serde_json::json!({}),
        ));
    }
    let keepalive = ssh.keepalive_secs.unwrap_or(DEFAULT_KEEPALIVE_SECS);
    let config = Arc::new(client::Config {
        keepalive_interval: (keepalive > 0).then(|| Duration::from_secs(keepalive.into())),
        keepalive_max: 3,
        inactivity_timeout: None,
        nodelay: true,
        ..client::Config::default()
    });
    let target = Peer {
        role: "SSH server",
        host: ssh.host.trim().to_string(),
        port: ssh.port,
    };
    let work = async {
        let mut learned = Vec::new();
        match ssh.jump_hosts.first() {
            None => {
                let stream = tcp_connect(&target).await?;
                let mut session = handshake(config, stream, &target, plan, &mut learned).await?;
                authenticate(&mut session, &ssh.user, &ssh.auth, &plan.secrets, &target).await?;
                Ok(Link {
                    session,
                    _jump: None,
                    learned,
                })
            }
            Some(hop) => {
                let jump_peer = Peer {
                    role: "jump host",
                    host: hop.host.trim().to_string(),
                    port: hop.port,
                };
                let stream = tcp_connect(&jump_peer).await?;
                let mut jump = handshake(config.clone(), stream, &jump_peer, plan, &mut learned).await?;
                authenticate(&mut jump, &hop.user, &hop.auth, &plan.secrets, &jump_peer).await?;
                let channel = jump
                    .channel_open_direct_tcpip(target.host.clone(), target.port.into(), "127.0.0.1", 0)
                    .await
                    .map_err(|error| {
                        ssh_error(
                            SSH_UNREACHABLE,
                            format!("The {} could not reach the {}: {error}", jump_peer.label(), target.label()),
                            target.detail(),
                        )
                    })?;
                let mut session = handshake(config, channel.into_stream(), &target, plan, &mut learned).await?;
                authenticate(&mut session, &ssh.user, &ssh.auth, &plan.secrets, &target).await?;
                Ok(Link {
                    session,
                    _jump: Some(jump),
                    learned,
                })
            }
        }
    };
    match tokio::time::timeout(plan.connect_timeout, work).await {
        Ok(result) => result,
        Err(_) => Err(ssh_error(
            SSH_TIMEOUT,
            format!(
                "The SSH connection to {} did not complete within {} s. Check the host, port and firewall.",
                target.label(),
                plan.connect_timeout.as_secs()
            ),
            target.detail(),
        )),
    }
}

async fn tcp_connect(peer: &Peer) -> AppResult<tokio::net::TcpStream> {
    match tokio::net::TcpStream::connect((peer.host.as_str(), peer.port)).await {
        Ok(stream) => {
            let _ = stream.set_nodelay(true);
            Ok(stream)
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => Err(ssh_error(
            SSH_TIMEOUT,
            format!("The {} did not answer: {error}", peer.label()),
            peer.detail(),
        )),
        Err(error) => {
            let hint = if error.kind() == std::io::ErrorKind::ConnectionRefused {
                " Nothing listens on that port: check the SSH port and that sshd runs."
            } else {
                ""
            };
            Err(ssh_error(
                SSH_UNREACHABLE,
                format!("Could not connect to the {}: {error}.{hint}", peer.label()),
                peer.detail(),
            ))
        }
    }
}

async fn handshake<S>(
    config: Arc<client::Config>,
    stream: S,
    peer: &Peer,
    plan: &Plan,
    learned: &mut Vec<HostKeyInfo>,
) -> AppResult<Handle<ClientHandler>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let verdict = Arc::new(Mutex::new(None));
    let handler = ClientHandler {
        peer: peer.clone(),
        policy: plan.ssh.host_key,
        known: plan.known.clone(),
        verdict: verdict.clone(),
    };
    let result = client::connect_stream(config, stream, handler).await;
    let verdict = verdict.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    match (result, verdict) {
        (Ok(session), verdict) => {
            if let Some(Verdict::Learned(info)) = verdict {
                learned.push(info);
            }
            Ok(session)
        }
        (Err(_), Some(verdict)) if !verdict.accepted() => Err(host_key_error(verdict, peer, plan)),
        (Err(error), _) => Err(protocol_error(error, peer)),
    }
}

fn host_key_error(verdict: Verdict, peer: &Peer, plan: &Plan) -> AppError {
    match verdict {
        Verdict::Changed { info, file, line } => ssh_error(
            SSH_HOST_KEY_MISMATCH,
            format!(
                "The host key of the {} has CHANGED. It now presents {} {}, but {} line {} records a different key. \
                 This can mean someone is intercepting the connection. If the server was reinstalled or its key \
                 rotated, verify the new fingerprint with its administrator before trusting it.",
                peer.label(),
                info.algorithm,
                info.fingerprint_sha256,
                file.display(),
                line
            ),
            serde_json::json!({
                "hostKey": info,
                "knownHostsFile": file,
                "line": line,
                "role": peer.role,
            }),
        ),
        Verdict::Unknown { info, other_algorithms } => {
            let why = if other_algorithms {
                "a key of another type is recorded for this host"
            } else {
                "this host is not in known_hosts"
            };
            ssh_error(
                SSH_HOST_KEY_UNKNOWN,
                format!(
                    "The authenticity of the {} cannot be established ({why}). Its {} key fingerprint is {}. \
                     Trust it only if it matches the server's.",
                    peer.label(),
                    info.algorithm,
                    info.fingerprint_sha256
                ),
                serde_json::json!({
                    "hostKey": info,
                    "knownHostsFile": plan.known.app_file,
                    "otherAlgorithms": other_algorithms,
                    "role": peer.role,
                }),
            )
        }
        Verdict::Failed(message) => ssh_error(SSH_CONFIG, message, peer.detail()),
        Verdict::Trusted | Verdict::Learned(_) | Verdict::Unchecked => ssh_error(
            SSH_PROTOCOL,
            format!("The {} rejected the connection.", peer.label()),
            peer.detail(),
        ),
    }
}

pub(crate) fn protocol_error(error: russh::Error, peer: &Peer) -> AppError {
    use russh::Error as E;
    let label = peer.label();
    match error {
        E::ConnectionTimeout | E::KeepaliveTimeout | E::InactivityTimeout | E::Elapsed(_) => {
            ssh_error(SSH_TIMEOUT, format!("The {label} stopped answering ({error})."), peer.detail())
        }
        E::IO(io) => ssh_error(
            SSH_UNREACHABLE,
            format!("The connection to the {label} failed during the SSH handshake: {io}. Is this port an SSH server?"),
            peer.detail(),
        ),
        E::HUP | E::Disconnect => ssh_error(
            SSH_UNREACHABLE,
            format!("The {label} closed the connection during the SSH handshake."),
            peer.detail(),
        ),
        E::Version => ssh_error(
            SSH_PROTOCOL,
            format!("The {label} did not answer with an SSH greeting. Is this port an SSH server?"),
            peer.detail(),
        ),
        E::NoCommonAlgo { kind, ours, theirs } => ssh_error(
            SSH_PROTOCOL,
            format!("The {label} supports no {kind:?} algorithm Fosta knows. Ours: {ours:?}; the server's: {theirs:?}."),
            peer.detail(),
        ),
        other => ssh_error(SSH_PROTOCOL, format!("SSH error with the {label}: {other}"), peer.detail()),
    }
}

fn method_name(auth: &SshAuth) -> &'static str {
    match auth {
        SshAuth::Password => "password",
        SshAuth::PrivateKey { .. } => "private key",
        SshAuth::Agent => "SSH agent",
    }
}

fn auth_outcome(result: AuthResult, user: &str, auth: &SshAuth, peer: &Peer) -> AppResult<()> {
    match result {
        AuthResult::Success => Ok(()),
        AuthResult::Failure {
            remaining_methods,
            partial_success,
        } => {
            let methods: Vec<&'static str> = remaining_methods.iter().map(<&'static str>::from).collect();
            let message = if partial_success {
                format!(
                    "The {} accepted the {} but asks for a second factor ({}), which Fosta does not support yet.",
                    peer.label(),
                    method_name(auth),
                    methods.join(", ")
                )
            } else {
                format!(
                    "The {} rejected {} authentication for user \"{user}\". The server accepts: {}.",
                    peer.label(),
                    method_name(auth),
                    if methods.is_empty() {
                        "nothing more".to_string()
                    } else {
                        methods.join(", ")
                    }
                )
            };
            let mut detail = peer.detail();
            detail["user"] = user.into();
            detail["method"] = method_name(auth).into();
            detail["serverMethods"] = methods.into();
            Err(ssh_error(SSH_AUTH_FAILED, message, detail))
        }
    }
}

async fn authenticate(session: &mut Handle<ClientHandler>, user: &str, auth: &SshAuth, secrets: &NetSecrets, peer: &Peer) -> AppResult<()> {
    match auth {
        SshAuth::Password => {
            let Some(password) = secrets.ssh_password.as_deref() else {
                return Err(secret_required(
                    "sshPassword",
                    format!("Enter the SSH password for the {}.", peer.label()),
                    peer,
                ));
            };
            let result = session
                .authenticate_password(user, password)
                .await
                .map_err(|error| protocol_error(error, peer))?;
            auth_outcome(result, user, auth, peer)
        }
        SshAuth::PrivateKey { key_file } => {
            let key = load_private_key(key_file, secrets.ssh_key_passphrase.clone(), peer).await?;
            let hash = if key.algorithm().is_rsa() {
                session
                    .best_supported_rsa_hash()
                    .await
                    .map_err(|error| protocol_error(error, peer))?
                    .flatten()
            } else {
                None
            };
            let result = session
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|error| protocol_error(error, peer))?;
            auth_outcome(result, user, auth, peer)
        }
        SshAuth::Agent => authenticate_with_agent(session, user, peer).await,
    }
}

fn expand_home(path: &str) -> PathBuf {
    let trimmed = path.trim();
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        if let Some(home) = crate::net::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(trimmed)
}

async fn load_private_key(key_file: &str, passphrase: Option<String>, peer: &Peer) -> AppResult<PrivateKey> {
    let path = expand_home(key_file);
    let shown = path.display().to_string();
    let has_passphrase = passphrase.as_deref().is_some_and(|text| !text.is_empty());
    // Decrypting an OpenSSH key runs bcrypt-pbkdf, which takes a noticeable moment: keep it off the runtime.
    let loaded =
        tokio::task::spawn_blocking(move || russh::keys::load_secret_key(&path, passphrase.as_deref().filter(|text| !text.is_empty())))
            .await
            .map_err(|error| ssh_error(SSH_KEY_FILE, format!("Could not load the private key: {error}"), peer.detail()))?;
    loaded.map_err(|error| match error {
        russh::keys::Error::IO(io) => ssh_error(
            SSH_KEY_FILE,
            format!("Could not read the private key file {shown}: {io}"),
            serde_json::json!({ "keyFile": shown }),
        ),
        russh::keys::Error::KeyIsEncrypted if !has_passphrase => secret_required(
            "sshKeyPassphrase",
            format!("The private key {shown} is encrypted. Enter its passphrase."),
            peer,
        ),
        other if has_passphrase => ssh_error(
            SSH_KEY_FILE,
            format!("Could not decrypt the private key {shown}; is the passphrase right? ({other})"),
            serde_json::json!({ "keyFile": shown }),
        ),
        other => ssh_error(
            SSH_KEY_FILE,
            format!("{shown} is not a usable OpenSSH or PEM private key: {other}"),
            serde_json::json!({ "keyFile": shown }),
        ),
    })
}

type DynAgent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

#[cfg(unix)]
async fn connect_agent() -> Result<DynAgent, String> {
    AgentClient::connect_env()
        .await
        .map(AgentClient::dynamic)
        .map_err(|error| format!("No SSH agent is reachable through SSH_AUTH_SOCK ({error}). Start ssh-agent and add a key with ssh-add."))
}

#[cfg(windows)]
async fn connect_agent() -> Result<DynAgent, String> {
    const OPENSSH_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
    let pipe = std::env::var("SSH_AUTH_SOCK")
        .ok()
        .filter(|path| path.starts_with(r"\\.\pipe\"))
        .unwrap_or_else(|| OPENSSH_PIPE.to_string());
    let pipe_error = match AgentClient::connect_named_pipe(&pipe).await {
        Ok(agent) => return Ok(agent.dynamic()),
        Err(error) => error,
    };
    match AgentClient::connect_pageant().await {
        Ok(agent) => Ok(agent.dynamic()),
        Err(pageant_error) => Err(format!(
            "No SSH agent is running: the OpenSSH agent pipe {pipe} is not available ({pipe_error}) and Pageant \
             did not answer ({pageant_error}). Start the \"OpenSSH Authentication Agent\" service or Pageant and \
             add a key."
        )),
    }
}

#[cfg(not(any(unix, windows)))]
async fn connect_agent() -> Result<DynAgent, String> {
    Err("SSH agents are not supported on this platform.".into())
}

async fn authenticate_with_agent(session: &mut Handle<ClientHandler>, user: &str, peer: &Peer) -> AppResult<()> {
    let agent_error = |message: String| ssh_error(SSH_AGENT, message, peer.detail());
    let mut agent = connect_agent().await.map_err(agent_error)?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| agent_error(format!("The SSH agent did not list its keys: {error}")))?;
    if identities.is_empty() {
        return Err(agent_error(
            "The SSH agent holds no keys. Add one with ssh-add (or load it in Pageant).".into(),
        ));
    }
    let mut last = None;
    for identity in &identities {
        let key = identity.public_key().into_owned();
        let hash = if key.algorithm().is_rsa() {
            session
                .best_supported_rsa_hash()
                .await
                .map_err(|error| protocol_error(error, peer))?
                .flatten()
        } else {
            None
        };
        match session.authenticate_publickey_with(user, key, hash, &mut agent).await {
            Ok(AuthResult::Success) => return Ok(()),
            Ok(failure) => last = Some(failure),
            // The agent refused to sign with this key (e.g. confirmation declined); try the next one.
            Err(_) => {}
        }
        if session.is_closed() {
            break;
        }
    }
    let failure = last.unwrap_or(AuthResult::Failure {
        remaining_methods: russh::MethodSet::empty(),
        partial_success: false,
    });
    auth_outcome(failure, user, &SshAuth::Agent, peer).map_err(|error| {
        let message = format!("{} (The agent offered {} key(s); none was accepted.)", error, identities.len());
        let mut detail = error.payload().unwrap_or_default();
        detail["agentKeys"] = identities.len().into();
        detail["message"] = message.clone().into();
        AppError::structured(message, detail)
    })
}
