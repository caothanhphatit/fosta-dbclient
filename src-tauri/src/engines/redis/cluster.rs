//! Redis Cluster: the shard map (`CLUSTER SHARDS`, `CLUSTER SLOTS` before Redis 7), commands sent to one chosen node, the
//! composite `SCAN` cursor and the overview the server view shows.
//!
//! The crate's cluster connection routes keyed commands to the slot's master by itself; what it cannot do is iterate the
//! keyspace (`SCAN` is per node) or say which node answered, so those go through [`RedisEngine::query_node`] with the
//! shard map read here. The map is cached for a few seconds and read again when a node id is unknown.

use std::time::{Duration, Instant};

use redis::{
    cluster_routing::{RoutingInfo, SingleNodeRoutingInfo},
    Value,
};
use serde::{Deserialize, Serialize};

use super::{
    connection::{
        parse_info, parse_keyspace, with_timeout, Backend, Conn, DbKeyspace, HostPort, RedisConnectOptions, RedisEngine, RedisServerInfo,
        RedisTopology,
    },
    types::{as_i64, as_items, as_pairs, as_string},
};
use crate::error::{AppError, AppResult};

/// How long a shard map is trusted before the next read asks the cluster again.
const TOPOLOGY_TTL: Duration = Duration::from_secs(5);

/// One cluster node as the shard map names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInfo {
    /// The 40-character node id (empty when the server did not say).
    pub id: String,
    pub host: String,
    pub port: u16,
    /// `host:port`, the address the cluster announces for the node.
    pub addr: String,
    /// `master` or `replica`.
    pub role: String,
    /// `online`, `failed` or `loading` (`CLUSTER SHARDS`); `None` from `CLUSTER SLOTS`.
    pub health: Option<String>,
}

impl NodeInfo {
    fn new(id: String, host: String, port: u16, role: &str, health: Option<String>) -> Self {
        let addr = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        Self {
            id,
            host,
            port,
            addr,
            role: role.to_owned(),
            health,
        }
    }

    pub fn addr(&self) -> String {
        self.addr.clone()
    }
}

/// A master, its replicas and the slot ranges it serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shard {
    pub master: NodeInfo,
    pub replicas: Vec<NodeInfo>,
    pub slots: Vec<(u16, u16)>,
}

/// The shard map, sorted by master node id (the order the composite SCAN cursor walks).
pub struct ClusterTopology {
    pub shards: Vec<Shard>,
    pub fetched: Instant,
}

fn field_map(value: Value) -> Vec<(String, Value)> {
    as_pairs(value)
        .into_iter()
        .filter_map(|(key, value)| as_string(key).map(|key| (key.to_ascii_lowercase(), value)))
        .collect()
}

fn take(fields: &mut Vec<(String, Value)>, name: &str) -> Option<Value> {
    let index = fields.iter().position(|(key, _)| key == name)?;
    Some(fields.swap_remove(index).1)
}

fn text_field(fields: &mut Vec<(String, Value)>, name: &str) -> Option<String> {
    take(fields, name)
        .and_then(as_string)
        .filter(|text| !text.is_empty() && text != "?")
}

fn port_of(value: Option<Value>) -> Option<u16> {
    value
        .as_ref()
        .and_then(as_i64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port > 0)
}

/// Pairs of slot numbers (`[0, 5460, 10923, 10924]` is two ranges).
fn slot_ranges(value: Option<Value>) -> Vec<(u16, u16)> {
    let numbers: Vec<u16> = value
        .map(as_items)
        .unwrap_or_default()
        .iter()
        .filter_map(as_i64)
        .filter_map(|slot| u16::try_from(slot).ok())
        .collect();
    numbers.as_chunks::<2>().0.iter().map(|[start, end]| (*start, *end)).collect()
}

/// Sorted, without masters that serve no slot: the cluster client only connects to nodes in the slot map (a node just
/// added, or a replica still joining, which shows up as an empty master for a moment).
fn sorted(mut shards: Vec<Shard>) -> Vec<Shard> {
    shards.retain(|shard| !shard.slots.is_empty());
    for shard in &mut shards {
        shard.slots.sort_unstable();
        shard.replicas.sort_by(|a, b| a.id.cmp(&b.id).then(a.addr.cmp(&b.addr)));
    }
    shards.sort_by(|a, b| a.master.id.cmp(&b.master.id).then(a.master.addr.cmp(&b.master.addr)));
    shards
}

