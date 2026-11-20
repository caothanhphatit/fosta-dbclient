//! Connection options, connecting, and server information.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use redis::{
    aio::{ConnectionLike, MultiplexedConnection},
    cluster::{ClusterClient, ClusterClientBuilder, NodeAddress},
    cluster_async::ClusterConnection,
    cluster_read_routing::RandomReplicaStrategy,
    AsyncConnectionConfig, Client, Cmd, ConnectionAddr, ConnectionInfo, IntoConnectionInfo, Pipeline, RedisError, RedisFuture, RedisResult,
    ServerError, Value,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::{
    cluster::{ClusterOverview, ClusterTopology},
    types::{as_items, as_pairs, as_string},
};
use crate::error::{AppError, AppResult};

pub const DEFAULT_PORT: u16 = 6379;
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 30_000;

/// How to reach Redis. The password comes from the secret store and is passed in separately by the caller.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RedisConnectOptions {
    pub host: String,
    pub port: Option<u16>,
    /// ACL user (Redis 6+). Empty or missing means the `default` user.
    pub username: Option<String>,
    #[serde(skip_serializing)]
    pub password: Option<String>,
    /// Logical database index (`SELECT`), 0 by default. Redis Cluster has database 0 only.
    pub db: i64,
    pub tls: Option<RedisTls>,
    /// Ask for RESP3 (`HELLO 3`): typed maps, sets, doubles and booleans in replies. RESP2 otherwise.
    pub resp3: bool,
    pub connect_timeout_ms: Option<u64>,
    /// Default timeout for one command (browsing, edits and the console).
    pub command_timeout_ms: Option<u64>,
    /// Sent as `CLIENT SETNAME` so the connection is recognisable in `CLIENT LIST`.
    pub client_name: Option<String>,
    pub topology: RedisTopology,
    /// Addresses the cluster nodes announce, mapped to the addresses to dial instead (port forwards, NAT). Dial time only:
    /// redirects still name the announced addresses.
    pub address_map: Vec<(HostPort, HostPort)>,
}

/// TLS settings (`rediss://`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RedisTls {
    pub verify: TlsVerify,
    /// PEM CA certificate(s) to trust instead of the system store.
    pub ca_pem: Option<String>,
    /// PEM client certificate chain and key for mutual TLS.
    pub client_cert_pem: Option<String>,
    #[serde(skip_serializing)]
    pub client_key_pem: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TlsVerify {
    /// Verify the certificate chain and the host name.
    #[default]
    Full,
    /// Verify the chain but accept a certificate issued for another host name (IP-only endpoints, tunnels).
    SkipHostname,
    /// Accept any certificate. Only for test servers.
    None,
}

/// Deployment shape. `Standalone` and `Cluster` connect; `Sentinel` is not supported yet.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RedisTopology {
    #[default]
    Standalone,
    /// Ask the sentinels for the current master of `master_name`, then connect to it (or a replica when `read_replica`).
    #[serde(rename_all = "camelCase")]
    Sentinel {
        master_name: String,
        sentinels: Vec<HostPort>,
        sentinel_username: Option<String>,
        #[serde(skip_serializing)]
        sentinel_password: Option<String>,
        read_replica: bool,
    },
    /// Redis Cluster: every seed node (the profile's host and port first); the slot map is discovered from them.
    #[serde(rename_all = "camelCase")]
    Cluster { seeds: Vec<HostPort>, read_from_replicas: bool },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

impl HostPort {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self { host: host.into(), port }
    }

    /// `host:port`, or `host` alone with `default_port`; IPv6 in brackets (`[::1]:7001`).
    pub fn parse(text: &str, default_port: u16) -> AppResult<Self> {
        let text = text.trim();
        let invalid = || AppError::new(format!("`{text}` is not a host:port address."));
        let (host, port) = if let Some(rest) = text.strip_prefix('[') {
            let (host, after) = rest.split_once(']').ok_or_else(invalid)?;
            match after {
                "" => (host, None),
                _ => (host, Some(after.strip_prefix(':').ok_or_else(invalid)?)),
            }
        } else {
            match text.rsplit_once(':') {
                // Two colons without brackets is a bare IPv6 address.
                Some((host, _)) if host.contains(':') => (text, None),
                Some((host, port)) => (host, Some(port)),
                None => (text, None),
            }
        };
        if host.is_empty() || host.contains([',', ' ', '/', '@', '=']) {
            return Err(invalid());
        }
        let port = match port {
            Some(port) => port.parse().ok().filter(|port| *port > 0).ok_or_else(invalid)?,
            None => default_port,
        };
        Ok(Self::new(host, port))
    }

    /// A list separated by commas or whitespace (`h1:7001, h2:7002`).
    pub fn parse_list(text: &str, default_port: u16) -> AppResult<Vec<Self>> {
        text.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(|part| Self::parse(part, default_port))
            .collect()
    }

    /// `announced=dialed` pairs separated by commas or whitespace (`10.0.1.5:6379=127.0.0.1:17001`).
    pub fn parse_map(text: &str) -> AppResult<Vec<(Self, Self)>> {
        text.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(|part| {
                let (from, to) = part
                    .split_once('=')
                    .ok_or_else(|| AppError::new(format!("`{part}` is not an address mapping such as 10.0.1.5:6379=127.0.0.1:17001.")))?;
                Ok((Self::parse(from, DEFAULT_PORT)?, Self::parse(to, DEFAULT_PORT)?))
            })
            .collect()
    }
}

