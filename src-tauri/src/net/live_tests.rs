//! SSH tunnel tests against the containers of `dev/compose/ssh.yml`, forwarding to the dev MariaDB of
//! `dev/docker-compose.yml`. Ignored by default; run with:
//!   docker compose -f dev/docker-compose.yml up -d --wait mariadb
//!   docker compose -f dev/compose/ssh.yml up -d --wait
//!   cargo test net::live_tests -- --ignored --test-threads=1

use std::path::PathBuf;
use std::time::Duration;

use mysql_async::prelude::Queryable;

use super::ssh::{self, SSH_AUTH_FAILED, SSH_HOST_KEY_MISMATCH, SSH_HOST_KEY_UNKNOWN};
use super::ssh::{SSH_KEY_FILE, SSH_SECRET_REQUIRED};
use super::*;

const TARGET_CONTAINER: &str = "fosta-dev-ssh-ssh-target-1";
const KEY_PASSPHRASE: &str = "tunnel-key-pass";

/// MariaDB as the SSH target container sees it.
fn mariadb() -> Endpoint {
    Endpoint {
        host: "mariadb".into(),
        port: 3306,
    }
}

/// A scratch directory with a fresh known_hosts file.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fosta-net-live-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn ctx(&self) -> NetContext {
        NetContext {
            known_hosts: self.dir.join("known_hosts"),
            user_known_hosts: None,
            connect_timeout: Duration::from_secs(10),
        }
    }

    /// Copies a key the target container generated into the scratch directory.
    fn key(&self, name: &str) -> String {
        let output = std::process::Command::new("docker")
            .args(["exec", TARGET_CONTAINER, "cat", &format!("/config/keys/{name}")])
            .output()
            .expect("docker exec");
        assert!(
            output.status.success(),
            "is dev/compose/ssh.yml up? {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = self.dir.join(name);
        std::fs::write(&path, output.stdout).unwrap();
        path.to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn target_ssh(auth: SshAuth) -> SshSpec {
    SshSpec {
        enabled: true,
        host: "127.0.0.1".into(),
        port: 2222,
        user: "tunnel".into(),
        auth,
        jump_hosts: Vec::new(),
        host_key: HostKeyPolicy::AcceptNew,
        keepalive_secs: Some(5),
    }
}

fn password(text: &str) -> NetSecrets {
    NetSecrets {
        ssh_password: Some(text.into()),
        ..NetSecrets::default()
    }
}

/// Runs a query through `endpoint` and returns its single value.
async fn query(endpoint: &Endpoint) -> Result<i64, mysql_async::Error> {
    let opts = mysql_async::OptsBuilder::default()
        .ip_or_hostname(endpoint.host.clone())
        .tcp_port(endpoint.port)
        .user(Some("root"))
        .pass(Some("studio-root"))
        .db_name(Some("shop"));
    let mut conn = mysql_async::Conn::new(opts).await?;
    let value: Option<i64> = conn.query_first("SELECT 40 + 2").await?;
    conn.disconnect().await?;
    Ok(value.unwrap_or_default())
}

async fn tunnel(spec: SshSpec, secrets: &NetSecrets, ctx: &NetContext, target: Endpoint) -> AppResult<PreparedEndpoint> {
    prepare(
        &NetSpec {
            ssh: Some(spec),
            tls: None,
        },
        &target,
        secrets,
        ctx,
    )
    .await
}

#[tokio::test]
#[ignore]
async fn live_ssh_password_tunnel_runs_queries_and_closes_with_its_guard() {
    let scratch = Scratch::new("password");
    let prepared = tunnel(target_ssh(SshAuth::Password), &password("tunnel-pass"), &scratch.ctx(), mariadb())
        .await
        .unwrap();
    assert_eq!(prepared.endpoint.host, "127.0.0.1");
    assert_ne!(prepared.endpoint.port, 3306);
    assert_eq!(prepared.tls_server_name, "mariadb");
    let guard = prepared.tunnel.as_ref().unwrap();
    // Trust on first use recorded the key and reports it.
    let learned = guard.learned_host_keys();
    assert_eq!(learned.len(), 1);
    assert!(learned[0].fingerprint_sha256.starts_with("SHA256:"));
    assert!(std::fs::read_to_string(scratch.dir.join("known_hosts"))
        .unwrap()
        .contains("[127.0.0.1]:2222"));
    // Several connections share the session, concurrently too.
    let (a, b) = tokio::join!(query(&prepared.endpoint), query(&prepared.endpoint));
    assert_eq!(a.unwrap(), 42);
    assert_eq!(b.unwrap(), 42);
    assert_eq!(query(&prepared.endpoint).await.unwrap(), 42);
    assert!(guard.status().forwarded >= 3);
    // The second tunnel finds the key already known.
    let again = tunnel(target_ssh(SshAuth::Password), &password("tunnel-pass"), &scratch.ctx(), mariadb())
        .await
        .unwrap();
    assert!(again.tunnel.as_ref().unwrap().learned_host_keys().is_empty());
    drop(again);
    // Dropping the guard closes the local port.
    let endpoint = prepared.endpoint.clone();
    drop(prepared);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(tokio::net::TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .await
        .is_err());
}

#[tokio::test]
#[ignore]
async fn live_ssh_wrong_password_is_an_auth_failure() {
    let scratch = Scratch::new("wrong-password");
    let error = tunnel(target_ssh(SshAuth::Password), &password("nope"), &scratch.ctx(), mariadb())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(SSH_AUTH_FAILED), "{error}");
    assert!(error.to_string().contains("user \"tunnel\""), "{error}");
    let missing = tunnel(target_ssh(SshAuth::Password), &NetSecrets::default(), &scratch.ctx(), mariadb())
        .await
        .unwrap_err();
    assert_eq!(missing.code(), Some(SSH_SECRET_REQUIRED));
    assert_eq!(missing.payload().unwrap()["slot"], "sshPassword");
}

#[tokio::test]
#[ignore]
async fn live_ssh_private_keys_authenticate() {
    let scratch = Scratch::new("keys");
    // OpenSSH format, encrypted.
    let ed25519 = SshAuth::PrivateKey {
        key_file: scratch.key("id_ed25519"),
    };
    let secrets = NetSecrets {
        ssh_key_passphrase: Some(KEY_PASSPHRASE.into()),
        ..NetSecrets::default()
    };
    let prepared = tunnel(target_ssh(ed25519.clone()), &secrets, &scratch.ctx(), mariadb())
        .await
        .unwrap();
    assert_eq!(query(&prepared.endpoint).await.unwrap(), 42);
    drop(prepared);
    // PEM (PKCS#1) RSA, unencrypted: exercises the rsa-sha2 negotiation.
    let rsa = SshAuth::PrivateKey {
        key_file: scratch.key("id_rsa"),
    };
    let prepared = tunnel(target_ssh(rsa), &NetSecrets::default(), &scratch.ctx(), mariadb())
        .await
        .unwrap();
    assert_eq!(query(&prepared.endpoint).await.unwrap(), 42);
    drop(prepared);
    // Encrypted key without / with a wrong passphrase.
    let error = tunnel(target_ssh(ed25519.clone()), &NetSecrets::default(), &scratch.ctx(), mariadb())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(SSH_SECRET_REQUIRED), "{error}");
    assert_eq!(error.payload().unwrap()["slot"], "sshKeyPassphrase");
    let wrong = NetSecrets {
        ssh_key_passphrase: Some("wrong".into()),
        ..NetSecrets::default()
    };
    let error = tunnel(target_ssh(ed25519), &wrong, &scratch.ctx(), mariadb()).await.unwrap_err();
    assert_eq!(error.code(), Some(SSH_KEY_FILE), "{error}");
    let missing = SshAuth::PrivateKey {
        key_file: scratch.dir.join("missing").to_string_lossy().into_owned(),
    };
    let error = tunnel(target_ssh(missing), &NetSecrets::default(), &scratch.ctx(), mariadb())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Some(SSH_KEY_FILE), "{error}");
}