/// `CLUSTER SHARDS` (Redis 7+, Valkey): one entry per shard with `slots` and `nodes`. A shard whose nodes are all
/// replicas (its master failed and no replica took over yet) or that serves no slot is left out. With `tls`, nodes
/// listen on their `tls-port`.
pub fn parse_shards(reply: Value, tls: bool) -> Vec<Shard> {
    let mut shards = Vec::new();
    for shard in as_items(reply) {
        let mut fields = field_map(shard);
        let slots = slot_ranges(take(&mut fields, "slots"));
        let mut master = None;
        let mut replicas = Vec::new();
        for node in take(&mut fields, "nodes").map(as_items).unwrap_or_default() {
            let mut node = field_map(node);
            let host = text_field(&mut node, "endpoint").or_else(|| text_field(&mut node, "ip"));
            let plain = port_of(take(&mut node, "port"));
            let secure = port_of(take(&mut node, "tls-port"));
            let port = if tls { secure.or(plain) } else { plain.or(secure) };
            let (Some(host), Some(port)) = (host, port) else {
                continue;
            };
            let id = text_field(&mut node, "id").unwrap_or_default();
            let role = text_field(&mut node, "role").unwrap_or_default();
            let health = text_field(&mut node, "health");
            if role == "master" && master.is_none() {
                master = Some(NodeInfo::new(id, host, port, "master", health));
            } else {
                replicas.push(NodeInfo::new(id, host, port, "replica", health));
            }
        }
        if let Some(master) = master {
            shards.push(Shard { master, replicas, slots });
        }
    }
    sorted(shards)
}

/// `CLUSTER SLOTS` (older servers): one entry per slot range, `[start, end, master, replica...]` with each node as
/// `[host, port, id, ...]`. Ranges of the same master make one shard.
pub fn parse_slots(reply: Value) -> Vec<Shard> {
    let mut shards: Vec<Shard> = Vec::new();
    for range in as_items(reply) {
        let mut parts = as_items(range).into_iter();
        let (Some(start), Some(end)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Some(start), Some(end)) = (
            as_i64(&start).and_then(|slot| u16::try_from(slot).ok()),
            as_i64(&end).and_then(|slot| u16::try_from(slot).ok()),
        ) else {
            continue;
        };
        let nodes: Vec<NodeInfo> = parts
            .enumerate()
            .filter_map(|(index, node)| {
                let mut node = as_items(node).into_iter();
                let host = node.next().and_then(as_string).filter(|host| !host.is_empty() && host != "?")?;
                let port = port_of(node.next())?;
                let id = node.next().and_then(as_string).unwrap_or_default();
                Some(NodeInfo::new(id, host, port, if index == 0 { "master" } else { "replica" }, None))
            })
            .collect();
        let mut nodes = nodes.into_iter();
        let Some(master) = nodes.next() else {
            continue;
        };
        let same = |shard: &&mut Shard| {
            if master.id.is_empty() {
                shard.master.addr == master.addr
            } else {
                shard.master.id == master.id
            }
        };
        match shards.iter_mut().find(|shard| same(shard)) {
            Some(shard) => {
                shard.slots.push((start, end));
                for replica in nodes {
                    if !shard.replicas.iter().any(|known| known.addr == replica.addr) {
                        shard.replicas.push(replica);
                    }
                }
            }
            None => shards.push(Shard {
                master,
                replicas: nodes.collect(),
                slots: vec![(start, end)],
            }),
        }
    }
    sorted(shards)
}

/// Where a cluster key scan stands: at the start, or on one master (by node id) at its own SCAN cursor.
#[derive(Debug, PartialEq, Eq)]
pub enum ClusterCursor {
    Start,
    At { node: String, cursor: String },
}

/// `""` and `"0"` start a scan; `<nodeId>@<nodeCursor>` continues one.
pub fn parse_cursor(text: &str) -> AppResult<ClusterCursor> {
    let text = text.trim();
    if text.is_empty() || text == "0" {
        return Ok(ClusterCursor::Start);
    }
    match text.split_once('@') {
        Some((node, cursor)) if !node.is_empty() && !cursor.is_empty() && cursor.bytes().all(|b| b.is_ascii_digit()) => {
            Ok(ClusterCursor::At {
                node: node.to_owned(),
                cursor: cursor.to_owned(),
            })
        }
        _ => Err(AppError::new(
            "This scan cursor does not belong to a Redis Cluster scan. Start the listing again.",
        )),
    }
}

