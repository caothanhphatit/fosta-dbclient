# Elasticsearch and OpenSearch

Engine ids `elasticsearch` and `opensearch`. One adapter serves both (`src-tauri/src/engines/elasticsearch/`), each
with its own descriptor; the UI module is `src/engines/elasticsearch/` and OpenSearch re-exports it. Tested against
Elasticsearch 8.19 and OpenSearch 2.19 (`dev/compose/elasticsearch.yml`, `dev/compose/opensearch.yml`).

## Connecting

| Setting | Notes |
|---|---|
| Host and port | `9200` by default. `https` when TLS is required (`require`, `verifyCa`, `verifyFull`). |
| Node URLs | Advanced: several nodes, comma separated (`https://es1:9200, https://es2:9200`); later ones are failover. A path prefix (a proxy) is kept. |
| Cloud ID | Elasticsearch only; replaces host and port. |
| Auth | Basic (user and password; also the OpenSearch security plugin), API key (Elasticsearch: the encoded value), bearer token, or none. |
| TLS | CA file, client certificate, accept invalid certificates, or the SHA-256 fingerprint of the node's own HTTP certificate (`pinnedSha256`, leaf only). The fingerprint Elasticsearch prints on first start belongs to its HTTP CA: use `config/certs/http_ca.crt` as the CA file instead. Built with the shared rustls helpers (`net::tls`). |
| SSH | The tunnel forwards one node: requests go to its local end, so a node list is not used through a tunnel. Set the TLS server name when the certificate is for the real host. |

Connection strings: `https://elastic:secret@es1:9243, https://es2:9243` (the password goes to the secret store, never into the
saved URL) or a Cloud ID.

The server must match the profile: an Elasticsearch profile on an OpenSearch cluster (or the reverse) is refused with a message
naming the right engine. Minimum versions: Elasticsearch 7.10, OpenSearch 1.0 (point-in-time paging needs OpenSearch 2.4).

## Tree

Three folders: **Indices**, **Data streams** (when the cluster has them) and **Aliases**. Every index, data stream and alias shows
its document count and size, and health or state as a badge. They are `collection` nodes, so the document grid opens on a
double-click and **Mapping** (the design view) shows the fields and the mapping as JSON. Lists page by 500 and honour the tree's name
filter.

## Document grid

- **Columns** are the top-level fields of the mapping, typed by the field type (`long` → integer, `scaled_float` → decimal, `date`
  → timestamp, `object` → document, `nested` → array of documents, `geo_point` and others → JSON). `_id` comes first; an alias, data
  stream or pattern adds `_index`.
- **Filters**: structured conditions become Query DSL clauses under `bool.filter` (`term`, `range`, case-insensitive `wildcard`
  and `prefix` for contains / starts with / ends with, `terms`, `exists`); text fields use their `keyword` sub-field. The raw
  filter is one Query DSL query.
- **Paging**: `from` / `size` while the set fits in the first 10,000 hits; beyond that `search_after` on the sort, with a point in
  time and `_shard_doc` as the tiebreaker when the index has no declared key. The point in time is kept alive for 5 minutes after
  each page.

### Keys

`_id` cannot be sorted on Elasticsearch 8 (no field data) and cannot be ranged on either server, so an index can declare the field
that holds its document id:

```json
PUT /orders
{ "mappings": { "_meta": { "fosta": { "key": "order_id" } }, "properties": { "order_id": { "type": "long" } } } }
```

With a declared key the document `_id` is that field's value as text, the field is the key column (sortable, rangeable, the
`search_after` tiebreaker) and `_id` is read-only. Inserts need the key; edits cannot change it. Without a declaration the key is
`_id`, which takes only `=`, `≠`, `in` and `not in` filters. Through an alias, data stream or pattern the key also has `_index`, and
edits go to the document's own (backing) index.

### Edits

Elasticsearch has no transactions, so an apply runs in two phases:

1. A realtime `_mget` reads every document an update or delete targets and every id an insert would create. A missing document, a
   changed version (`_seq_no` / `_primary_term`, read with the page) or changed original values stop the apply with `CONFLICT`
   before anything is written; an insert over an existing id is an error.
2. One `_bulk` with `refresh=wait_for`: `create` for inserts, `if_seq_no` / `if_primary_term` on every update and delete
   (`detect_noop: false`, so the version always moves).