impl std::fmt::Display for HostPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

impl RedisConnectOptions {
    /// Parses `redis://[user[:password]@]host[:port][/db]` or `rediss://...` (TLS). The password, if any, lands in
    /// `password` so the caller can move it to the secret store.
    ///
    /// A Redis Cluster URL lists its seed nodes: `redis+cluster://[user[:password]@]h1:7001,h2:7002[/0]` (also
    /// `rediss+cluster`, `valkey+cluster`, `valkeys+cluster`).
    pub fn from_url(url: &str) -> AppResult<Self> {
        let url = url.trim();
        let Some((scheme, rest)) = url.split_once("://") else {
            return Err(AppError::new(
                "This is not a valid Redis URL: it does not start with redis:// or rediss://.",
            ));
        };
        let scheme = scheme.to_ascii_lowercase();
        if scheme == "redis-sentinel" {
            return Err(sentinel_not_yet());
        }
        let (base, topology) = match scheme.split_once('+') {
            Some((base, topology)) => (base, Some(topology)),
            None => (scheme.as_str(), None),
        };
        let tls = match base {
            "redis" | "valkey" => None,
            "rediss" | "valkeys" => Some(RedisTls::default()),
            other => {
                return Err(AppError::new(format!(
                    "Redis URLs start with redis:// or rediss://, not {other}://."
                )))
            }
        };
        match topology {
            None => {}
            Some("cluster") => return Self::from_cluster_url(rest, tls),
            Some("sentinel") => return Err(sentinel_not_yet()),
            Some(other) => {
                return Err(AppError::new(format!(
                    "Unknown Redis URL topology +{other}: use {base}+cluster:// for a Redis Cluster."
                )))
            }
        }
        if url_authority(rest).1.contains(',') {
            return Err(AppError::new(format!(
                "This Redis URL names several hosts. For a Redis Cluster write {base}+cluster://h1:7001,h2:7002 (Sentinel is not \
                 supported yet)."
            )));
        }
        let parsed = reqwest::Url::parse(url).map_err(|error| AppError::new(format!("This is not a valid Redis URL: {error}")))?;
        let decode = |text: &str| percent_decode(text);
        let username = Some(decode(parsed.username())).filter(|name| !name.is_empty());
        let password = parsed.password().map(decode);
        let db = match parsed.path().trim_matches('/') {
            "" => 0,
            text => text
                .parse()
                .map_err(|_| AppError::new(format!("The database in a Redis URL is a number, not `{text}`.")))?,
        };
        Ok(Self {
            host: parsed.host_str().unwrap_or("127.0.0.1").trim_matches(['[', ']']).to_string(),
            port: parsed.port(),
            username,
            password,
            db,
            tls,
            ..Default::default()
        })
    }

    /// The part of a `+cluster` URL after `://`: user info, comma-separated seeds, optional `/0`.
    fn from_cluster_url(rest: &str, tls: Option<RedisTls>) -> AppResult<Self> {
        let (userinfo, hosts, path) = url_authority(rest);
        let (username, password) = match userinfo {
            None => (None, None),
            Some(info) => match info.split_once(':') {
                Some((user, password)) => (Some(percent_decode(user)), Some(percent_decode(password))),
                None => (Some(percent_decode(info)), None),
            },
        };
        let seeds = hosts
            .split(',')
            .filter(|host| !host.trim().is_empty())
            .map(|host| HostPort::parse(host, DEFAULT_PORT))
            .collect::<AppResult<Vec<_>>>()?;
        let Some(first) = seeds.first().cloned() else {
            return Err(AppError::new(
                "A Redis Cluster URL names at least one node: redis+cluster://host:7001.",
            ));
        };
        match path.trim_matches('/') {
            "" | "0" => {}
            text if text.parse::<i64>().is_ok() => return Err(cluster_db_error()),
            text => return Err(AppError::new(format!("The database in a Redis URL is a number, not `{text}`."))),
        }
        Ok(Self {
            host: first.host,
            port: Some(first.port),
            username: username.filter(|name| !name.is_empty()),
            password,
            tls,
            topology: RedisTopology::Cluster {
                seeds,
                read_from_replicas: false,
            },
            ..Default::default()
        })
    }

