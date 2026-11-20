//! Redis Sentinel: asking the sentinels for the current master, connecting to it, and following a failover.
//!
//! [`SentinelResolver::resolve`] walks the sentinels (the last one that answered first): `SENTINEL GET-MASTER-ADDR-BY-NAME`,
//! the address map, a connection with the data user and password, then `ROLE` must say `master` (a sentinel that has not
//! seen the failover yet names a replica). The sentinels have their own password: empty means none, and a sentinel that
//! answers `NOAUTH` is tried again with the data password.
//!
//! [`SentinelLink`] holds the engine's shared master connection. When a command fails because the master went away
//! (connection refused or dropped) or turned into a replica (`READONLY`, `MASTERDOWN`), the link resolves again and swaps
//! the connection. Only `READONLY`/`MASTERDOWN` and a refused connection retry the command (it never ran); a connection
//! that dropped mid-command is swapped but the command is not sent again, since it may have run. A command that timed out
//! is not sent again either, but the sentinels are asked in the background and the connection is swapped when they name
//! another master (a master that went silent never refuses nor drops the connection). One resolve is bounded by a few
//! connect timeouts.

use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        RwLock,
    },
    time::Duration,
};

use redis::{aio::MultiplexedConnection, AsyncConnectionConfig, RedisError, Value};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{
    connection::{dial_node, name_node, HostPort, RedisConnectOptions, RedisTopology, DEFAULT_CONNECT_TIMEOUT_MS},
    types::{as_items, as_pairs, as_string},
};
use crate::{
    contract::EngineError,
    error::{AppError, AppResult},
};

/// The default sentinel port, for `options.nodes` and sentinel URLs without one.
pub const DEFAULT_SENTINEL_PORT: u16 = 26379;

/// Finds the current master of one monitored name.
pub struct SentinelResolver {
    master_name: String,
    sentinels: Vec<HostPort>,
    username: Option<String>,
    password: Option<String>,
    /// The data connection's options (user, password, TLS, database, address map, timeouts).
    data: RedisConnectOptions,
    /// Index of the sentinel that answered last; tried first next time.
    last_good: AtomicUsize,
}

/// The master the sentinels named, connected.
pub struct Resolved {
    /// The address the sentinels announce.
    pub master: HostPort,
    pub conn: MultiplexedConnection,
}

/// Why one sentinel did not lead to a master; the most telling reason across all sentinels is reported.
enum Miss {
    Unreachable(String),
    Replica(HostPort),
    UnknownMaster,
    SentinelAuth,
}

impl SentinelResolver {
    pub fn new(options: &RedisConnectOptions) -> AppResult<Self> {
        let RedisTopology::Sentinel {
            master_name,
            sentinels,
            sentinel_username,
            sentinel_password,
            ..
        } = &options.topology
        else {
            return Err(AppError::new("This connection does not use Redis Sentinel."));
        };
        if master_name.trim().is_empty() {
            return Err(master_name_required());
        }
        if sentinels.is_empty() {
            return Err(AppError::new("Enter the address of at least one sentinel."));
        }
        Ok(Self {
            master_name: master_name.trim().to_owned(),
            sentinels: sentinels.clone(),
            username: sentinel_username.clone().filter(|name| !name.trim().is_empty()),
            password: sentinel_password.clone().filter(|password| !password.is_empty()),
            data: options.clone(),
            last_good: AtomicUsize::new(0),
        })
    }

    pub fn master_name(&self) -> &str {
        &self.master_name
    }

    fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.data.connect_timeout_ms.unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS)).max(Duration::from_millis(500))
    }

    /// The sentinels in the order to try: the last one that answered first.
    fn order(&self) -> Vec<usize> {
        let start = self.last_good.load(Ordering::Relaxed) % self.sentinels.len();
        (0..self.sentinels.len())
            .map(|step| (start + step) % self.sentinels.len())
            .collect()
    }

    /// A connection to one sentinel: database 0, RESP2, the profile's TLS, the sentinel user and password.
    async fn dial_sentinel(&self, sentinel: &HostPort, password: Option<&str>) -> Result<MultiplexedConnection, RedisError> {
        let options = RedisConnectOptions {
            host: sentinel.host.clone(),
            port: Some(sentinel.port),
            username: self.username.clone(),
            password: password.map(str::to_owned),
            db: 0,
            tls: self.data.tls.clone(),
            resp3: false,
            topology: RedisTopology::Standalone,
            address_map: Vec::new(),
            client_name: None,
            ..self.data.clone()
        };
        let client = options
            .client()
            .map_err(|error| RedisError::from((redis::ErrorKind::InvalidClientConfig, "sentinel address", error.to_string())))?;
        let timeout = self.connect_timeout();
        let config = AsyncConnectionConfig::new()
            .set_connection_timeout(Some(timeout))
            .set_response_timeout(Some(timeout));
        let mut conn = client.get_multiplexed_async_connection_with_config(&config).await?;
        redis::cmd("PING").query_async::<Value>(&mut conn).await?;
        Ok(conn)
    }

    /// A connection to one sentinel that is allowed to talk: with the sentinel password, or without any; a sentinel that
    /// answers `NOAUTH` to a connection without a password is tried with the data password.
    async fn sentinel_at(&self, sentinel: &HostPort) -> Result<MultiplexedConnection, RedisError> {
        let first = self.dial_sentinel(sentinel, self.password.as_deref()).await;
        match (
            &first,
            &self.password,
            self.data.password.as_deref().filter(|password| !password.is_empty()),
        ) {
            (Err(error), None, Some(data_password)) if is_auth_error(error) => self.dial_sentinel(sentinel, Some(data_password)).await,
            _ => first,
        }
    }

    /// A connection to the first sentinel that answers and lets us in, and its address. Like
    /// [`SentinelResolver::resolve`], a sentinel that refuses the login is skipped (another may have the password the
    /// profile has); only when none lets us in is that the error.
    pub async fn sentinel(&self) -> AppResult<(HostPort, MultiplexedConnection)> {
        let (mut last, mut refused) = (None, false);
        for index in self.order() {
            let sentinel = &self.sentinels[index];
            match self.sentinel_at(sentinel).await {
                Ok(conn) => {
                    self.last_good.store(index, Ordering::Relaxed);
                    return Ok((sentinel.clone(), conn));
                }
                Err(error) if is_auth_error(&error) => refused = true,
                Err(error) => last = Some(format!("{sentinel}: {error}")),
            }
        }
        if refused {
            return Err(sentinel_auth_error());
        }
        Err(unreachable_error(last.unwrap_or_default()))
    }

    /// How long one whole [`SentinelResolver::resolve`] may take: a few connect timeouts, enough for one sentinel that
    /// does not answer and another that does, never unbounded (a failover recovery holds the link's refresh lock).
    fn resolve_deadline(&self) -> Duration {
        self.connect_timeout() * 3
    }

    /// Asks the sentinels for the master, connects to it with the data credentials (database `db` of the options) and
    /// checks that it is a master, within [`SentinelResolver::resolve_deadline`].
    pub async fn resolve(&self) -> AppResult<Resolved> {
        let deadline = self.resolve_deadline();
        match tokio::time::timeout(deadline, self.resolve_unbounded()).await {
            Ok(resolved) => resolved,
            Err(_) => Err(unreachable_error(format!("no master found within {} ms", deadline.as_millis()))),
        }
    }

    async fn resolve_unbounded(&self) -> AppResult<Resolved> {
        let mut misses = Vec::new();
        for index in self.order() {
            let sentinel = &self.sentinels[index];
            let mut conn = match self.sentinel_at(sentinel).await {
                Ok(conn) => conn,
                Err(error) if is_auth_error(&error) => {
                    misses.push(Miss::SentinelAuth);
                    continue;
                }
                Err(error) => {
                    misses.push(Miss::Unreachable(format!("sentinel {sentinel}: {error}")));
                    continue;
                }
            };
            let reply = tokio::time::timeout(
                self.connect_timeout(),
                redis::cmd("SENTINEL")
                    .arg("GET-MASTER-ADDR-BY-NAME")
                    .arg(&self.master_name)
                    .query_async::<Value>(&mut conn),
            )
            .await;
            let announced = match reply {
                Ok(Ok(reply)) => match master_address(reply) {
                    Some(address) => address,
                    None => {
                        misses.push(Miss::UnknownMaster);
                        continue;
                    }
                },
                Ok(Err(error)) if is_auth_error(&error) => {
                    misses.push(Miss::SentinelAuth);
                    continue;
                }
                Ok(Err(error)) => {
                    misses.push(Miss::Unreachable(format!("sentinel {sentinel}: {error}")));
                    continue;
                }
                Err(_) => {
                    misses.push(Miss::Unreachable(format!("sentinel {sentinel} did not answer")));
                    continue;
                }
            };
            let dialed = self.data.mapped(&announced);
            let options = RedisConnectOptions {
                host: dialed.host.clone(),
                port: Some(dialed.port),
                topology: RedisTopology::Standalone,
                ..self.data.clone()
            };
            let mut master = match dial_node(&options.client()?, &options).await {
                Ok(conn) => conn,
                // Wrong data credentials: every sentinel would lead to the same refusal.
                Err(error) if is_auth_error(&error) => return Err(error.into()),
                Err(error) => {
                    misses.push(Miss::Unreachable(format!("master {dialed}: {}", AppError::from(error).message())));
                    continue;
                }
            };
            if let Err(error) = name_node(&mut master, &options).await {
                misses.push(Miss::Unreachable(format!("master {dialed}: {}", error.message())));
                continue;
            }
            let role = tokio::time::timeout(self.connect_timeout(), redis::cmd("ROLE").query_async::<Value>(&mut master)).await;
            let is_master = match role {
                Ok(Ok(reply)) => role_of(reply).as_deref() == Some("master"),
                // An ACL user without ROLE: trust the sentinels.
                Ok(Err(error)) if error.code().is_some() => true,
                Ok(Err(error)) => {
                    misses.push(Miss::Unreachable(format!("master {dialed}: {error}")));
                    continue;
                }
                Err(_) => {
                    misses.push(Miss::Unreachable(format!("master {dialed} did not answer ROLE")));
                    continue;
                }
            };
            if !is_master {
                misses.push(Miss::Replica(announced));
                continue;
            }
            self.last_good.store(index, Ordering::Relaxed);
            return Ok(Resolved {
                master: announced,
                conn: master,
            });
        }
        Err(self.explain(misses))
    }

    fn explain(&self, misses: Vec<Miss>) -> AppError {
        let name = &self.master_name;
        if misses.iter().any(|miss| matches!(miss, Miss::SentinelAuth)) {
            return sentinel_auth_error();
        }
        if let Some(Miss::Replica(address)) = misses.iter().find(|miss| matches!(miss, Miss::Replica(_))) {
            return AppError::from(EngineError::connect(format!(
                "The sentinels name {address} as the master of `{name}`, but it answers as a replica (failover in progress?). Try \
                 again in a few seconds."
            )))
            .with_engine(super::descriptor::ID);
        }
        if misses.iter().any(|miss| matches!(miss, Miss::UnknownMaster)) {
            return AppError::new(format!(
                "The sentinels do not monitor a master named `{name}`. Check the master name (SENTINEL MASTERS lists them)."
            ));
        }
        let detail = misses
            .into_iter()
            .filter_map(|miss| match miss {
                Miss::Unreachable(text) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("; ");
        unreachable_error(detail)
    }

    /// `SENTINEL <sub> <master name>` on a sentinel connection.
    async fn ask(&self, conn: &MultiplexedConnection, sub: &str) -> AppResult<Value> {
        let mut conn = conn.clone();
        let cmd = redis::cmd("SENTINEL").arg(sub).arg(&self.master_name).clone();
        match tokio::time::timeout(self.connect_timeout(), cmd.query_async::<Value>(&mut conn)).await {
            Ok(reply) => Ok(reply?),
            Err(_) => Err(AppError::new("The sentinel did not answer in time.")),
        }
    }

    /// What the sentinels know of the master: its address, quorum, replicas and the sentinels watching it.
    pub async fn overview(&self) -> AppResult<SentinelOverview> {
        let (asked, conn) = self.sentinel().await?;
        let master = fields(self.ask(&conn, "MASTER").await?);
        let replicas = as_items(self.ask(&conn, "REPLICAS").await?)
            .into_iter()
            .map(fields)
            .map(peer)
            .collect();
        let mut sentinels: Vec<SentinelPeer> = as_items(self.ask(&conn, "SENTINELS").await?)
            .into_iter()
            .map(fields)
            .map(peer)
            .collect();
        sentinels.insert(
            0,
            SentinelPeer {
                address: asked.to_string(),
                flags: "sentinel (answering)".into(),
                link: None,
            },
        );
        let get = |key: &str| master.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone());
        let address = match (get("ip"), get("port").and_then(|port| port.parse().ok())) {
            (Some(host), Some(port)) => HostPort::new(host, port).to_string(),
            _ => String::new(),
        };
        Ok(SentinelOverview {
            master_name: self.master_name.clone(),
            master: address,
            master_flags: get("flags").unwrap_or_default(),
            quorum: get("quorum").and_then(|quorum| quorum.parse().ok()),
            replicas,
            sentinels,
        })
    }
}