#[tokio::test]
#[ignore]
async fn live_ssh_host_keys_are_verified() {
    let scratch = Scratch::new("host-keys");
    let ctx = scratch.ctx();
    let strict = SshSpec {
        host_key: HostKeyPolicy::Strict,
        ..target_ssh(SshAuth::Password)
    };
    let secrets = password("tunnel-pass");
    let error = tunnel(strict.clone(), &secrets, &ctx, mariadb()).await.unwrap_err();
    assert_eq!(error.code(), Some(SSH_HOST_KEY_UNKNOWN), "{error}");
    let info: HostKeyInfo = serde_json::from_value(error.field("hostKey").unwrap().clone()).unwrap();
    assert!(error.to_string().contains(&info.fingerprint_sha256));
    // The user accepts the prompt.
    ssh::trust_host_key(&ctx.known_hosts, &info.host, info.port, &info.public_key).unwrap();
    let prepared = tunnel(strict.clone(), &secrets, &ctx, mariadb()).await.unwrap();
    assert_eq!(query(&prepared.endpoint).await.unwrap(), 42);
    drop(prepared);
    // A different key of the same type is refused, even with accept-new.
    let other = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    assert_eq!(info.algorithm, "ssh-ed25519", "russh prefers the server's Ed25519 host key");
    ssh::trust_host_key(&ctx.known_hosts, &info.host, info.port, other).unwrap();
    let error = tunnel(target_ssh(SshAuth::Password), &secrets, &ctx, mariadb()).await.unwrap_err();
    assert_eq!(error.code(), Some(SSH_HOST_KEY_MISMATCH), "{error}");
    assert!(error.to_string().contains("CHANGED"), "{error}");
    assert_eq!(error.payload().unwrap()["hostKey"]["fingerprintSha256"], info.fingerprint_sha256);
    // Off skips the check entirely.
    let off = SshSpec {
        host_key: HostKeyPolicy::Off,
        ..target_ssh(SshAuth::Password)
    };
    assert!(tunnel(off, &secrets, &ctx, mariadb()).await.is_ok());
}