    pub fn is_cluster(&self) -> bool {
        matches!(self.topology, RedisTopology::Cluster { .. })
    }

    fn command_timeout(&self) -> Duration {
        Duration::from_millis(self.command_timeout_ms.unwrap_or(DEFAULT_COMMAND_TIMEOUT_MS).max(100))
    }

    pub(crate) fn client(&self) -> AppResult<Client> {
        if self.host.trim().is_empty() {
            return Err(AppError::new("Enter the Redis host."));
        }
        let host = self.host.trim().to_string();
        let port = self.port.unwrap_or(DEFAULT_PORT);
        let addr = match &self.tls {
            None => ConnectionAddr::Tcp(host, port),
            Some(tls) => ConnectionAddr::TcpTls {
                host,
                port,
                insecure: tls.verify == TlsVerify::None,
                tls_params: None,
            },
        };
        let info: ConnectionInfo = addr.into_connection_info()?.set_redis_settings(self.redis_settings(self.db));
        let Some(tls) = &self.tls else {
            return Ok(Client::open(info)?);
        };
        let mut client = match tls_certificates(tls)? {
            Some(certificates) => Client::build_with_tls(info, certificates)?,
            None => Client::open(info)?,
        };
        if tls.verify == TlsVerify::SkipHostname {
            let mut info = client.get_connection_info().clone();
            let mut addr = info.addr().clone();
            addr.set_danger_accept_invalid_hostnames(true);
            info = info.set_addr(addr);
            client = Client::open(info)?;
        }
        Ok(client)
    }

    /// User, password, protocol and database of every connection.
    fn redis_settings(&self, db: i64) -> redis::RedisConnectionInfo {
        let mut settings = redis::RedisConnectionInfo::default().set_db(db);
        if let Some(username) = self.username.as_deref().filter(|name| !name.is_empty()) {
            settings = settings.set_username(username);
        }
        if let Some(password) = self.password.as_deref().filter(|password| !password.is_empty()) {
            settings = settings.set_password(password);
        }
        if self.resp3 {
            settings = settings.set_protocol(redis::ProtocolVersion::RESP3);
        }
        settings
    }

    /// The Redis Cluster client: the seeds, the same user, password and TLS on every node, database 0. Replies wait as long
    /// as the callers' own timeouts allow (no response timeout here, so blocking console commands work).
    pub(crate) fn cluster_client(&self) -> AppResult<ClusterClient> {
        let RedisTopology::Cluster { seeds, read_from_replicas } = &self.topology else {
            return Err(AppError::new("This connection is not a Redis Cluster."));
        };
        if self.db != 0 {
            return Err(cluster_db_error());
        }
        if seeds.is_empty() {
            return Err(AppError::new("Enter the Redis host."));
        }
        let nodes = seeds
            .iter()
            .map(|seed| {
                let (host, port) = (seed.host.clone(), seed.port);
                let addr = match &self.tls {
                    None => ConnectionAddr::Tcp(host, port),
                    Some(tls) => ConnectionAddr::TcpTls {
                        host,
                        port,
                        insecure: tls.verify == TlsVerify::None,
                        tls_params: None,
                    },
                };
                Ok(addr.into_connection_info()?.set_redis_settings(self.redis_settings(0)))
            })
            .collect::<AppResult<Vec<ConnectionInfo>>>()?;
        let protocol = if self.resp3 {
            redis::ProtocolVersion::RESP3
        } else {
            redis::ProtocolVersion::RESP2
        };
        let connect_timeout = Duration::from_millis(self.connect_timeout_ms.unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS));
        let mut builder = ClusterClientBuilder::new(nodes)
            .connection_timeout(connect_timeout)
            .retries(2)
            .database_id(0)
            .use_protocol(protocol);
        if let Some(tls) = &self.tls {
            builder = builder.tls(if tls.verify == TlsVerify::None {
                redis::TlsMode::Insecure
            } else {
                redis::TlsMode::Secure
            });
            if let Some(certificates) = tls_certificates(tls)? {
                builder = builder.certs(certificates);
            }
            if tls.verify == TlsVerify::SkipHostname {
                builder = builder.danger_accept_invalid_hostnames(true);
            }
        }
        if *read_from_replicas {
            builder = builder.read_routing_strategy(RandomReplicaStrategy);
        }
        if !self.address_map.is_empty() {
            let map = self
                .address_map
                .iter()
                .map(|(from, to)| {
                    (
                        NodeAddress::new(from.host.clone(), from.port),
                        NodeAddress::new(to.host.clone(), to.port),
                    )
                })
                .collect();
            builder = builder.node_address_map(map);
        }
        Ok(builder.build()?)
    }
}

