//! Connection options, connecting, and server information.

use std::{collections::BTreeMap, time::Duration};

use redis::{aio::MultiplexedConnection, AsyncConnectionConfig, Client, ConnectionAddr, ConnectionInfo, IntoConnectionInfo, Value};
use serde::{Deserialize, Serialize};

use super::types::{as_items, as_pairs, as_string};
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
    /// Logical database index (`SELECT`), 0 by default. Ignored by Redis Cluster.
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

/// Deployment shape. Only `Standalone` connects today; see the module docs for the Sentinel and Cluster plan.
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
    /// Redis Cluster: seed nodes; the slot map is discovered with `CLUSTER SHARDS`.
    #[serde(rename_all = "camelCase")]
    Cluster { seeds: Vec<HostPort>, read_from_replicas: bool },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostPort {
    pub host: String,
    pub port: u16,
}

impl RedisConnectOptions {
    /// Parses `redis://[user[:password]@]host[:port][/db]` or `rediss://...` (TLS). The password, if any, lands in
    /// `password` so the caller can move it to the secret store.
    pub fn from_url(url: &str) -> AppResult<Self> {
        let parsed = reqwest::Url::parse(url.trim()).map_err(|error| AppError::new(format!("This is not a valid Redis URL: {error}")))?;
        let tls = match parsed.scheme() {
            "redis" | "valkey" => None,
            "rediss" | "valkeys" => Some(RedisTls::default()),
            other => {
                return Err(AppError::new(format!(
                    "Redis URLs start with redis:// or rediss://, not {other}://."
                )))
            }
        };
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
        let mut settings = redis::RedisConnectionInfo::default().set_db(self.db);
        if let Some(username) = self.username.as_deref().filter(|name| !name.is_empty()) {
            settings = settings.set_username(username);
        }
        if let Some(password) = self.password.as_deref().filter(|password| !password.is_empty()) {
            settings = settings.set_password(password);
        }
        if self.resp3 {
            settings = settings.set_protocol(redis::ProtocolVersion::RESP3);
        }
        let info: ConnectionInfo = addr.into_connection_info()?.set_redis_settings(settings);
        let Some(tls) = &self.tls else {
            return Ok(Client::open(info)?);
        };
        let has_certificates = tls.ca_pem.is_some() || tls.client_cert_pem.is_some();
        let mut client = if has_certificates {
            let client_tls = match (&tls.client_cert_pem, &tls.client_key_pem) {
                (Some(cert), Some(key)) => Some(redis::ClientTlsConfig {
                    client_cert: cert.as_bytes().to_vec(),
                    client_key: key.as_bytes().to_vec(),
                }),
                (None, None) => None,
                _ => return Err(AppError::new("Mutual TLS needs both the client certificate and its key.")),
            };
            let certificates = redis::TlsCertificates {
                client_tls,
                root_cert: tls.ca_pem.as_ref().map(|pem| pem.as_bytes().to_vec()),
            };
            Client::build_with_tls(info, certificates)?
        } else {
            Client::open(info)?
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
}

/// An open Redis connection. Browsing and edits share one multiplexed connection (cheap to clone); each console gets its
/// own so `SELECT`, `MULTI` and `CLIENT` state stay in that console.
#[derive(Clone)]
pub struct RedisEngine {
    pub(crate) client: Client,
    pub(crate) conn: MultiplexedConnection,
    pub(crate) options: RedisConnectOptions,
    pub(crate) modules: Vec<RedisModule>,
}

impl RedisEngine {
    pub async fn connect(options: RedisConnectOptions) -> AppResult<Self> {
        match &options.topology {
            RedisTopology::Standalone => {}
            RedisTopology::Sentinel { .. } => {
                return Err(AppError::new(
                    "Redis Sentinel connections are not supported yet. Connect to the master directly.",
                ))
            }
            RedisTopology::Cluster { .. } => {
                return Err(AppError::new(
                    "Redis Cluster connections are not supported yet. Connect to a single node to browse it.",
                ))
            }
        }
        let client = options.client()?;
        let conn = open_connection(&client, &options).await?;
        let mut engine = Self {
            client,
            conn,
            options,
            modules: Vec::new(),
        };
        engine.modules = engine.load_modules().await;
        Ok(engine)
    }

    pub(crate) async fn new_connection(&self) -> AppResult<MultiplexedConnection> {
        open_connection(&self.client, &self.options).await
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

    /// Runs a pipeline; server errors come back as `Value::ServerError` items instead of failing the whole batch.
    pub(crate) async fn pipeline(&self, pipe: &mut redis::Pipeline) -> AppResult<Vec<Value>> {
        let mut conn = self.conn.clone();
        pipe.ignore_errors();
        with_timeout(self.command_timeout(), pipe.query_async::<Vec<Value>>(&mut conn)).await
    }

    pub async fn ping(&self) -> AppResult<()> {
        self.query(&redis::cmd("PING")).await.map(|_| ())
    }

    /// `INFO` parsed into sections, plus the figures the connection view shows.
    pub async fn server_info(&self) -> AppResult<RedisServerInfo> {
        let reply = match self.query(redis::cmd("INFO").arg("everything")).await {
            Ok(reply) => reply,
            Err(_) => self.query(&redis::cmd("INFO")).await?,
        };
        let text = as_string(reply).unwrap_or_default();
        let sections = parse_info(&text);
        let get = |section: &str, key: &str| sections.get(section).and_then(|values| values.get(key)).cloned();
        let number = |section: &str, key: &str| get(section, key).and_then(|value| value.parse::<u64>().ok());
        let databases = self
            .config_get("databases")
            .await
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok());
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
        Ok(RedisServerInfo {
            version: get("server", "valkey_version")
                .or_else(|| get("server", "redis_version"))
                .unwrap_or_default(),
            flavor,
            mode: get("server", "redis_mode").unwrap_or_else(|| "standalone".into()),
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
        })
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

async fn open_connection(client: &Client, options: &RedisConnectOptions) -> AppResult<MultiplexedConnection> {
    let connect_timeout = Duration::from_millis(options.connect_timeout_ms.unwrap_or(DEFAULT_CONNECT_TIMEOUT_MS));
    // The per-command timeout is applied by the callers (the console has its own), so the connection itself waits.
    let config = AsyncConnectionConfig::new()
        .set_connection_timeout(Some(connect_timeout))
        .set_response_timeout(None);
    let mut conn = client.get_multiplexed_async_connection_with_config(&config).await?;
    if let Some(name) = options.client_name.as_deref().filter(|name| !name.is_empty()) {
        let name: String = name.chars().map(|c| if c.is_ascii_graphic() { c } else { '-' }).collect();
        // Older servers or restrictive ACLs may refuse this; the connection is still usable.
        let _ = redis::cmd("CLIENT").arg("SETNAME").arg(name).query_async::<Value>(&mut conn).await;
    }
    Ok(conn)
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
        assert_eq!((plain.port, plain.db, plain.tls.is_none(), plain.username), (None, 0, true, None));
    }
}