pub fn cursor_text(node: &str, cursor: &str) -> String {
    format!("{node}@{cursor}")
}

/// One shard in the overview: its nodes, slots, and the master's key count and memory.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardOverview {
    pub master: NodeInfo,
    pub replicas: Vec<NodeInfo>,
    pub slots: Vec<(u16, u16)>,
    pub slot_count: u32,
    pub keys: Option<u64>,
    pub used_memory: Option<u64>,
}

/// `CLUSTER INFO` headline figures and the shards.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterOverview {
    /// `ok` or `fail`.
    pub state: String,
    pub slots_assigned: Option<u64>,
    pub slots_ok: Option<u64>,
    pub known_nodes: Option<u64>,
    pub shards: Vec<ShardOverview>,
}

impl RedisEngine {
    /// The cluster's shards, sorted by master id; from the cache unless `refresh` or older than a few seconds. A single
    /// node is one shard of its own.
    pub(crate) async fn shards(&self, refresh: bool) -> AppResult<Vec<Shard>> {
        let Backend::Cluster { topology, .. } = &self.backend else {
            let host = self.options.host.trim().to_owned();
            let port = self.options.port.unwrap_or(super::connection::DEFAULT_PORT);
            return Ok(vec![Shard {
                master: NodeInfo::new(String::new(), host, port, "master", None),
                replicas: Vec::new(),
                slots: Vec::new(),
            }]);
        };
        let mut cached = topology.lock().await;
        if let Some(known) = cached.as_ref().filter(|known| !refresh && known.fetched.elapsed() < TOPOLOGY_TTL) {
            return Ok(known.shards.clone());
        }
        let tls = self.options.tls.is_some();
        let shards = match self.query(redis::cmd("CLUSTER").arg("SHARDS")).await {
            Ok(reply) => parse_shards(reply, tls),
            Err(_) => Vec::new(),
        };
        let shards = if shards.is_empty() {
            parse_slots(self.query(redis::cmd("CLUSTER").arg("SLOTS")).await?)
        } else {
            shards
        };
        if shards.is_empty() {
            return Err(AppError::new(
                "The Redis Cluster reported no shards (CLUSTER SHARDS and CLUSTER SLOTS are empty).",
            ));
        }
        *cached = Some(ClusterTopology {
            shards: shards.clone(),
            fetched: Instant::now(),
        });
        Ok(shards)
    }

    /// Runs one command on the given node (on a single-node connection, on that node). A node the cluster client has no
    /// connection to (a replica still syncing, which `CLUSTER SLOTS` leaves out) is reached with a connection of its own.
    pub(crate) async fn query_node(&self, cmd: &redis::Cmd, node: &NodeInfo) -> AppResult<Value> {
        let Conn::Cluster(conn) = &self.conn else {
            return self.query(cmd).await;
        };
        let mut conn = conn.clone();
        let routing = RoutingInfo::SingleNode(SingleNodeRoutingInfo::ByAddress {
            host: node.host.clone(),
            port: node.port,
        });
        let routed = tokio::time::timeout(self.command_timeout(), conn.route_command(cmd.clone(), routing)).await;
        match routed {
            Ok(Err(error)) if error.kind() == redis::ErrorKind::ClusterConnectionNotFound => self.query_node_directly(cmd, node).await,
            Ok(result) => Ok(result?),
            Err(_) => Err(AppError::new(format!(
                "Redis did not answer within {} ms.",
                self.command_timeout().as_millis()
            ))),
        }
    }

    /// One command on a one-off connection to `node` (its address mapped through the address map), with the profile's
    /// credentials and TLS.
    async fn query_node_directly(&self, cmd: &redis::Cmd, node: &NodeInfo) -> AppResult<Value> {
        let announced = HostPort::new(node.host.clone(), node.port);
        let dialed = self
            .options
            .address_map
            .iter()
            .find(|(from, _)| *from == announced)
            .map_or(announced.clone(), |(_, to)| to.clone());
        let options = RedisConnectOptions {
            host: dialed.host,
            port: Some(dialed.port),
            db: 0,
            topology: RedisTopology::Standalone,
            address_map: Vec::new(),
            client_name: None,
            ..self.options.clone()
        };
        let client = options.client()?;
        let connect_timeout = Duration::from_millis(options.connect_timeout_ms.unwrap_or(10_000));
        let config = redis::AsyncConnectionConfig::new()
            .set_connection_timeout(Some(connect_timeout))
            .set_response_timeout(None);
        let mut conn = client.get_multiplexed_async_connection_with_config(&config).await?;
        with_timeout(self.command_timeout(), cmd.query_async::<Value>(&mut conn)).await
    }