/// The CA and client certificate from the profile, `None` when it names neither (system roots, no mutual TLS).
fn tls_certificates(tls: &RedisTls) -> AppResult<Option<redis::TlsCertificates>> {
    if tls.ca_pem.is_none() && tls.client_cert_pem.is_none() {
        return Ok(None);
    }
    let client_tls = match (&tls.client_cert_pem, &tls.client_key_pem) {
        (Some(cert), Some(key)) => Some(redis::ClientTlsConfig {
            client_cert: cert.as_bytes().to_vec(),
            client_key: key.as_bytes().to_vec(),
        }),
        (None, None) => None,
        _ => return Err(AppError::new("Mutual TLS needs both the client certificate and its key.")),
    };
    Ok(Some(redis::TlsCertificates {
        client_tls,
        root_cert: tls.ca_pem.as_ref().map(|pem| pem.as_bytes().to_vec()),
    }))
}

/// A connection the engine runs commands on: one node (standalone, or the master Sentinel resolved) or a whole Redis
/// Cluster, which routes each command to the node that owns its slot. Cheap to clone; clones share the connection.
#[derive(Clone)]
pub enum Conn {
    Node(MultiplexedConnection),
    Cluster(ClusterConnection),
}

impl ConnectionLike for Conn {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        match self {
            Self::Node(conn) => conn.req_packed_command(cmd),
            Self::Cluster(conn) => conn.req_packed_command(cmd),
        }
    }

    fn req_packed_commands<'a>(&'a mut self, pipeline: &'a Pipeline, offset: usize, count: usize) -> RedisFuture<'a, Vec<Value>> {
        match self {
            Self::Node(conn) => conn.req_packed_commands(pipeline, offset, count),
            Self::Cluster(conn) => conn.req_packed_commands(pipeline, offset, count),
        }
    }

    fn get_db(&self) -> i64 {
        match self {
            Self::Node(conn) => conn.get_db(),
            Self::Cluster(conn) => conn.get_db(),
        }
    }
}

/// Where new connections come from.
#[derive(Clone)]
pub enum Backend {
    /// One server, reached through `client`.
    Node { client: Arc<Client> },
    /// A Redis Cluster, discovered from its seed nodes, with the shard map last read (see [`RedisEngine::shards`]).
    Cluster {
        client: Arc<ClusterClient>,
        topology: Arc<Mutex<Option<ClusterTopology>>>,
    },
}

/// An open Redis connection. Browsing and edits share one connection (cheap to clone); each console gets its own so
/// `SELECT`, `MULTI` and `CLIENT` state stay in that console.
#[derive(Clone)]
pub struct RedisEngine {
    pub(crate) backend: Backend,
    pub(crate) conn: Conn,
    pub(crate) options: RedisConnectOptions,
    pub(crate) modules: Vec<RedisModule>,
}

impl RedisEngine {
    pub async fn connect(options: RedisConnectOptions) -> AppResult<Self> {
        let backend = match &options.topology {
            RedisTopology::Standalone => Backend::Node {
                client: Arc::new(options.client()?),
            },
            RedisTopology::Sentinel { .. } => return Err(sentinel_not_yet()),
            RedisTopology::Cluster { .. } => Backend::Cluster {
                client: Arc::new(options.cluster_client()?),
                topology: Arc::default(),
            },
        };
        let conn = open_connection(&backend, &options).await?;
        let mut engine = Self {
            backend,
            conn,
            options,
            modules: Vec::new(),
        };
        engine.modules = engine.load_modules().await;
        Ok(engine)
    }

    /// A fresh connection of its own (consoles, guarded edits): a new node connection, or a new cluster connection.
    pub(crate) async fn new_connection(&self) -> AppResult<Conn> {
        open_connection(&self.backend, &self.options).await
    }

    /// The engine talks to a Redis Cluster.
    pub fn is_cluster(&self) -> bool {
        matches!(self.backend, Backend::Cluster { .. })
    }

    pub fn options(&self) -> &RedisConnectOptions {
        &self.options
    }