/// The engine's shared master connection behind Sentinel, swapped when a failover moves the master.
pub struct SentinelLink {
    pub resolver: SentinelResolver,
    current: RwLock<Current>,
    /// One re-resolve at a time; the others wait and use its connection.
    refresh: Mutex<()>,
}

struct Current {
    generation: u64,
    master: HostPort,
    conn: MultiplexedConnection,
}

/// A failure that may mean the master moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failover {
    /// `READONLY` or `MASTERDOWN`: the server refused before running anything, so the command can run again.
    Demoted,
    /// The connection was refused: nothing was sent.
    Refused,
    /// The connection dropped: the command may have run, so it is not sent again.
    Dropped,
}

impl Failover {
    pub fn of(error: &RedisError) -> Option<Self> {
        if matches!(error.code(), Some("READONLY" | "MASTERDOWN")) {
            Some(Self::Demoted)
        } else if error.is_connection_refusal() {
            Some(Self::Refused)
        } else if error.is_connection_dropped() || error.is_io_error() || error.is_unrecoverable_error() {
            Some(Self::Dropped)
        } else {
            None
        }
    }

    /// The command did not run, so sending it again is safe.
    pub fn retry(self) -> bool {
        matches!(self, Self::Demoted | Self::Refused)
    }
}

impl SentinelLink {
    pub fn new(resolver: SentinelResolver, resolved: Resolved) -> Self {
        Self {
            resolver,
            current: RwLock::new(Current {
                generation: 0,
                master: resolved.master,
                conn: resolved.conn,
            }),
            refresh: Mutex::new(()),
        }
    }