#[tokio::test]
#[ignore]
async fn live_ssh_jump_host_reaches_an_internal_ssh_server() {
    let scratch = Scratch::new("jump");
    // The jump host only knows ssh-target by its compose name; 127.0.0.1:2223 is the jump host itself.
    let spec = SshSpec {
        host: "ssh-target".into(),
        port: 2222,
        jump_hosts: vec![SshHop {
            host: "127.0.0.1".into(),
            port: 2223,
            user: "jump".into(),
            auth: SshAuth::Password,
        }],
        ..target_ssh(SshAuth::PrivateKey {
            key_file: scratch.key("id_ed25519"),
        })
    };
    let secrets = NetSecrets {
        ssh_password: Some("jump-pass".into()),
        ssh_key_passphrase: Some(KEY_PASSPHRASE.into()),
        ..NetSecrets::default()
    };
    let prepared = tunnel(spec, &secrets, &scratch.ctx(), mariadb()).await.unwrap();
    let learned = prepared.tunnel.as_ref().unwrap().learned_host_keys().to_vec();
    assert_eq!(learned.len(), 2, "{learned:?}");
    assert!(learned.iter().any(|info| info.host == "ssh-target"));
    assert_eq!(query(&prepared.endpoint).await.unwrap(), 42);
}

#[tokio::test]
#[ignore]
async fn live_ssh_tunnel_reconnects_after_the_session_drops() {
    let scratch = Scratch::new("reconnect");
    let prepared = tunnel(target_ssh(SshAuth::Password), &password("tunnel-pass"), &scratch.ctx(), mariadb())
        .await
        .unwrap();
    let guard = prepared.tunnel.as_ref().unwrap();
    assert_eq!(query(&prepared.endpoint).await.unwrap(), 42);
    guard.break_session_for_test().await;
    assert_eq!(query(&prepared.endpoint).await.unwrap(), 42);
    let status = guard.status();
    assert!(status.connected);
    assert_eq!(status.reconnects, 1, "{status:?}");
}

#[tokio::test]
#[ignore]
async fn live_ssh_refused_forward_is_reported() {
    let scratch = Scratch::new("forward");
    let closed = Endpoint {
        host: "mariadb".into(),
        port: 9,
    };
    let prepared = tunnel(target_ssh(SshAuth::Password), &password("tunnel-pass"), &scratch.ctx(), closed)
        .await
        .unwrap();
    assert!(query(&prepared.endpoint).await.is_err());
    tokio::time::sleep(Duration::from_millis(100)).await;
    let last = prepared.tunnel.as_ref().unwrap().last_error().unwrap_or_default();
    assert!(last.contains("refused to forward to mariadb:9"), "{last}");
}