    pub fn modules(&self) -> &[RedisModule] {
        &self.modules
    }

    pub fn has_module(&self, name: &str) -> bool {
        self.modules.iter().any(|module| module.name.eq_ignore_ascii_case(name))
    }

    /// RedisJSON is loaded (`ReJSON` module, or built into Redis 8).
    pub fn has_json(&self) -> bool {
        self.has_module("ReJSON")
    }

    pub(crate) fn command_timeout(&self) -> Duration {
        self.options.command_timeout()
    }

    /// Runs one command on the shared connection with the default timeout and returns the raw reply.
    pub(crate) async fn query(&self, cmd: &redis::Cmd) -> AppResult<Value> {
        let mut conn = self.conn.clone();
        with_timeout(self.command_timeout(), cmd.query_async::<Value>(&mut conn)).await
    }

    /// Runs one command on every master: one `(address, reply)` per master, in the order of [`RedisEngine::shards`]. A
    /// single node answers alone.
    pub(crate) async fn query_masters(&self, cmd: &redis::Cmd) -> AppResult<Vec<(String, Value)>> {
        if !self.is_cluster() {
            let reply = self.query(cmd).await?;
            return Ok(vec![(self.address(), reply)]);
        }
        let shards = self.shards(false).await?;
        let replies = shards.iter().map(|shard| self.query_node(cmd, &shard.master));
        let replies = futures_util::future::join_all(replies).await;
        shards
            .iter()
            .zip(replies)
            .map(|(shard, reply)| Ok((shard.master.addr(), reply?)))
            .collect()
    }

    /// `host:port` of the configured endpoint.
    pub(crate) fn address(&self) -> String {
        format!("{}:{}", self.options.host.trim(), self.options.port.unwrap_or(DEFAULT_PORT))
    }

    /// Runs a pipeline; server errors come back as `Value::ServerError` items instead of failing the whole batch.
    ///
    /// On a cluster a non-atomic pipeline may span hash slots, which one cluster pipeline cannot: each command then runs
    /// on its own, concurrently, and the replies come back in order. Ignored commands (`.ignore()`) still answer there
    /// (the crate does not expose which ones are ignored), so callers on a cluster do not use `.ignore()`.
    pub(crate) async fn pipeline(&self, pipe: &mut redis::Pipeline) -> AppResult<Vec<Value>> {
        pipe.ignore_errors();
        match &self.conn {
            Conn::Cluster(conn) if !pipe.is_transaction() => {
                let replies = pipe.cmd_iter().map(|cmd| {
                    let mut conn = conn.clone();
                    async move { conn.req_packed_command(cmd).await }
                });
                let replies = futures_util::future::join_all(replies);
                let replies = with_timeout(self.command_timeout(), async { Ok(replies.await) }).await?;
                replies.into_iter().map(server_error_value).collect()
            }
            conn => {
                let mut conn = conn.clone();
                with_timeout(self.command_timeout(), pipe.query_async::<Vec<Value>>(&mut conn)).await
            }
        }
    }

    pub async fn ping(&self) -> AppResult<()> {
        self.query_masters(&redis::cmd("PING")).await.map(|_| ())
    }