    /// The connection in use and its generation (bumped on each swap).
    pub fn current(&self) -> (u64, MultiplexedConnection) {
        let current = self.current.read().unwrap_or_else(|poisoned| poisoned.into_inner());
        (current.generation, current.conn.clone())
    }

    /// The master's address as the sentinels announce it.
    pub fn master(&self) -> HostPort {
        self.current.read().unwrap_or_else(|poisoned| poisoned.into_inner()).master.clone()
    }

    /// After a failure on the connection of `generation`: resolves the master again and swaps the connection. The new
    /// connection, or `None` when the sentinels do not name a working master (yet).
    pub async fn recover(&self, generation: u64) -> Option<MultiplexedConnection> {
        let _one_at_a_time = self.refresh.lock().await;
        {
            let current = self.current.read().unwrap_or_else(|poisoned| poisoned.into_inner());
            if current.generation != generation {
                return Some(current.conn.clone());
            }
        }
        let resolved = self.resolver.resolve().await.ok()?;
        let mut current = self.current.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        current.generation += 1;
        current.master = resolved.master;
        current.conn = resolved.conn.clone();
        Some(resolved.conn)
    }

    /// After a command on the connection of `generation` timed out: asks the sentinels again and swaps the connection only
    /// when they name another master (a master that went silent, e.g. behind a dead route, never refuses or drops the
    /// connection). A slow command on the right master changes nothing. The command is never sent again.
    pub async fn recheck(&self, generation: u64) {
        let _one_at_a_time = self.refresh.lock().await;
        if self.current.read().unwrap_or_else(|poisoned| poisoned.into_inner()).generation != generation {
            return;
        }
        let Ok(resolved) = self.resolver.resolve().await else {
            return;
        };
        let mut current = self.current.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.master != resolved.master {
            current.generation += 1;
            current.master = resolved.master;
            current.conn = resolved.conn;
        }
    }

