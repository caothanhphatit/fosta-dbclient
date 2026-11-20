# Redis

Fosta Studio talks to Redis 7.4 and 8.x, and to Valkey and other RESP servers, over the `redis` crate (rustls for TLS).
The engine id is `redis`; the code is in `src-tauri/src/engines/redis/` and the UI in `src/engines/redis/`.

## Scope

**Supported:** a standalone server, a **Redis Cluster** or a master behind **Redis Sentinel** (Valkey too), with a password and
an optional ACL user, TLS (`rediss://`), RESP2 or RESP3. A standalone server or a Sentinel master shows all its logical databases
(`db0` to `db15` by default); a cluster has database 0 only.

**Not supported:** a Redis Cluster or Sentinel connection **through an SSH tunnel** (the nodes announce their own addresses, which
one tunnel cannot reach: forward each node yourself and fill **Address map**, or connect to one node as Standalone). On a
cluster: `MULTI`/`EXEC`, `WATCH` and `SCAN` in the console (the key browser scans every shard), vector search, and commands
whose keys live in different hash slots (use a `{hashtag}`). Pub/Sub (`SUBSCRIBE`, `PSUBSCRIBE`, `SSUBSCRIBE`) and `MONITOR` are
refused by the console, because they need a stream that never ends.

## Connecting

| Field | Meaning |
|---|---|
| Topology | Standalone, Redis Cluster or Sentinel. The form shows the fields of the chosen topology only. |
| Host, port | The server (default 6379); on a cluster one seed node, behind Sentinel one sentinel (port 26379 by default). Through an SSH tunnel (standalone only) the engine connects to the tunnel's local end. |
| More nodes | Cluster and Sentinel: more seed nodes or sentinels (`host:port`, separated by commas), tried when the first is down. |
| Master name | Sentinel: the name the sentinels monitor (`sentinel monitor <name> ...`). Required. |
| Sentinel user, sentinel password | Sentinel: the sentinels' own login, when they have one. An empty sentinel password sends none, and the data password when a sentinel asks for one. |
| User, password | Empty user means the `default` ACL user. The password is kept in the secret store, never in the profile. |
| Database index | Standalone and Sentinel: the logical database of the connection (`SELECT`). The tree lists every database whatever this says. Hidden on a cluster, which has database 0 only. |
| Read from replicas | Cluster: send reads to replicas (they may lag behind their master). |
| Address map | Cluster and Sentinel: `announced=dialled` pairs such as `10.0.1.5:6379=127.0.0.1:17001`, for nodes behind NAT or forwarded ports. |
| Use RESP3 | Typed maps, sets and doubles in replies. Off by default. |
| Command timeout | Per command, 30 s by default. |
| Client name | Sent as `CLIENT SETNAME`, shown in `CLIENT LIST`. |

TLS modes: `require` encrypts without checking the certificate, `verifyCa` checks the chain but not the host name,
`verifyFull` checks both. The endpoint of a tunnel is `127.0.0.1`, so use `verifyCa` through a tunnel. The CA file and a client
certificate and key (mutual TLS) are read from the files named in the profile. `prefer` is not offered: Redis cannot negotiate TLS.
A cluster or Sentinel connection uses the same TLS settings and login on every node and sentinel. Nodes usually announce IP
addresses, so `verifyFull` needs IP addresses in the certificates' SANs; `verifyCa` is the practical choice there.

Connection strings: `redis://`, `rediss://` (also `valkey://`, `valkeys://`); `redis+cluster://[user[:password]@]h1:7001,h2:7002`
(also `rediss+cluster`, `valkey+cluster`, `valkeys+cluster`); `redis+sentinel://[user[:password]@]s1:26379,s2:26379/<master>[/<db>]`
(also `rediss+sentinel`) and Lettuce's `redis-sentinel://s1:26379,s2:26379/<db>#<master>`. The user and password of a URL are the
data login; the sentinel password is never read from a URL. A connection to one node of a cluster made as Standalone works, but sees
that node's keys only: the server view says so.

## The tree and the key browser

The tree shows `db0` to `db15` (the number comes from `CONFIG GET databases`, or 16 when the ACL forbids `CONFIG`) with the key
count from `INFO keyspace`; a cluster shows `db0` only, with the keys of every shard summed. Expanding a database lists its keys 200 at a time with their type and TTL; the filter box is a `MATCH`
pattern. Double-click a database for the **key browser**:

- `SCAN` only, never `KEYS`; on a cluster every master in turn, behind one cursor (a resharding or failover during a scan may
  repeat or skip keys). The pattern is a glob (`user:*`); plain text without `* ? [` matches keys that contain it.
- A page collects keys until it has 200 or the time budget (1.5 s) runs out, then hands back the cursor: **Load more** continues.
  A sparse pattern on a big database therefore returns part-filled pages instead of blocking the server.
- Filter by type (`SCAN ... TYPE`), show the **memory** of each key (`MEMORY USAGE`, sampled, one pipelined round trip per page),
  create a key of any editable type.

Keys that are not valid UTF-8 are shown as `0x...` and addressed by their bytes; they can be browsed and deleted, but their
values are edited from the console.

## Value editors

Open a key from the tree or the key list. The header shows the type, size, memory, encoding and TTL.

| Type | Editor |
|---|---|
| string | Text area; a string that parses as a JSON object or array opens in a **pretty JSON** view and is saved compact. Binary strings are shown as hex, strings over 1 MiB are shown partly and not edited. |
| hash | Fields with inline value editing, add, delete, a `HSCAN MATCH` filter and paging. |
| list | A window of 100 elements; edit an element in place (`LSET`), push to the head or tail, remove the first element equal to a value (`LREM 1`). |
| set | Members with add, remove, a `SSCAN MATCH` filter and paging. |
| sorted set | Members by rank, lowest or highest first, edit scores in place, add and remove. |
| stream | Entries newest first with paging, consumer groups, and **append** (`XADD`, with an optional id and `MAXLEN ~`). |
| JSON (RedisJSON) | Read-only pretty view; change it with `JSON.SET` in the console. |
| other types | Not editable here; use the console. |

**TTL** is set (`10m`, `2h`, `90`, `1d`, `500ms`) or removed (`PERSIST`) in the header. Saving a string keeps its TTL.

### Concurrent changes

Every edit carries what you saw of the part of the key it changes: the whole string, the field's old value, the element at an
index, a member's presence or score. The backend reads that part again on a connection of its own under `WATCH`, compares, and
only then runs `MULTI` ... `EXEC`. A different value, or a key that changed between the comparison and `EXEC`, fails with
`CONFLICT` and nothing is written. The editor shows what the key holds now and offers **Reload** or **Overwrite** (the same edit
without the guard). Creating a key is guarded by "must not exist". Deleting whole keys is not guarded: it is a destructive
operation with a typed confirmation instead.

## Console

A query tab is a console: one command per line as in `redis-cli` (quotes and `\xHH` escapes work, `#` starts a comment), with the
Redis completion language (command names, option keywords, arity hints). Each line is one result set: array and set replies are
rows of `#` and `value`, maps are rows of `field` and `value`, anything else is one cell; nested replies are shown as `redis-cli`
text. A server error stops the run with the error name (`WRONGTYPE`, `ERR`, `NOPERM`) as the vendor code and the failing line as
the position. **Stop** unblocks a blocking command (`CLIENT UNBLOCK`) and otherwise closes the tab's connection, which the next
run replaces. `SELECT`, `MULTI` and `CLIENT SETNAME` stay inside the tab.

The database picker of the console (and "New console" or "Flush database…" on a database in the tree) names the database of each
run: the tab sends `SELECT n` on its own connection first, so a command always lands in the database the picker shows, whatever
database the profile connects to. A `SELECT` typed in the console moves the tab as well, and "(no database)" stays where the tab is.

### Safety rules

Commands are classified for the statement gateway before they run:

| Class | Examples | Default rules |
|---|---|---|
| Read | `GET`, `HGETALL`, `SCAN`, `KEYS user:*`, `TTL`, `CONFIG GET`, `INFO` | allowed |
| Write | `SET`, `DEL`, `HSET`, `LPUSH`, `SADD`, `ZADD`, `XADD`, `EXPIRE`, `RENAME` | confirmation |
| Destructive | `FLUSHALL`, `FLUSHDB`, `SWAPDB`, `EVAL` and other scripts, `FUNCTION FLUSH` | typed confirmation |
| Administrative | `CONFIG SET`, `KEYS *`, `DEBUG`, `SHUTDOWN`, `ACL SETUSER`, `CLIENT KILL`, `REPLICAOF` | typed confirmation |
| Refused | `SUBSCRIBE`, `PSUBSCRIBE`, `SSUBSCRIBE`, `MONITOR` | never run |