    /// `INFO` parsed into sections, plus the figures the connection view shows.
    /// On a cluster: the first master's `INFO`, the keyspace summed over the masters, and the cluster overview.
    pub async fn server_info(&self) -> AppResult<RedisServerInfo> {
        if self.is_cluster() {
            return self.cluster_server_info().await;
        }
        let text = self.info_text(None).await?;
        let databases = self
            .config_get("databases")
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok());
        Ok(self.server_info_from(&text, databases, self.address()))
    }

    /// `INFO everything` (plain `INFO` on servers without it) of one node, or of the shared connection.
    pub(crate) async fn info_text(&self, node: Option<&super::cluster::NodeInfo>) -> AppResult<String> {
        let everything = redis::cmd("INFO").arg("everything").clone();
        let plain = redis::cmd("INFO");
        let reply = match node {
            Some(node) => match self.query_node(&everything, node).await {
                Ok(reply) => reply,
                Err(_) => self.query_node(&plain, node).await?,
            },
            None => match self.query(&everything).await {
                Ok(reply) => reply,
                Err(_) => self.query(&plain).await?,
            },
        };
        Ok(as_string(reply).unwrap_or_default())
    }

    /// The headline figures of one `INFO` text, answered by `node`.
    pub(crate) fn server_info_from(&self, text: &str, databases: Option<u32>, node: String) -> RedisServerInfo {
        let sections = parse_info(text);
        let get = |section: &str, key: &str| sections.get(section).and_then(|values| values.get(key)).cloned();
        let number = |section: &str, key: &str| get(section, key).and_then(|value| value.parse::<u64>().ok());
        let topology = if self.is_cluster() {
            "cluster"
        } else if get("cluster", "cluster_enabled").as_deref() == Some("1") {
            "clusterNode"
        } else {
            "standalone"
        };
        let keyspace = sections
            .get("keyspace")
            .map(|entries| entries.iter().filter_map(|(db, stats)| parse_keyspace(db, stats)).collect())
            .unwrap_or_default();
        let flavor = match get("server", "server_name").as_deref() {
            Some("valkey") => ServerFlavor::Valkey,
            _ if get("server", "dragonfly_version").is_some() => ServerFlavor::Dragonfly,
            _ if get("server", "redis_version").is_some() => ServerFlavor::Redis,
            _ => ServerFlavor::Unknown,
        };
        RedisServerInfo {
            topology: topology.to_owned(),
            node,
            cluster: None,
            version: get("server", "valkey_version")
                .or_else(|| get("server", "redis_version"))
                .unwrap_or_default(),
            flavor,
            // Valkey 8 names it `server_mode`.
            mode: get("server", "redis_mode")
                .or_else(|| get("server", "server_mode"))
                .unwrap_or_else(|| "standalone".into()),
            role: get("replication", "role").unwrap_or_default(),
            os: get("server", "os"),
            uptime_seconds: number("server", "uptime_in_seconds"),
            connected_clients: number("clients", "connected_clients"),
            used_memory: number("memory", "used_memory"),
            used_memory_peak: number("memory", "used_memory_peak"),
            maxmemory: number("memory", "maxmemory"),
            maxmemory_policy: get("memory", "maxmemory_policy"),
            total_commands_processed: number("stats", "total_commands_processed"),
            keyspace_hits: number("stats", "keyspace_hits"),
            keyspace_misses: number("stats", "keyspace_misses"),
            databases,
            current_db: self.options.db,
            keyspace,
            modules: self.modules.clone(),
            sections,
        }
    }

    /// `CONFIG GET name` (single value). `Ok(None)` when the server does not know it; errors when ACLs forbid `CONFIG`.
    pub async fn config_get(&self, name: &str) -> AppResult<Option<String>> {
        let reply = self.query(redis::cmd("CONFIG").arg("GET").arg(name)).await?;
        Ok(as_pairs(reply).into_iter().next().and_then(|(_, value)| as_string(value)))
    }

    async fn load_modules(&self) -> Vec<RedisModule> {
        let Ok(reply) = self.query(redis::cmd("MODULE").arg("LIST")).await else {
            return Vec::new();
        };
        as_items(reply)
            .into_iter()
            .map(|module| {
                let fields: BTreeMap<String, Value> = as_pairs(module)
                    .into_iter()
                    .filter_map(|(key, value)| as_string(key).map(|key| (key, value)))
                    .collect();
                RedisModule {
                    name: fields.get("name").cloned().and_then(as_string).unwrap_or_default(),
                    version: fields.get("ver").and_then(super::types::as_i64),
                }
            })
            .filter(|module| !module.name.is_empty())
            .collect()
    }
}

/// Splits what follows `scheme://` into the user info (before the last `@`), the hosts and the path (without query or
/// fragment).
fn url_authority(rest: &str) -> (Option<&str>, &str, &str) {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let path = &path[..path.find(['?', '#']).unwrap_or(path.len())];
    match authority.rsplit_once('@') {
        Some((userinfo, hosts)) => (Some(userinfo), hosts, path),
        None => (None, authority, path),
    }
}

pub(crate) fn sentinel_not_yet() -> AppError {
    AppError::new("Redis Sentinel connections are not supported yet. Connect to the master directly.")
}

pub(crate) fn cluster_db_error() -> AppError {
    AppError::new("Redis Cluster has only database 0.")
}

/// Decodes `%XX` escapes in URL user names and passwords.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |byte: u8| (byte as char).to_digit(16);
        match (
            bytes[index],
            bytes.get(index + 1).copied().and_then(hex),
            bytes.get(index + 2).copied().and_then(hex),
        ) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                index += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A server error reply stays a `Value::ServerError` item; any other failure (connection, timeout) fails the batch.
fn server_error_value(reply: RedisResult<Value>) -> AppResult<Value> {
    match reply {
        Ok(value) => Ok(value),
        Err(error) => ServerError::try_from(error)
            .map(Value::ServerError)
            .map_err(|error: RedisError| error.into()),
    }
}