    /// The message when the master was lost mid-command and the link reconnected: the user runs it again.
    pub fn reconnected_error(&self, error: &RedisError) -> AppError {
        AppError::from(EngineError::connect(format!(
            "The connection to the Redis master dropped ({error}); reconnected to the current master {}. Run it again.",
            self.master()
        )))
        .with_engine(super::descriptor::ID)
    }
}

/// What the sentinels report about the monitored master.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentinelOverview {
    pub master_name: String,
    /// `host:port` as the sentinels announce it.
    pub master: String,
    /// The sentinels' flags for the master (`master`, `master,s_down`, ...).
    pub master_flags: String,
    pub quorum: Option<u32>,
    pub replicas: Vec<SentinelPeer>,
    /// The sentinel that answered first, then the others it knows.
    pub sentinels: Vec<SentinelPeer>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentinelPeer {
    pub address: String,
    pub flags: String,
    /// A replica's `master-link-status` (`ok`, `err`).
    pub link: Option<String>,
}

/// `SENTINEL MASTER`/`REPLICAS`/`SENTINELS` entries: flat field/value lists (RESP2) or maps (RESP3).
fn fields(value: Value) -> Vec<(String, String)> {
    as_pairs(value)
        .into_iter()
        .filter_map(|(key, value)| Some((as_string(key)?, as_string(value).unwrap_or_default())))
        .collect()
}

fn peer(fields: Vec<(String, String)>) -> SentinelPeer {
    let get = |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone());
    let address = match (get("ip"), get("port").and_then(|port| port.parse().ok())) {
        (Some(host), Some(port)) => HostPort::new(host, port).to_string(),
        _ => get("name").unwrap_or_default(),
    };
    SentinelPeer {
        address,
        flags: get("flags").unwrap_or_default(),
        link: get("master-link-status"),
    }
}

/// `[host, port]` from `SENTINEL GET-MASTER-ADDR-BY-NAME`; `None` for nil (the name is not monitored).
pub(crate) fn master_address(reply: Value) -> Option<HostPort> {
    let mut items = as_items(reply).into_iter();
    let host = as_string(items.next()?)?;
    let port = super::types::as_i64(&items.next()?)?;
    Some(HostPort::new(host, u16::try_from(port).ok()?))
}

/// The first element of `ROLE`: `master`, `slave` or `sentinel`.
fn role_of(reply: Value) -> Option<String> {
    as_items(reply).into_iter().next().and_then(as_string)
}

/// A refused login, told by the client's error kind or the server's error code, never by the message text (a host name
/// or a master name may well contain "password").
fn is_auth_error(error: &RedisError) -> bool {
    matches!(error.kind(), redis::ErrorKind::AuthenticationFailed) || matches!(error.code(), Some("NOAUTH" | "WRONGPASS"))
}

fn sentinel_auth_error() -> AppError {
    AppError::from(EngineError::connect(
        "The sentinels rejected the sentinel user name or password. Leave the sentinel password empty when the sentinels \
         have none.",
    ))
    .with_engine(super::descriptor::ID)
}

fn unreachable_error(detail: String) -> AppError {
    AppError::from(EngineError::connect(format!(
        "Could not reach a Redis master through the sentinels. Check the sentinel addresses and ports. ({detail})"
    )))
    .with_engine(super::descriptor::ID)
}