    /// Every node: masters first, then replicas.
    pub(crate) async fn all_nodes(&self) -> AppResult<Vec<NodeInfo>> {
        let shards = self.shards(false).await?;
        let mut nodes: Vec<NodeInfo> = shards.iter().map(|shard| shard.master.clone()).collect();
        nodes.extend(shards.into_iter().flat_map(|shard| shard.replicas));
        Ok(nodes)
    }

    /// The node at `addr` (`host:port`), reading the shard map again when it is not in the cached one.
    pub(crate) async fn find_node(&self, addr: &str) -> AppResult<NodeInfo> {
        let addr = addr.trim();
        for refresh in [false, true] {
            let shards = self.shards(refresh).await?;
            let found = shards
                .iter()
                .flat_map(|shard| std::iter::once(&shard.master).chain(&shard.replicas))
                .find(|node| node.addr == addr);
            if let Some(node) = found {
                return Ok(node.clone());
            }
        }
        Err(AppError::new(format!("{addr} is not a node of this Redis Cluster.")))
    }

    /// `INFO` of one node (its own figures, no cluster overview).
    pub(crate) async fn node_server_info(&self, addr: &str) -> AppResult<RedisServerInfo> {
        let node = self.find_node(addr).await?;
        let text = self.info_text(Some(&node)).await?;
        Ok(self.server_info_from(&text, Some(1), node.addr()))
    }