async fn open_connection(backend: &Backend, options: &RedisConnectOptions) -> AppResult<Conn> {
    let connect_timeout = Duration::from_millis(options.connect_timeout_ms.unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS));
    let client_name = options
        .client_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| name.chars().map(|c| if c.is_ascii_graphic() { c } else { '-' }).collect::<String>());
    let client = match backend {
        Backend::Node { client } => client,
        Backend::Cluster { client, .. } => {
            let mut conn = with_timeout(connect_timeout, client.get_async_connection()).await?;
            if let Some(name) = client_name {
                // The crate sends it to every node it knows now (best effort; nodes connected later stay unnamed).
                let _ = with_timeout(
                    connect_timeout,
                    redis::cmd("CLIENT").arg("SETNAME").arg(name).query_async::<Value>(&mut conn),
                )
                .await;
            }
            return Ok(Conn::Cluster(conn));
        }
    };
    // The per-command timeout is applied by the callers (the console has its own), so the connection itself waits.
    let config = AsyncConnectionConfig::new()
        .set_connection_timeout(Some(connect_timeout))
        .set_response_timeout(None);
    let mut conn = client.get_multiplexed_async_connection_with_config(&config).await?;
    if let Some(name) = client_name {
        // Older servers or restrictive ACLs may refuse this; the connection is still usable.
        let _ = redis::cmd("CLIENT").arg("SETNAME").arg(name).query_async::<Value>(&mut conn).await;
    }
    Ok(Conn::Node(conn))
}