/// An in-process agent (russh's agent server) on a private socket or pipe, found through `SSH_AUTH_SOCK`.
#[tokio::test]
#[ignore]
async fn live_ssh_agent_authenticates() {
    use russh::keys::agent::client::AgentClient;

    #[derive(Clone)]
    struct TestAgent;
    impl russh::keys::agent::server::Agent for TestAgent {}

    let scratch = Scratch::new("agent");
    let key = russh::keys::load_secret_key(scratch.key("id_ed25519"), Some(KEY_PASSPHRASE)).unwrap();

    #[cfg(windows)]
    let socket = {
        use tokio::net::windows::named_pipe::ServerOptions;
        let name = format!(r"\\.\pipe\fosta-test-agent-{}", std::process::id());
        let first = ServerOptions::new().first_pipe_instance(true).create(&name).unwrap();
        let pipe = name.clone();
        let incoming = futures_util::stream::unfold(first, move |server| {
            let pipe = pipe.clone();
            async move {
                server.connect().await.ok()?;
                let next = ServerOptions::new().create(&pipe).ok()?;
                Some((Ok(server), next))
            }
        });
        tokio::spawn(russh::keys::agent::server::serve(Box::pin(incoming), TestAgent));
        let mut client = AgentClient::connect_named_pipe(&name).await.unwrap();
        client.add_identity(&key, &[]).await.unwrap();
        name
    };
    #[cfg(unix)]
    let socket = {
        let path = scratch.dir.join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let incoming = futures_util::stream::unfold(listener, |listener| async move {
            let accepted = listener.accept().await.map(|(stream, _)| stream);
            Some((accepted, listener))
        });
        tokio::spawn(russh::keys::agent::server::serve(Box::pin(incoming), TestAgent));
        let mut client = AgentClient::connect_uds(&path).await.unwrap();
        client.add_identity(&key, &[]).await.unwrap();
        path.to_string_lossy().into_owned()
    };

    let previous = std::env::var_os("SSH_AUTH_SOCK");
    std::env::set_var("SSH_AUTH_SOCK", &socket);
    let result = tunnel(target_ssh(SshAuth::Agent), &NetSecrets::default(), &scratch.ctx(), mariadb()).await;
    match previous {
        Some(value) => std::env::set_var("SSH_AUTH_SOCK", value),
        None => std::env::remove_var("SSH_AUTH_SOCK"),
    }
    let prepared = result.unwrap();
    assert_eq!(query(&prepared.endpoint).await.unwrap(), 42);
}

/// The whole path the app takes: a contract `ConnectionSpec` with SSH and TLS settings through the connection manager
/// (`net::prepare`, then the MariaDB engine with mysql_async's TLS) to the dev MariaDB, which serves a self-signed
/// certificate (MariaDB 11.4+).
#[tokio::test]
#[ignore]
async fn live_ssh_connection_manager_tunnels_and_tls_reach_mariadb() {
    use crate::contract::{self, ConnectSecrets, ConnectionSpec, SecretSlot, SshConfig, TlsConfig};
    use crate::runtime::ConnectionManager;

    let secrets = |ssh_password: &str| {
        let mut secrets = ConnectSecrets::password("studio-root");
        secrets.values.insert(SecretSlot::SshPassword, ssh_password.into());
        secrets
    };
    let require_tls = Some(TlsConfig {
        mode: contract::TlsMode::Require,
        accept_invalid_certs: true,
        ..TlsConfig::default()
    });
    // Direct TLS first (no tunnel).
    let direct = ConnectionSpec {
        engine: "mariadb".into(),
        host: "127.0.0.1".into(),
        port: 3306,
        user: "root".into(),
        database: Some("shop".into()),
        tls: require_tls.clone(),
        ..ConnectionSpec::default()
    };
    let info = ConnectionManager::probe(direct, secrets("")).await.expect("direct TLS");
    assert_eq!(info.server.product, "MariaDB");

    let mut tunneled = ConnectionSpec {
        engine: "mariadb".into(),
        host: mariadb().host,
        port: mariadb().port,
        user: "root".into(),
        database: Some("shop".into()),
        ssh: Some(SshConfig {
            enabled: true,
            host: "127.0.0.1".into(),
            port: 2222,
            user: "tunnel".into(),
            auth: contract::spec::SshAuth::Password,
            jump_hosts: Vec::new(),
            // Off: this test must not write the app's own known_hosts file.
            host_key: contract::spec::HostKeyPolicy::Off,
            keepalive_secs: None,
        }),
        ..ConnectionSpec::default()
    };
    let info = ConnectionManager::probe(tunneled.clone(), secrets("tunnel-pass"))
        .await
        .expect("through the tunnel");
    assert_eq!(info.server.product, "MariaDB");
    tunneled.tls = require_tls;
    ConnectionManager::probe(tunneled.clone(), secrets("tunnel-pass"))
        .await
        .expect("TLS through the tunnel");
    let Err(error) = ConnectionManager::probe(tunneled.clone(), secrets("wrong")).await else {
        panic!("a wrong SSH password must fail")
    };
    assert_eq!(error.code(), Some(SSH_AUTH_FAILED));
    tunneled.host.clear();
    let Err(error) = ConnectionManager::probe(tunneled, secrets("tunnel-pass")).await else {
        panic!("a tunnel needs the database host")
    };
    assert_eq!(error.code(), Some(ssh::SSH_CONFIG));
}