`KEYS` with a pattern that matches everything is administrative because it blocks the server while it walks every key; a narrower
pattern is a read. `EXEC` takes the highest class of the commands queued since `MULTI`. Edits made in the editors go through the
same gateway as `redis.edit` (writes confirm, deleting keys needs the typed name).

## Server view

On the connection's context menu, **Server info and slow log**: version, mode, role, uptime, clients, memory, command count and hit
rate; the keyspace per database; every `INFO` section with a search box; and the slow log (`SLOWLOG GET`, 32 to 512 entries) with
the threshold and length the server reports.

- **Cluster**: the cluster state, slot coverage and known nodes, then one row per shard: the master (address and id), its replicas
  with their health, the slot ranges, the keys and the memory. Click a node for its own `INFO`. The slow log merges every node by
  time, with a Node column.
- **Sentinel**: the master name, the current master and its flags, the quorum, the replicas with their link status, and the sentinels.
- A standalone connection to one node of a cluster shows a warning: switch Topology to Redis Cluster to see every shard.

## Vectors (Redis 8 Query Engine)

The vector tools appear when the server has the Query Engine (the `search` module, built into Redis 8; Redis 7.4 without the
module has none). Search indexes live in database 0 only, so its tree lists the indexes with vector fields first; each opens
*Vector search* and *Vector health*.

- **Indexes as tables**: `FT._LIST` and `FT.INFO`. The columns are the `VECTOR` fields (FLAT or HNSW, `FLOAT32`, `FLOAT64` or
  `BFLOAT16`) over HASH documents (little-endian blobs) or JSON documents (arrays). A row is a document, keyed by its Redis key
  (`__key`); a bare id such as `7` stands for the index prefix plus `7`.
- **Search**: a KNN query, `FT.SEARCH idx "*=>[KNN k @field $vec EF_RUNTIME ef AS vec_distance]" PARAMS 2 vec <blob> SORTBY
  vec_distance DIALECT 2`, with `EF_RUNTIME` at least k on HNSW fields and `FT.EXPLAINCLI` as the plan. A field's index serves one
  metric, its `DISTANCE_METRIC`; *Auto* uses it. Other metrics scan every document. Distances use Redis's scale: cosine is
  `1 - cos`, L2 is squared, inner product is `1 - dot`. The copyable command carries the blob as `"\x…"`. Field names are
  escaped in the query (`@\$\.embedding`, `@my\-vec`). Searches take no filter, so the view shows no filter box.
- **Exact top-k** (recall) uses a FLAT field over the same data with the same metric when there is one. Otherwise it scans
  every document and computes distances in the app. An index with more than 50,000 documents refuses such a scan with an
  error instead of returning a partial top-k.
- **Health**: the row count is the index's `num_docs`. The index does not count documents without the field.

## Operations the views call

`redis.scan`, `redis.keyInfo`, `redis.readValue`, `redis.edit` (reads take `db`), `redis.info` and `redis.slowlog`, through
`invoke_capability`. The shared `vector.*` operations are reads (`FT.INFO`, `FT.SEARCH`, `FT.EXPLAINCLI`, `HGET`, `JSON.GET`). `redis.edit` takes a typed edit plus an optional `expect`; see `ops.rs` and `guard.rs`.

## Tests

Pure tests run with `cargo test engines::redis`. The contract tests run against `dev/compose/redis.yml`:

```sh
docker compose -f dev/compose/redis.yml up -d --wait
cargo test contract_redis -- --ignored --test-threads=1
```

They work in databases 14 and 15 on keys that start with `contract:` and never touch the dev seed in databases 0 and 1. The shared
tabular suites (tree to a table, describe columns, browse, edit rows) do not apply to a key-value engine, so `contract_tests.rs`
runs the classification, connect, query and cancel suites and adds its own: `SCAN` paging with type, TTL and memory, every value
type, TTL set and remove, stream append, every guard, a change made between the comparison and `EXEC`, database routing and tree
paging, `INFO` and `SLOWLOG`. CI runs them on Redis 7.4 and 8.2 (`ci/engines/redis.json`).

`contract_redis_vector` runs the shared vector suite on `fosta_contract_vectors`, an HNSW cosine index over 300 HASH documents
`fosta_contract:vec:<id>` that it recreates in database 0 (index and those keys only). Without the search module (Redis 7.4) it
prints a note and passes. The dev seed adds `products_idx` over `products:<id>` hashes with a 4-dimensional embedding.