pub(crate) async fn with_timeout<T>(timeout: Duration, future: impl std::future::Future<Output = redis::RedisResult<T>>) -> AppResult<T> {
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => Ok(result?),
        Err(_) => Err(AppError::new(format!("Redis did not answer within {} ms.", timeout.as_millis()))),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisModule {
    pub name: String,
    pub version: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerFlavor {
    Redis,
    Valkey,
    Dragonfly,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbKeyspace {
    pub db: i64,
    pub keys: u64,
    pub expires: u64,
    pub avg_ttl_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisServerInfo {
    /// How the connection reaches Redis: `standalone`, `cluster`, or `clusterNode` (a single node of a cluster, connected
    /// as standalone: keys on other nodes answer MOVED).
    pub topology: String,
    /// `host:port` of the node whose `INFO` this is.
    pub node: String,
    /// Slot coverage and shards, on a cluster connection.
    pub cluster: Option<ClusterOverview>,
    pub version: String,
    pub flavor: ServerFlavor,
    /// `standalone`, `sentinel` or `cluster`.
    pub mode: String,
    /// `master` or `slave` (replica).
    pub role: String,
    pub os: Option<String>,
    pub uptime_seconds: Option<u64>,
    pub connected_clients: Option<u64>,
    pub used_memory: Option<u64>,
    pub used_memory_peak: Option<u64>,
    pub maxmemory: Option<u64>,
    pub maxmemory_policy: Option<String>,
    pub total_commands_processed: Option<u64>,
    pub keyspace_hits: Option<u64>,
    pub keyspace_misses: Option<u64>,
    /// `CONFIG GET databases`; `None` when `CONFIG` is not allowed.
    pub databases: Option<u32>,
    pub current_db: i64,
    pub keyspace: Vec<DbKeyspace>,
    pub modules: Vec<RedisModule>,
    /// Every `INFO` section (lower-case name) with its raw fields.
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
}

/// Parses `INFO` text: `# Section` headers and `key:value` lines.
pub fn parse_info(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut sections: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut current = String::from("default");
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('#') {
            current = name.trim().to_ascii_lowercase();
            sections.entry(current.clone()).or_default();
        } else if let Some((key, value)) = line.split_once(':') {
            sections
                .entry(current.clone())
                .or_default()
                .insert(key.to_string(), value.to_string());
        }
    }
    sections
}

/// `db0` + `keys=1,expires=0,avg_ttl=0`.
pub(crate) fn parse_keyspace(db: &str, stats: &str) -> Option<DbKeyspace> {
    let db = db.strip_prefix("db")?.parse().ok()?;
    let mut parsed = DbKeyspace {
        db,
        keys: 0,
        expires: 0,
        avg_ttl_ms: 0,
    };
    for part in stats.split(',') {
        let (key, value) = part.split_once('=')?;
        let value = value.parse().unwrap_or(0);
        match key {
            "keys" => parsed.keys = value,
            "expires" => parsed.expires = value,
            "avg_ttl" => parsed.avg_ttl_ms = value,
            _ => {}
        }
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_info_and_keyspace() {
        let text = "# Server\r\nredis_version:7.4.1\r\nredis_mode:standalone\r\n\r\n# Keyspace\r\ndb0:keys=12,expires=2,avg_ttl=500\r\n";
        let sections = parse_info(text);
        assert_eq!(sections["server"]["redis_version"], "7.4.1");
        let db = parse_keyspace("db0", &sections["keyspace"]["db0"]).unwrap();
        assert_eq!((db.db, db.keys, db.expires, db.avg_ttl_ms), (0, 12, 2, 500));
    }

    #[test]
    fn parses_urls() {
        let options = RedisConnectOptions::from_url("rediss://app:p%40ss@cache.example.com:6380/3").unwrap();
        assert_eq!(options.host, "cache.example.com");
        assert_eq!(options.port, Some(6380));
        assert_eq!(options.username.as_deref(), Some("app"));
        assert_eq!(options.password.as_deref(), Some("p@ss"));
        assert_eq!(options.db, 3);
        assert!(options.tls.is_some());
        assert!(RedisConnectOptions::from_url("http://x").is_err());
        let plain = RedisConnectOptions::from_url("redis://localhost").unwrap();
        assert_eq!(
            (plain.port, plain.db, plain.tls.is_none(), plain.username.as_deref()),
            (None, 0, true, None)
        );
        assert!(!plain.is_cluster());
    }

    #[test]
    fn parses_cluster_urls() {
        let options = RedisConnectOptions::from_url("rediss+cluster://app:p%40ss@h1:7001,h2,[::1]:7003/0?x=1").unwrap();
        assert_eq!((options.host.as_str(), options.port), ("h1", Some(7001)));
        assert_eq!(options.username.as_deref(), Some("app"));
        assert_eq!(options.password.as_deref(), Some("p@ss"));
        assert!(options.tls.is_some());
        let RedisTopology::Cluster { seeds, read_from_replicas } = &options.topology else {
            panic!("cluster");
        };
        assert_eq!(
            seeds,
            &vec![HostPort::new("h1", 7001), HostPort::new("h2", 6379), HostPort::new("::1", 7003)]
        );
        assert!(!read_from_replicas);
        assert_eq!(seeds[2].to_string(), "[::1]:7003");
        let valkey = RedisConnectOptions::from_url("VALKEY+CLUSTER://:secret@10.0.0.1:7001").unwrap();
        assert_eq!((valkey.username.as_deref(), valkey.password.as_deref()), (None, Some("secret")));
        assert!(valkey.tls.is_none() && valkey.is_cluster());
        let db = RedisConnectOptions::from_url("redis+cluster://h1:7001/2").unwrap_err();
        assert_eq!(db.to_string(), "Redis Cluster has only database 0.");
        assert!(RedisConnectOptions::from_url("redis+cluster://").is_err());
        assert!(RedisConnectOptions::from_url("redis+cluster://h1:port").is_err());
        assert!(RedisConnectOptions::from_url("redis+mesh://h1").is_err());
        let several = RedisConnectOptions::from_url("redis://h1:7001,h2:7002").unwrap_err();
        assert!(several.to_string().contains("redis+cluster://"), "{several}");
        for sentinel in ["redis+sentinel://s1:26379/fosta", "redis-sentinel://s1:26379#fosta"] {
            assert!(RedisConnectOptions::from_url(sentinel)
                .unwrap_err()
                .to_string()
                .contains("not supported yet"));
        }
    }

    #[test]
    fn parses_host_ports_and_address_maps() {
        assert_eq!(HostPort::parse("cache:7001", 6379).unwrap(), HostPort::new("cache", 7001));
        assert_eq!(HostPort::parse("cache", 26379).unwrap(), HostPort::new("cache", 26379));
        assert_eq!(HostPort::parse("[fe80::1]", 6379).unwrap(), HostPort::new("fe80::1", 6379));
        assert_eq!(HostPort::parse("fe80::1", 6379).unwrap(), HostPort::new("fe80::1", 6379));
        for bad in ["", ":7001", "h:0", "h:99999", "[::1", "[::1]x"] {
            assert!(HostPort::parse(bad, 6379).is_err(), "{bad}");
        }
        assert_eq!(HostPort::parse_list(" a:1,b:2  c ", 6379).unwrap().len(), 3);
        assert!(HostPort::parse_list("", 6379).unwrap().is_empty());
        let map = HostPort::parse_map("10.0.1.5:6379=127.0.0.1:17001 10.0.1.6=localhost:17002").unwrap();
        assert_eq!(map[1], (HostPort::new("10.0.1.6", 6379), HostPort::new("localhost", 17002)));
        assert!(HostPort::parse_map("10.0.1.5:6379").is_err());
    }
}