pub(crate) fn master_name_required() -> AppError {
    AppError::new("Redis Sentinel needs the master name (the name in `sentinel monitor <name> ...`).")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(text: &str) -> Value {
        Value::BulkString(text.as_bytes().to_vec())
    }

    #[test]
    fn master_addresses_and_roles_are_read() {
        let reply = Value::Array(vec![bulk("10.0.0.5"), bulk("6390")]);
        assert_eq!(master_address(reply), Some(HostPort::new("10.0.0.5", 6390)));
        assert_eq!(master_address(Value::Nil), None);
        assert_eq!(
            role_of(Value::Array(vec![bulk("master"), Value::Int(1)])).as_deref(),
            Some("master")
        );
    }

    #[test]
    fn sentinel_entries_become_peers() {
        let entry = Value::Array(
            [
                "name",
                "127.0.0.1:6391",
                "ip",
                "127.0.0.1",
                "port",
                "6391",
                "flags",
                "slave",
                "master-link-status",
                "ok",
            ]
            .map(bulk)
            .to_vec(),
        );
        let peer = peer(fields(entry));
        assert_eq!(peer.address, "127.0.0.1:6391");
        assert_eq!((peer.flags.as_str(), peer.link.as_deref()), ("slave", Some("ok")));
    }

    #[test]
    fn failover_errors_say_whether_to_retry() {
        let server = |code: &str| redis::make_extension_error(code.into(), Some("x".into()));
        assert_eq!(Failover::of(&server("READONLY")), Some(Failover::Demoted));
        assert_eq!(Failover::of(&server("MASTERDOWN")), Some(Failover::Demoted));
        assert_eq!(Failover::of(&server("WRONGTYPE")), None);
        let refused = RedisError::from(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert_eq!(Failover::of(&refused), Some(Failover::Refused));
        let dropped = RedisError::from(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert_eq!(Failover::of(&dropped), Some(Failover::Dropped));
        assert!(Failover::Demoted.retry() && Failover::Refused.retry() && !Failover::Dropped.retry());
    }

    #[test]
    fn the_resolver_needs_a_master_name_and_a_sentinel() {
        let options = |name: &str, sentinels: Vec<HostPort>| RedisConnectOptions {
            topology: RedisTopology::Sentinel {
                master_name: name.into(),
                sentinels,
                sentinel_username: None,
                sentinel_password: Some(String::new()),
                read_replica: false,
            },
            ..Default::default()
        };
        assert!(SentinelResolver::new(&options(" ", vec![HostPort::new("s", 26379)])).is_err());
        assert!(SentinelResolver::new(&options("fosta", Vec::new())).is_err());
        let resolver = SentinelResolver::new(&options("fosta", vec![HostPort::new("a", 1), HostPort::new("b", 2)])).unwrap();
        // An empty sentinel password means none.
        assert!(resolver.password.is_none());
        resolver.last_good.store(1, Ordering::Relaxed);
        assert_eq!(resolver.order(), [1, 0]);
        assert!(SentinelResolver::new(&RedisConnectOptions::default()).is_err());
    }

    #[test]
    fn logins_are_told_by_kind_and_code_not_by_text() {
        let server = |code: &str, detail: &str| redis::make_extension_error(code.into(), Some(detail.into()));
        assert!(is_auth_error(&server("WRONGPASS", "invalid username-password pair")));
        assert!(is_auth_error(&server("NOAUTH", "Authentication required.")));
        assert!(is_auth_error(&RedisError::from((redis::ErrorKind::AuthenticationFailed, "x"))));
        assert!(!is_auth_error(&server("ERR", "No such master with that name: password-store")));
        let io = RedisError::from(std::io::Error::other("connect to authentication.internal failed"));
        assert!(!is_auth_error(&io));
    }

    /// A fake RESP server on 127.0.0.1: `reply` answers each command (its arguments), `None` never answers it.
    async fn fake_server(reply: fn(&[String]) -> Option<String>) -> HostPort {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let Ok(read) = socket.read(&mut chunk).await else { return };
                        if read == 0 {
                            return;
                        }
                        buffer.extend_from_slice(&chunk[..read]);
                        while let Some((args, used)) = parse_command(&buffer) {
                            buffer.drain(..used);
                            match reply(&args) {
                                Some(answer) => {
                                    if socket.write_all(answer.as_bytes()).await.is_err() {
                                        return;
                                    }
                                }
                                // Hold the connection, never answer.
                                None => std::future::pending::<()>().await,
                            }
                        }
                    }
                });
            }
        });
        HostPort::new("127.0.0.1", port)
    }

    /// One `*N` array of bulk strings from the front of `buffer`, and the bytes it used.
    fn parse_command(buffer: &[u8]) -> Option<(Vec<String>, usize)> {
        fn line(buffer: &[u8], at: usize) -> Option<(&str, usize)> {
            let end = at + buffer.get(at..)?.windows(2).position(|pair| pair == b"\r\n")?;
            Some((std::str::from_utf8(&buffer[at..end]).ok()?, end + 2))
        }
        let (header, mut at) = line(buffer, 0)?;
        let count: usize = header.strip_prefix('*')?.parse().ok()?;
        let mut args = Vec::new();
        for _ in 0..count {
            let (size, next) = line(buffer, at)?;
            let size: usize = size.strip_prefix('$')?.parse().ok()?;
            let arg = buffer.get(next..next + size)?;
            args.push(String::from_utf8_lossy(arg).into_owned());
            at = next + size + 2;
            if buffer.len() < at {
                return None;
            }
        }
        Some((args, at))
    }

    fn resolver_for(sentinels: Vec<HostPort>, client_name: Option<&str>) -> SentinelResolver {
        SentinelResolver::new(&RedisConnectOptions {
            connect_timeout_ms: Some(300),
            client_name: client_name.map(str::to_owned),
            topology: RedisTopology::Sentinel {
                master_name: "fosta".into(),
                sentinels,
                sentinel_username: None,
                sentinel_password: None,
                read_replica: false,
            },
            ..Default::default()
        })
        .unwrap()
    }

    fn upper(args: &[String]) -> Vec<String> {
        args.iter().map(|arg| arg.to_ascii_uppercase()).collect()
    }

    #[tokio::test]
    async fn a_sentinel_that_refuses_the_login_is_skipped() {
        let refusing = fake_server(|_| Some("-WRONGPASS invalid username-password pair or user is disabled.\r\n".into())).await;
        let open = fake_server(|args| match upper(args).first().map(String::as_str) {
            Some("PING") => Some("+PONG\r\n".into()),
            _ => Some("+OK\r\n".into()),
        })
        .await;
        let resolver = resolver_for(vec![refusing.clone(), open.clone()], None);
        let (answered, _) = resolver.sentinel().await.expect("the second sentinel lets us in");
        assert_eq!(answered, open);
        // Only when none lets us in is it the sentinels' login.
        let resolver = resolver_for(vec![refusing], None);
        let Err(error) = resolver.sentinel().await else {
            panic!("the only sentinel refuses the login");
        };
        assert!(error.to_string().contains("sentinels rejected"), "{error}");
    }

    #[tokio::test]
    async fn a_master_that_never_answers_its_client_name_does_not_hold_the_resolve() {
        // The master answers the connection setup and ROLE but never CLIENT SETNAME.
        let master = fake_server(|args| match upper(args).as_slice() {
            [client, setname, ..] if client == "CLIENT" && setname == "SETNAME" => None,
            [role] if role == "ROLE" => Some("*1\r\n$6\r\nmaster\r\n".into()),
            _ => Some("+OK\r\n".into()),
        })
        .await;
        static MASTER_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
        MASTER_PORT.store(master.port, Ordering::SeqCst);
        let sentinel = fake_server(|args| match upper(args).as_slice() {
            [ping] if ping == "PING" => Some("+PONG\r\n".into()),
            [sentinel, get, ..] if sentinel == "SENTINEL" && get == "GET-MASTER-ADDR-BY-NAME" => {
                let port = MASTER_PORT.load(Ordering::SeqCst).to_string();
                Some(format!("*2\r\n$9\r\n127.0.0.1\r\n${}\r\n{port}\r\n", port.len()))
            }
            _ => Some("+OK\r\n".into()),
        })
        .await;
        let resolver = resolver_for(vec![sentinel], Some("studio"));
        let outcome = tokio::time::timeout(Duration::from_secs(10), resolver.resolve()).await;
        let error = outcome.expect("the resolve is bounded").err().expect("no usable master");
        assert!(error.to_string().contains("SETNAME"), "{error}");
    }
}