A document changed between the two phases fails its item with 409; the items that succeeded are undone (best effort, guarded by
their new versions) and the apply reports `CONFLICT` with `compensated: true`, or `false` and a note when undoing failed. Indices
that require custom routing are read-only in the grid.

## Query tabs

- **Console** (`es-console`): Kibana Dev Tools requests, `METHOD /path` and a JSON or NDJSON body; `#` and `//` comment lines and
  `"""` strings are accepted. Each request shows a hits table (`_index`, `_id`, `_score` and the `_source` fields) when the
  response has hits, then the response as JSON with its HTTP status. Completion offers methods, endpoints, index names, Query DSL
  keys by context and the fields of the request's index; Run current statement runs the request under the cursor; Format indents
  JSON bodies.
- **ES|QL** (Elasticsearch 8.11+) and **SQL** (`_sql`, or `_plugins/_sql` on OpenSearch): one query, streamed as a typed table up
  to the row limit; SQL cursors are followed, then closed.

Stop drops the running request (the cluster cancels a search when its connection closes) and cancels the tasks it left on the
cluster, found by their `X-Opaque-Id`.

## Safety (gateway)

Console requests are classified by method and path. The API is read from its position (`/_cluster/settings`,
`/products/_search`), so a document id such as `_search` does not make a write look like a read; segments are percent-decoded like
the server does.

| Category | Requests |
|---|---|
| Read | GET / HEAD (except `_flush`, `_refresh`), `_search`, `_count`, `_msearch`, `_mget`, `_field_caps`, `_validate`, `_explain`, `_analyze`, PIT, `_query`, SQL queries |
| Write | `_doc`, `_create`, `_update`, `_bulk`, `_reindex` |
| DDL | `PUT /<index>`, `_mapping`, `_settings`, aliases, templates, pipelines, ILM / ISM policies, rollover, open |
| Destructive | `DELETE /<index>`, `_delete_by_query`, `_update_by_query`, `_close`, `_aliases` with `remove_index`, deleting templates, pipelines, policies, data streams, snapshots |
| Admin | `_cluster/settings`, reroute, node shutdown, task cancel, snapshot restore, refresh / flush / force merge, `_security`, anything unrecognised |

A path the HTTP client would rewrite before sending (`.` or `..` segments, also percent-encoded, `#`, `\`, characters it
re-encodes) is refused outright, so what is classified is what is sent.

Raw grid filters must be one Query DSL query without code: `script`, `script_score`, `script_fields`, `scripted_metric`,
`_script`, any key ending in `_script` (`minimum_should_match_script`), `runtime_mappings` and `wrapper` (a base64 query decoded
only by the server) are refused. The check runs in the engine's `validate_filter` (the gateway) and again inside `browse`, and the
filter is only ever placed under `bool.filter`.

## Errors

An error body becomes `ENGINE_ERROR` with the error `type` as the vendor code (the root cause's type for `all shards failed`),
the `reason` as the message, the `caused_by` chain, the root causes and the first shard failure as `cause`, and the HTTP `status`.
`parsing_exception` positions point into the request body in the editor; ES|QL and SQL `line N:M` positions point into the query.
Timeouts are `TIMEOUT`, unreachable nodes and TLS failures `CONNECTION_ERROR`.

## OpenSearch differences

No ES|QL, no API keys, no Cloud ID; SQL goes to `_plugins/_sql` (JDBC format), point in time to `_search/point_in_time`,
lifecycle policies are ISM. With the security plugin enabled the cluster needs basic auth and https (the demo configuration uses
`admin`); a 401 says so.

## Engine operations

`invoke` names for engine views: `es.clusterHealth`, `es.indexDetails`, `es.cat`, `es.document`, `es.names` (index, alias and data
stream names), `es.fields` (fields of an index) are reads; `es.documentOp` is a write and `es.byQuery` is destructive.

## Tests

```sh
docker start fosta-dev-nosql-elasticsearch-1
docker compose -p fosta-dev-nosql -f dev/compose/opensearch.yml up -d --wait   # from the main checkout
sh dev/seed-nosql.sh                                                             # optional: logs-app for the PIT test
cd src-tauri && cargo test contract_elasticsearch -- --ignored --test-threads=1
cargo test contract_opensearch -- --ignored --test-threads=1
```

Override the servers with `FOSTA_CONTRACT_ELASTICSEARCH_HOST` / `_PORT` / `_USER` / `_PASSWORD` and
`FOSTA_CONTRACT_OPENSEARCH_HOST` / `_PORT`.