    /// The first master's `INFO`, the keyspace summed over the masters, and the cluster overview.
    pub(crate) async fn cluster_server_info(&self) -> AppResult<RedisServerInfo> {
        let shards = self.shards(true).await?;
        let first = &shards[0].master;
        let text = self.info_text(Some(first)).await?;
        let mut info = self.server_info_from(&text, Some(1), first.addr());
        let cluster_info = self
            .query(redis::cmd("CLUSTER").arg("INFO"))
            .await
            .ok()
            .and_then(as_string)
            .unwrap_or_default();
        let cluster_info = parse_info(&cluster_info).remove("default").unwrap_or_default();
        let number = |key: &str| cluster_info.get(key).and_then(|value| value.parse::<u64>().ok());
        let plain_info = redis::cmd("INFO");
        let per_master = futures_util::future::join_all(shards.iter().map(|shard| self.query_node(&plain_info, &shard.master))).await;
        let mut total = DbKeyspace {
            db: 0,
            keys: 0,
            expires: 0,
            avg_ttl_ms: 0,
        };
        let mut overview = Vec::with_capacity(shards.len());
        for (shard, reply) in shards.into_iter().zip(per_master) {
            let sections = reply.ok().and_then(as_string).map(|text| parse_info(&text));
            let keyspace = sections.as_ref().and_then(|sections| {
                let stats = sections.get("keyspace")?.get("db0")?;
                parse_keyspace("db0", stats)
            });
            if let Some(keyspace) = &keyspace {
                total.keys += keyspace.keys;
                total.expires += keyspace.expires;
            }
            let used_memory = sections
                .as_ref()
                .and_then(|sections| sections.get("memory")?.get("used_memory")?.parse().ok());
            let keys = match (&sections, keyspace) {
                (_, Some(keyspace)) => Some(keyspace.keys),
                (Some(_), None) => Some(0),
                (None, None) => None,
            };
            overview.push(ShardOverview {
                slot_count: shard.slots.iter().map(|(start, end)| u32::from(end - start) + 1).sum(),
                master: shard.master,
                replicas: shard.replicas,
                slots: shard.slots,
                keys,
                used_memory,
            });
        }
        info.keyspace = vec![total];
        info.current_db = 0;
        info.cluster = Some(ClusterOverview {
            state: cluster_info.get("cluster_state").cloned().unwrap_or_default(),
            slots_assigned: number("cluster_slots_assigned"),
            slots_ok: number("cluster_slots_ok"),
            known_nodes: number("cluster_known_nodes"),
            shards: overview,
        });
        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(text: &str) -> Value {
        Value::BulkString(text.as_bytes().to_vec())
    }

    fn node(id: &str, port: i64, role: &str, health: &str) -> Value {
        Value::Array(vec![
            bulk("id"),
            bulk(id),
            bulk("port"),
            Value::Int(port),
            bulk("ip"),
            bulk("127.0.0.1"),
            bulk("endpoint"),
            bulk("127.0.0.1"),
            bulk("role"),
            bulk(role),
            bulk("replication-offset"),
            Value::Int(42),
            bulk("health"),
            bulk(health),
        ])
    }

    fn shard(slots: &[i64], nodes: Vec<Value>) -> Value {
        Value::Array(vec![
            bulk("slots"),
            Value::Array(slots.iter().map(|slot| Value::Int(*slot)).collect()),
            bulk("nodes"),
            Value::Array(nodes),
        ])
    }

    #[test]
    fn cluster_shards_parse_into_sorted_shards() {
        let reply = Value::Array(vec![
            shard(
                &[5461, 10922],
                vec![node("bbb", 7002, "master", "online"), node("eee", 7005, "replica", "online")],
            ),
            shard(
                &[0, 5460],
                vec![node("aaa", 7001, "master", "online"), node("ddd", 7004, "replica", "loading")],
            ),
            // A shard whose master failed and no replica took over, and a master without slots: left out.
            shard(&[], vec![node("zzz", 7009, "replica", "failed")]),
            shard(&[], vec![node("000", 7010, "master", "online")]),
        ]);
        let shards = parse_shards(reply, false);
        assert_eq!(shards.len(), 2);
        assert_eq!(shards[0].master.id, "aaa");
        assert_eq!(shards[0].master.addr, "127.0.0.1:7001");
        assert_eq!(shards[0].slots, vec![(0, 5460)]);
        assert_eq!(shards[0].replicas[0].health.as_deref(), Some("loading"));
        assert_eq!(shards[1].replicas[0].role, "replica");
        // RESP3 maps parse the same way.
        let resp3 = Value::Array(vec![Value::Map(vec![
            (bulk("slots"), Value::Array(vec![Value::Int(0), Value::Int(16383)])),
            (
                bulk("nodes"),
                Value::Array(vec![Value::Map(vec![
                    (bulk("id"), bulk("abc")),
                    (bulk("port"), Value::Int(7001)),
                    (bulk("tls-port"), Value::Int(8001)),
                    (bulk("ip"), bulk("10.0.0.1")),
                    (bulk("endpoint"), bulk("?")),
                    (bulk("role"), bulk("master")),
                    (bulk("health"), bulk("online")),
                ])]),
            ),
        ])]);
        let shards = parse_shards(resp3.clone(), false);
        assert_eq!(shards[0].master.addr, "10.0.0.1:7001");
        assert_eq!(parse_shards(resp3, true)[0].master.port, 8001);
    }

    #[test]
    fn cluster_slots_group_ranges_by_master() {
        let master = |port: i64, id: &str| Value::Array(vec![bulk("127.0.0.1"), Value::Int(port), bulk(id)]);
        let reply = Value::Array(vec![
            Value::Array(vec![Value::Int(0), Value::Int(100), master(7002, "b"), master(7005, "e")]),
            Value::Array(vec![Value::Int(101), Value::Int(200), master(7001, "a"), master(7004, "d")]),
            Value::Array(vec![Value::Int(300), Value::Int(400), master(7002, "b"), master(7005, "e")]),
        ]);
        let shards = parse_slots(reply);
        assert_eq!(shards.len(), 2);
        assert_eq!(shards[0].master.id, "a");
        assert_eq!(shards[1].slots, vec![(0, 100), (300, 400)]);
        assert_eq!(shards[1].replicas.len(), 1);
        assert_eq!(shards[1].replicas[0].addr, "127.0.0.1:7005");
        assert!(shards[1].master.health.is_none());
    }

    #[test]
    fn cursors_round_trip() {
        assert_eq!(parse_cursor("").unwrap(), ClusterCursor::Start);
        assert_eq!(parse_cursor("0").unwrap(), ClusterCursor::Start);
        let text = cursor_text("e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca", "1792");
        assert_eq!(
            parse_cursor(&text).unwrap(),
            ClusterCursor::At {
                node: "e7d1eecce10fd6bb5eb35b9f99a514335d9ba9ca".into(),
                cursor: "1792".into()
            }
        );
        // A node's own cursor 0 means "start that node", not "start over".
        assert!(matches!(parse_cursor("abc@0").unwrap(), ClusterCursor::At { cursor, .. } if cursor == "0"));
        for bad in ["17", "abc@", "@5", "abc@x1"] {
            assert!(parse_cursor(bad).is_err(), "{bad}");
        }
    }
}
