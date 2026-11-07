# Fosta Studio: architecture research for the engine refactor

Research date: 2026-11-08. Context read: `docs/adr/0001-hexagonal-core-and-engine-modules.md`, `ARCHITECTURE.md`, `ROADMAP.md`,
`src-tauri/Cargo.toml`. Sources are cited inline. Where a claim comes only from a third-party summary or is unverified, it says so.

## Executive summary (recommended architecture)

- **A Cargo workspace instead of one crate.** Split today's single `src-tauri` crate into these parts:
  - `fosta-core`: domain, gateway and capability traits, with no Tauri.
  - `fosta-proto`: wire types.
  - One crate per engine adapter.
  - Thin inbound adapters: `fosta-tauri` and `fosta-web`.

  Engines are Cargo features, so a build can leave engines out.
- **Two plugin tiers, one contract.** First-party engines are compiled-in Rust trait objects, which are the fastest and fully
  typed. Third-party engines and engines that need a heavy native SDK (Oracle Instant Client, vendor SDKs) run **out of process**.
  They speak the *same* capability contract as JSON-RPC 2.0 over stdio, and send bulk results as Arrow IPC frames. Skip WASM and
  `abi_stable` dylibs for drivers for now.
- **Capabilities instead of one `Driver`.** Keep ADR 0001: a small core port plus optional capability traits, announced through a
  manifest-shaped `capabilities()`. Tabularis' manifest is a good checklist.
- **Arrow as the result format.** Engines produce Arrow `RecordBatch`es (typed and columnar). Batches stream to the UI as Arrow IPC
  bytes through a Tauri `Channel` or `ipc::Response`, which arrive as an ArrayBuffer with no JSON. The web edition sends the same
  bytes as binary WebSocket frames. This replaces `Option<String>` cells and is the base for profiling, export and DuckDB.
- **Embedded DuckDB as an optional feature.** It gives local analytics over results and over CSV, Parquet and JSON files. It also
  gives cross-database joins through DuckDB's MySQL, Postgres and SQLite attach. This is a real differentiator against Navicat.
  It costs about 45-55 MB per platform, so ship it as a downloadable component rather than in the base installer.
- **One language service in the Rust core.** Make it transport-agnostic, like Supabase's postgres-language-server. It contains:
  - Parsing: sqlparser-rs for validity and classification, and tree-sitter for tolerant parsing while the user types.
  - A cached schema model with introspection levels, as in DataGrip.
  - Per-language providers: SQL dialects, Mongo shell, Redis commands, and ES Query DSL / ES|QL.

  The statement gateway reuses the same service.
- **UI: keep plain TypeScript.** Add these pieces:
  - A view registry keyed by `(engine, nodeKind, capability)`.
  - Lazy `import()` per engine.
  - `@preact/signals-core` for state.
  - A Web Worker for Arrow decoding and formatting.
  - A virtualised grid that reads Arrow columns directly.

  Allow a framework "island" only for one complex view, and only if it pays for itself.
- **Contract tests per capability**, run against every adapter. Use testcontainers-rs locally and GitHub Actions service
  containers in CI.
- **Target Navicat's weak points directly:**
  - Open source and free.
  - Large results that stay fast.
  - Real NoSQL depth: document, key-value and search views.
  - An open driver protocol.
  - An assistant that never runs statements outside the gateway.

---

## 1. How other clients structure engine support

**Findings**

- **DBeaver**
  - Built on an OSGi platform with 130+ plugins on Eclipse RCP. Model plugins are separate from desktop UI plugins, so the same
    back end serves both DBeaver and CloudBeaver ([DBeaver README](https://github.com/dbeaver/dbeaver)).
  - Extending it means `plugin.xml` extension points. One user reported that overriding the generic data-source extension had no
    effect ([discussion #9044](https://github.com/dbeaver/dbeaver/discussions/9044)).
  - Slow startup and high memory are blamed on Java and Eclipse
    ([Devart](https://www.devart.com/dbforge/dbeaver-alternatives.html),
    [Beekeeper blog](https://www.beekeeperstudio.io/blog/dbeaver-alternatives-free)). Both sources are competitors.
  - **What worked:** the split between model and UI, and broad JDBC coverage.
  - **What became heavy:** the platform's weight, the XML extension registry, and the JVM.
- **DataGrip**
  - Keeps one model per schema in memory, saves it to disk, and refreshes it incrementally.
  - Uses introspection levels: level 1 loads names and signatures, level 2 loads everything except source, level 3 loads
    everything.
  - Picks the level automatically from schema size, for example level 3 for up to 1000 objects in the current schema
    ([introspection levels](https://www.jetbrains.com/help/datagrip/introspection-levels.html),
    [JetBrains blog](https://blog.jetbrains.com/?p=414260/)).
  - **What worked:** completion from a cached model, and fetching object source only when needed.
- **Beekeeper Studio**
  - Uses one TypeScript client class per engine (`mysql.ts`, `postgresql.ts`, `sqlite.ts`, `redis.ts`, `bigquery.ts`, ...).
    They share the base classes `BasicDatabaseClient.ts` and `BaseV1DatabaseClient.ts`
    ([repo dir](https://github.com/beekeeper-studio/beekeeper-studio/tree/master/apps/studio/src/lib/db/clients)).
  - Since version 5.3 (July 2025), UI plugins run as sandboxed iframes on a `plugin://` protocol. They are declared in a manifest
    and talk to the host through `postMessage`. The AI Shell ships as such a plugin
    ([plugin dev docs](https://docs.beekeeperstudio.io/plugin_development/),
    [5.3 release](https://www.beekeeperstudio.io/es/blog/release-5.3)).
  - **What worked:** simple per-engine classes, and isolated UI plugins.
  - **Limits:** engines are still compiled in, and the class inheritance tends to force a SQL shape onto non-SQL engines. That is
    Fosta's current problem.
- **Tabularis (Tauri)**
  - Driver plugins are **separate executables**. They speak **JSON-RPC 2.0, one message per line over stdin/stdout**.
  - The author rejected dylibs because Rust has no stable ABI and a plugin crash would kill the host. WASM was rejected because
    drivers are I/O-heavy and need native SDKs.
  - Implementation: one Tokio task owns the child's pipes and a map of pending requests. Calls time out after 120 s, child
    processes use `kill_on_drop`, plugins may not claim built-in ids, and there is no signing yet
    ([design post](https://tabularis.dev/blog/database-drivers-as-external-processes)).
  - The manifest declares `id`, `executable`, `default_port`, `capabilities`, `data_types`, `settings` and `connection_fields`.
    Capability flags include schemas, views, routines, file_based, explain, column_stats, sql_dialect, identifier_quote and
    readonly.
  - There are "a bit more than thirty" methods, such as `initialize`, `test_connection`, `get_databases`, `get_tables`,
    `execute_query` and `explain_query` ([wiki](https://tabularis.dev/wiki/plugins)).
  - Redis ships as a plugin ([blog](https://tabularis.dev/blog/nosql-plugins-community)).
  - **What worked:** drivers in any language, crash isolation, and installing without a restart.
  - **Weak points:** the method set is shaped around SQL tables, results are JSON only, and there is no sandbox.
- **TablePlus**
  - Native: Swift, Objective-C, C and C++ on macOS, and C# on Windows ([FAQ](https://tableplus.com/faq/)).
  - Has JavaScript plugins in beta, for UI actions rather than drivers ([site](https://tableplus.com/)).
  - **What worked:** speed.
  - **Limits:** closed source, and two separate codebases.
- **Grafana**
  - Backend plugins are subprocesses launched through HashiCorp go-plugin and called over **gRPC**. A panic in a plugin does not
    take down the server.
  - Capabilities: query data (returns data frames), resources (custom HTTP), health check, streaming and metrics. Instances are
    cached per configuration, which allows connection pooling
    ([backend plugins](https://grafana.com/developers/plugin-tools/key-concepts/backend-plugins/)).
  - **What worked:** this is the closest analogue to "backend driver plus frontend view" with a typed contract.
- **Zed**
  - Extensions are Rust compiled to `wasm32-wasip2` components and run in Wasmtime, with versioned WIT interfaces
    (`wit/since_v*`).
  - Tree-sitter grammars are loaded as WASM so that crashes in their C code cannot take down the editor
    ([Zed blog](https://zed.dev/blog/language-extensions-part-1)).
  - Extensions provide languages and language-server launchers, not network-heavy drivers.
- **VS Code**
  - Extensions run in a separate Extension Host: local Node, a web worker, or remote.
  - They load lazily through activation events ([docs](https://code.visualstudio.com/api/advanced-topics/extension-host)).

**Recommendation**: borrow one thing from each of four projects, and avoid DBeaver's platform layer.

- From Grafana: the shape, a typed backend contract plus a registered frontend view per capability.
- From Tabularis: the transport for third parties, stdio JSON-RPC plus a manifest.
- From DataGrip: introspection levels.
- From VS Code: lazy activation.

Trade-off: you maintain your own small plugin protocol instead of inheriting a platform.

## 2. Plugin isolation options for Rust drivers

| Option | Speed | Crash isolation | Languages | Status / cost |
|---|---|---|---|---|
| Compiled-in trait objects, cargo features | Best; Arrow batches need no copy | **None**: `Cargo.toml` sets `panic = "abort"`, so a driver panic kills the app | Rust | Simplest; adding an engine means recompiling |
| Out of process, JSON-RPC over stdio | Serialisation and a pipe per call. Fine for metadata; use Arrow IPC for rows | Full | Any | Proven by Tabularis (stdio JSON-RPC) and Grafana (gRPC) |
| WASM component (Wasmtime + WIT) | Good | Strong sandbox | Rust and other languages compiled to wasip2 | Workable for pure-Rust drivers in principle; not verified end to end. Native SDKs such as Oracle OCI cannot run in it. Details below. |
| Dynamic libraries (`abi_stable`) | Best | None | Rust only | No unloading, and every type must implement `StableAbi` ([docs](https://docs.rs/abi_stable)) |

WASM status in detail:

- `wasi:sockets` TCP exists. Wasmtime denies all addresses by default; `inherit_network()` plus `allow_ip_name_lookup` open them
  ([wasmtime-wasi source](https://docs.rs/crate/wasmtime-wasi/36.0.7/source/src/sockets/mod.rs),
  [Zulip #9849](https://bytecodealliance.github.io/zulip-archive/stream/225357-git-wasmtime/topic/wasmtime.20.2F.20issue.20.239849.20Network.20access.20example.html)).
- WASI 0.3 adds native async ([wasi.dev](https://wasi.dev/releases)).
- `wasi-tls` and `wasi-sql` are still proposals.
- Pure-Rust drivers with rustls may work, but I did not verify this end to end.

**Recommendation (layered)**

1. **Tier 1, compiled in.** MariaDB/MySQL, SQL Server, MongoDB, PostgreSQL, SQLite, ClickHouse, Redis, and
   Elasticsearch/OpenSearch.
   - Run each engine call in its own `tokio::spawn` task.
   - Switch release builds to `panic = "unwind"`, so a driver panic becomes an error instead of a crash. The cost is a slightly
     larger binary.
2. **Tier 2, out of process.** For Oracle, community drivers, and anything written in Go, Python or Java.
   - Oracle needs the Oracle client libraries through ODPI-C (see [rust-oracle](https://github.com/kubo/rust-oracle)).
   - A JDBC bridge could later give DBeaver-level breadth.
   - The host adapter `RemoteEngine` implements the same capability traits, so the core cannot tell the difference.
3. **Later, WASM.** For small sandboxed extensions such as formatters, value renderers and completion providers, as Zed does.
   Wait until WASI 0.3 and wasi-tls settle.

**Protocol sketch** (`fosta-driver-protocol`, versioned):

```jsonc
// host -> driver, one JSON object per line on stdin
{"jsonrpc":"2.0","id":1,"method":"handshake","params":{"protocol":"1.0","host":"fosta 0.x"}}
// <- {"id":1,"result":{"id":"oracle","protocol":"1.0","capabilities":{
//      "core":true,"tabular.browse":true,"tabular.edit":true,"schema.ddl":true,"explain":true,
//      "streaming":true,"cancel":true,"transactions":true},"tree":{"nodeKinds":[...]},"dialect":"oracle"}}
{"id":2,"method":"connect","params":{"profile":{...},"secret":{"handle":"s-41"}}}      // secret sent once, never logged
{"id":3,"method":"session.open","params":{"conn":"c1"}}
{"id":4,"method":"session.run","params":{"session":"s1","text":"select ...","stream":"r7","batchRows":2000}}
// <- {"id":4,"result":{"stream":"r7","schema":"<base64 Arrow schema>"}}
// <- notifications: {"method":"stream.batch","params":{"stream":"r7","seq":0,"bytes":"<len>"}} + raw frame
{"method":"$/cancel","params":{"id":4}}                                               // LSP-style cancel
{"id":5,"method":"tree.children","params":{"ref":{"database":"HR","schema":"HR","kind":"schema"}}}
```

Notes on the protocol:

- **Framing:** use LSP-style `Content-Length` headers instead of newline-delimited JSON. An Arrow IPC frame can then follow a JSON
  header on the same pipe without base64. Tabularis' line protocol cannot carry binary data.
- **Method names:** namespace them by capability (`tabular.*`, `document.*`, `kv.*`, `search.*`, `schema.*`), mirroring the Rust
  traits.
- **Keeping both sides in sync:** generate the protocol types from the Rust types (serde plus `schemars`).

## 3. Data interchange for speed

**Findings**

- **Today:** cells travel as `Option<String>` in JSON arrays, with at most 1000 rows per result set (`ARCHITECTURE.md`). Types are
  lost: numbers, dates, binary as `0x…`, and flattened Mongo documents. JSON cost grows with result size.
- **Tauri 2:**
  - Return values are JSON by default, which "can slow down your application" for large data. Returning `tauri::ipc::Response`
    gives the UI an ArrayBuffer instead.
  - The docs recommend **channels** for "ordered, high-throughput data delivery". Events carry JSON only
    ([Calling Rust](https://v2.tauri.app/develop/calling-rust/)).
  - `Channel<Vec<u8>>` serialises as a JSON array of numbers; use `InvokeResponseBody::Raw` instead
    ([tauri channel source](https://docs.rs/crate/tauri/3.0.0-alpha.3/source/src/ipc/channel.rs)).
- **Arrow vs JSON evidence**, mostly measured outside the browser:
  - About 10x faster than JSON from around 1M rows, in a Python test
    ([blog, 2025](https://guillesd.github.io/intro/2025/07/27/arrow-data-transfer.html)).
  - 1.5-2x faster at 1M rows, in a Next.js/Snowflake transport test
    ([kayhan.dev](https://hn.jakechampion.name/reader/https://kayhan.dev/posts/012-arrow-flight-vs-json-nextjs-snowflake-benchmark/)).
  - In JS, **Flechette** decodes Arrow IPC 1.3-11x faster than the reference `apache-arrow` package, and is smaller
    ([Flechette](https://idl.uw.edu/flechette/)).
  - I found no published browser benchmark of Arrow IPC against `JSON.parse`. This needs measuring.
- **ADBC:**
  - Stable drivers: PostgreSQL (C++), SQLite (C), Snowflake (Go, with a Rust wrapper), Flight SQL (Go) and DuckDB (third party).
  - BigQuery is experimental. DataFusion is the only driver written in Rust.
  - C drivers can be used from Rust ([status, v19](https://arrow.apache.org/adbc/19/driver/status.html)).
  - The Rust `adbc_core` crate has a driver manager that loads drivers from a path, a TOML manifest or a URI
    ([driver manifests](https://arrow.apache.org/adbc/main/format/driver_manifests.html)).
  - ClickHouse ships an ADBC driver through Columnar's Driver Foundry and `dbc install`
    ([ClickHouse blog](https://clickhouse.com/blog/introducing-the-clickhouse-adbc-driver)).

**Recommendation**

- **Result type:** make arrow-rs `RecordBatch` the result type of `session.run` and `browse`. Adapters convert native rows with
  Arrow builders. Engine-specific types become Arrow extension types (`fosta.bson`, `fosta.vector`, `fosta.geometry`).
- **Streaming:** send a small first batch (for example 500 rows) so the first rows appear quickly, then larger batches.
- **Cancellation:** each run gets a `CancellationToken`. Cancelling drops the stream and calls the engine's own cancel:
  `KILL QUERY`, a TDS attention message, `killOp`, or an ES task cancel.
- **Transport:** a Tauri `Channel` carrying raw Arrow IPC bytes. The web edition sends the same bytes as binary WebSocket frames.
- **Decoding:** Flechette in a Web Worker. The grid reads columns lazily, so rows are never built as JS objects.
- **Editing:** the grid's text rendering and editing work from Arrow values. Edits still go back as typed `RowChange`s.
- **ADBC is optional.** It is useful for Snowflake, BigQuery and Flight SQL breadth, as Tier 2 loaded by manifest. For the core
  engines, native Rust crates give control over cancel, sessions and metadata that ADBC does not standardise.
- **Trade-off:** arrow-rs adds compile time, some binary size, and one more format to debug. In return you get typed values and
  one format for the grid, export, profiling, DuckDB and plugins.

## 4. Embedded DuckDB

**Findings**

- **Cross-database work:** DuckDB attaches MySQL, Postgres and SQLite (core extensions, formerly `*_scanner`) and joins across them
  in one query. `COPY FROM DATABASE` copies data between them. A single transaction can write to only one attached database
  ([DuckDB blog](https://duckdb.org/2024/01/26/multi-database-support-in-duckdb),
  [extensions](https://duckdb.org/docs/current/core_extensions/overview)).
- **Building it:** the duckdb-rs `bundled` feature compiles DuckDB from its C++ source. It leaves out ICU because of the crates.io
  10 MB limit, and build time grew sharply when bundling began
  ([duckdb-rs README](https://docs.rs/crate/duckdb/1.4.4/source/README.md)).
- **Size:** there is no official figure. Debian's CLI package is about 50 MB installed, and third-party macOS dylibs are about
  50-55 MB ([Debian](https://packages.debian.org/en/sid/duckdb),
  [frozen-duckdb](https://docs.rs/crate/frozen-duckdb/latest/source/docs/architecture/binary-management.md)).
- **License:** MIT ([repo](https://github.com/duckdb/duckdb)).

**Is it a differentiator?** Yes. Navicat has BI and charts, but I found no embedded analytical engine for querying result sets,
files and several servers together in its 17.x announcements
([Navicat 17.2](https://www.navicat.com/en/company/press-2/3273-navicat-17-2-introduces-ai-assistant,-snowflake-support,-and-enhanced-data-modeling-features.html)).

**Recommendation**: put DuckDB in a `fosta-analytics` crate behind a feature flag. Run it as a Tier-2 sidecar or an optional
download, so the base installer stays small and the slow C++ build stays out of the normal dev loop.

Uses:

- "Analyze this result": register Arrow batches as DuckDB tables, with no copy.
- Open CSV, Parquet or JSON files as a connection.
- The profiling roadmap item (histograms, distinct counts), computed locally.
- "Federated query" across several connections.

Notes:

- DuckDB's MySQL and Postgres scanners need their own credentials. Pass secrets through the core, never into saved SQL.
- Mongo, ES and Redis data joins in through exported Arrow.

## 5. Schema introspection and SQL intelligence

**Findings**

- **sqlparser-rs** 0.63 has 16 dialects, including MySQL, MsSql, PostgreSQL, SQLite, ClickHouse, **Oracle**, DuckDB, Snowflake
  and BigQuery ([docs.rs](https://docs.rs/sqlparser/latest/sqlparser/dialect/index.html)). It fails on incomplete SQL, so it suits
  classification and validation, not completion while the user types.
- **tree-sitter SQL** (DerekStride, MIT; Rust crate `tree-sitter-sequel`) is permissive across Postgres, MariaDB and SQLite syntax,
  and always produces a tree ([pkg](https://pkg.go.dev/github.com/DerekStride/tree-sitter-sql)).
- **Supabase's postgres-language-server** ([Supabase blog](https://supabase.com/blog/postgres-language-server)):
  - Runs **both** tree-sitter and libpg_query on each statement.
  - Treats one statement as the unit of compilation, and the live schema as the source of truth.
  - Re-analyses only the statements that changed.
  - Uses a transport-agnostic workspace API, with LSP and CLI as two entry points (the Biome approach).
- **DataGrip:** a cached per-schema model, introspection levels, and incremental refresh (see section 1).

**Recommendation**: yes. Build one `fosta-lang` service in Rust, called through ordinary commands rather than a separate LSP
process.

- **A `LanguageProvider` trait** with `split_statements`, `classify` (for the gateway), `diagnostics`,
  `complete(text, offset, schema_view)`, `hover` and `format`.
- **Implementations:**
  - SQL: tree-sitter for tolerant context, plus a sqlparser dialect for validity and classification.
  - MongoDB shell: tree-sitter-javascript plus a catalogue of known operators.
  - Redis: the command table from `COMMAND DOCS`.
  - ES Query DSL: JSON-schema driven, based on the index mapping.
  - ES|QL: a small hand-written parser (the ES|QL grammar is open).
- **A `SchemaCache`** per connection:
  - Introspection levels 1-3, as in DataGrip.
  - Saved to disk, and refreshed per object when needed.
  - Filled by the `schema.introspect` capability.
  - The assistant reads the same cache.
- **LSP-shaped API:** positions and completion items, so a standalone LSP for VS Code or Zed later is only a thin wrapper.
- **Trade-off:** two parsers per SQL dialect. Tree-sitter grammars are not dialect-exact, so completion may suggest invalid syntax.

## 6. Elasticsearch "full" client

**APIs that matter** ([Elasticsearch API reference](https://www.elastic.co/docs/api/doc/elasticsearch),
[ES|QL API](https://www.elastic.co/guide/en/elasticsearch/reference/current/esql-query-api.html),
[tasks](https://www.elastic.co/docs/api/doc/elasticsearch/v8/group/endpoint-tasks)):

- **Querying:** `_search` with aggregations, `_query` (ES|QL, plus async run/get/stop), and `_sql` (plus translate).
- **Index structure:** `_mapping`, index settings, templates, aliases and data streams.
- **Ingest and text analysis:** `_ingest/pipeline` and its `_simulate`, and `_analyze`.
- **Cluster:** `_cat/*`, `_cluster/health`, `_nodes/stats`, and shard allocation explain.
- **Lifecycle and backups:** ILM explain and policies, and `_snapshot` repositories and restore.
- **Tasks:** `_tasks` (list and cancel).
- **Bulk data:** `_bulk` and `_reindex`.
- **Paging:** PIT plus `search_after`.

**Client libraries**

- The official `elasticsearch-rs` is still labelled alpha / work in progress ([repo](https://github.com/elastic/elasticsearch-rs)).
- `opensearch-rs` is the Apache-2.0 fork for OpenSearch ([lib.rs](https://lib.rs/crates/opensearch)).

**Use plain HTTP** (`reqwest`, already a dependency) with a thin typed layer for the endpoints above. Both products speak the same
REST/JSON for the core APIs, and this way you control version differences and auth (API key, basic, cloud id).

**OpenSearch**: yes, support it in the same adapter, with a `flavor` detected from `GET /`. Differences to handle:

- OpenSearch has no ES|QL, but has SQL and PPL at `_plugins/_sql` and `_plugins/_ppl`
  ([AWS docs](https://docs.aws.amazon.com/opensearch-service/latest/developerguide/sql-support.html)).
- ILM in Elasticsearch, ISM in OpenSearch.
- OpenSearch's security plugin.

**Licensing** is not a problem for a client:

- Elasticsearch added AGPL as a license option in August 2024
  ([admin-magazine](https://www.admin-magazine.com/News/Open-Source-AGPL-Added-as-License-Option-for-Elasticsearch)).
- OpenSearch is Apache-2.0 under the Linux Foundation
  ([devclass](https://www.devclass.com/development/2024/09/16/aws-hands-opensearch-to-linux-foundation-is-this-why-elasticsearch-was-made-open-source-again/1621865)).
- Talking HTTP to a server does not make Fosta a derivative work. This is not legal advice.

## 7. UI architecture without a framework

- **View registry:**
  - Register views with `registerView({ engine?, nodeKind, capability, load: () => import('./engines/redis/keyView') })`. The most
    specific match wins. Editors are registered per language id.
  - This mirrors VS Code's activation events ([extension host](https://code.visualstudio.com/api/advanced-topics/extension-host)):
    nothing for Redis loads until a Redis node is opened.
  - Vite already splits bundles on dynamic `import()`.
- **State:**
  - Use `@preact/signals-core`: signal, computed, effect and batch, framework independent, about 1-2 KB
    ([npm](https://npmjs.com/package/@preact/signals-core)).
  - One store per workspace, connection and tab. Views subscribe with `effect` and dispose of it on close, which fixes leaks from
    manual listeners.
- **Virtualisation:**
  - Keep the existing virtual `DataGrid`, but make its data source an interface (`rowCount`, `getCell(col,row)`) backed by Arrow
    columns.
  - Virtualise the tree too, for large schemas and for Redis keyspaces paged with `SCAN`.
- **Workers:**
  - Run these in a Worker: Arrow decoding, CSV/JSON export formatting, pretty-printing large JSON documents, and search inside
    results.
  - Transfer `ArrayBuffer`s instead of copying them.
- **Framework island:**
  - Use one only when a view has a dense, stateful, interactive layout and the plain-TS version passes about 1500 lines. Examples:
    a visual query or aggregation builder, or the ER diagram.
  - Mount Preact or Solid inside that view's container only.
  - Trade-off: two styles of UI code, so keep islands rare.
- **UI plugins:** if third parties get UI later, use Beekeeper's model: a sandboxed iframe, `postMessage`, and views declared in a
  manifest ([docs](https://docs.beekeeperstudio.io/plugin_development/)).

## 8. Testing an engine matrix

- **Capability contract suite** in `fosta-engine-tests`:
  - Generic async test functions, for example `tabular::browse_paging(&dyn TabularBrowse)`, `tabular::edit_roundtrip`,
    `session::cancel_long_query`, `types::roundtrip(fixture)` and `tree::children_stable`.
  - Each adapter crate runs every suite for the capabilities it advertises. A macro generates the `#[tokio::test]`s.
  - The out-of-process `RemoteEngine` runs the same suite against a sample plugin, which also tests the protocol.
- **Local:** testcontainers-rs with `testcontainers-modules`. It has modules for Postgres, MySQL, MSSQL, Oracle, Redis,
  Elasticsearch, ClickHouse and MongoDB, each behind a feature
  ([docs](https://docs.rs/testcontainers-modules/latest/testcontainers_modules/)).
- **CI:**
  - A GitHub Actions matrix of engine x version, using service containers
    ([GitHub docs](https://docs.github.com/en/actions/using-containerized-services/about-service-containers)).
  - Containers run on Linux runners only. Windows runs the unit tests plus SQLite and DuckDB.
  - Nightly: older and newer server versions, and OpenSearch.
- Keep the existing `live_tests.rs` as the starting point, and move fixtures (typed values per engine) into shared files.

## 9. Navicat weaknesses to target

- **Cost**
  - The most frequent complaint. G2 lists $1,599 for a perpetual license and $799.99 per year for one user, and reviewers call it
    expensive for small teams ([G2 compare](https://www.g2.com/compare/dbeaver-vs-oracle-mysql-workbench-vs-navicat-premium),
    [ToolRadar](https://toolradar.com/tools/navicat/pricing)).
  - Third-party price listings disagree with each other.
  - A free "Premium Lite" exists, with core features only ([Wikipedia](https://en.wikipedia.org/wiki/Navicat)).
- **Large data**
  - Reports of "occasional instability during large query executions" and performance issues with large databases. These come
    from aggregator summaries and are anecdotal ([SaaSworthy](https://www.saasworthy.com/product/navicat-premium/reviews),
    [Spotsaas](https://www.spotsaas.com/product/navicat-premium)).
  - Navicat's own MongoDB release notes fix slowness in Query Builder and in Excel export
    ([release notes](https://www.navicat.com/de/products/navicat-for-mongodb-release-note.html)).
- **NoSQL depth**
  - SaaSworthy lists "limited non-relational database support".
  - Navicat 17 covers Redis and MongoDB. I found no evidence of Elasticsearch or ClickHouse support.
- **Extensibility:** I found no public plugin or driver API in the Navicat 17 material.
- **AI**
  - Navicat 17.2 added an AI Assistant pane supporting ChatGPT, Gemini, DeepSeek and Ollama
    ([Navicat blog](https://navicat.com/company/aboutus/blog/3271-navicat-17-2-smarter-database-management-with-ai-support-and-enhanced-cloud-capabilities)).
  - So AI alone is no longer a differentiator. AI that is schema-aware, guarded by the gateway, and works with local models still is.

**Targets for Fosta:**

- Open source and free.
- The first rows appear in under 100 ms, and million-row results still scroll smoothly (Arrow plus streaming).
- Elasticsearch/OpenSearch and ClickHouse support, which Navicat lacks.
- An open driver protocol.
- DuckDB federation across connections.
- The safety gateway.

---

## Proposed layout

### Rust (cargo workspace under `src-tauri/`)

```
crates/
  fosta-proto/        wire types: ObjectRef, NodeKind, Capabilities, RowChange, typed Value, Arrow ext types, protocol messages
  fosta-core/         domain + services: ConnectionManager, SessionManager, StatementGateway, rules, transactions,
                      history, audit, schema graph, assistant orchestration; ports: SecretStore, ConfigStore, EventSink
  fosta-engine/       capability traits: Engine (core port), TabularBrowse/Edit, DocumentBrowse/Edit, KeyValue, Search,
                      SchemaIntrospect, Ddl, Explain, Aggregate, VectorSearch, StreamResults; EngineRegistry; Arrow helpers
  fosta-lang/         LanguageProvider trait, SchemaCache, sql/, mongo/, redis/, es_dsl/, esql/
  engines/
    fosta-mysql/ fosta-mssql/ fosta-mongo/ fosta-postgres/ fosta-sqlite/
    fosta-clickhouse/ fosta-redis/ fosta-elastic/      (each: adapter.rs, introspect.rs, types.rs, dialect.rs, tests/)
    fosta-remote/     RemoteEngine: spawns Tier-2 drivers, JSON-RPC + Arrow framing, manifests, supervision
  fosta-analytics/    DuckDB (feature "duckdb")
  fosta-ai/           LLM providers
  fosta-engine-tests/ capability contract suites + fixtures
  fosta-tauri/        app binary: Tauri commands, channels, OS secret store
  fosta-web/          HTTP/WebSocket server for the Docker edition
drivers/              out-of-process drivers (fosta-driver-oracle, sample-driver), each with manifest.json
```

### TypeScript (`src/`)

```
app/            shell, navigation, view registry, command palette
core/           api/ (transport: tauri | websocket), types (generated from fosta-proto), signals stores, arrow/ (worker client)
shared/         grid/ (Arrow data source), tree/ (virtual), jsonTree/, editor/ (CodeMirror + language bridge), dialogs/
engines/
  sql/          table view, query view, design view (shared by MySQL/MSSQL/PG/SQLite/ClickHouse/Oracle)
  mongo/        collection document view, aggregation builder, shell editor
  redis/        key browser, typed value editors, console
  elastic/      index view, mapping, DSL/ES|QL/SQL editor, aggregations, ingest simulate, cluster/ILM/snapshots/tasks
  analytics/    DuckDB notebook-style view
  index.ts      registrations (dynamic imports only)
features/       cross-engine: assistant, history, importExport, profiling, workspaces, tasks
workers/        arrowDecode.worker.ts, export.worker.ts
```

## Capabilities per planned engine

| Engine | Core caps | Optional caps | Languages | Notes |
|---|---|---|---|---|
| PostgreSQL | Sessions, cancel (`pg_cancel_backend`), transactions | Tabular browse/edit, schema and DDL, explain (JSON), streaming, estimated counts, vector (pgvector), roles | SQL (Postgres) | `tokio-postgres`; schemas are a level in the tree |
| SQLite | Sessions, transactions, cancel (interrupt) | Tabular browse/edit, schema and DDL, explain | SQL (SQLite) | `rusqlite`; file-based, no server |
| ClickHouse | Sessions (HTTP query id), cancel (`KILL QUERY`) | Tabular browse (read-mostly), schema, explain, streaming, estimated counts | SQL (ClickHouse) | Official `clickhouse` crate, RowBinary over HTTP ([docs](https://clickhouse.com/docs/integrations/language-clients/rust)). Edits only through `ALTER UPDATE`; mark them as mutations |
| Oracle | Sessions, transactions, cancel (break) | Tabular browse/edit, schema and DDL (packages, sequences), explain plan | SQL (Oracle, PL/SQL) | Tier 2, out of process (needs Oracle client libraries) |
| Redis | Sessions (per database), running commands | Key-value browse (SCAN; typed values: string, hash, list, set, zset, stream, JSON), TTL, pub/sub, slowlog, info, cluster | Redis commands | No tables; key-prefix tree |
| Elasticsearch / OpenSearch | HTTP session, task cancel | Document browse/edit, search, aggregations, schema (mappings, templates), ingest, cluster admin, ILM/ISM, snapshots, tasks, vector (kNN) | Query DSL, ES&#124;QL (ES), SQL, PPL (OpenSearch) | Plain HTTP; flavour detected from the server |

Existing engines: MariaDB/MySQL and SQL Server get tabular, schema, explain and streaming capabilities. MongoDB moves to document
browse/edit, aggregation and schema sampling.

## Phased plan

**Now (the refactor):**

1. Convert to a Cargo workspace. Move the code into `fosta-proto`, `fosta-core`, `fosta-engine`, three engine crates and
   `fosta-tauri`. No behaviour change, and the live tests stay green.
2. Introduce the capability traits, `ObjectRef` and the engine-described tree. Port MySQL and MSSQL to the tabular capabilities,
   and MongoDB to the document capabilities.
3. Switch results to Arrow `RecordBatch`es streamed over a Tauri Channel, with Flechette in a worker and a grid data source over
   Arrow. Lift the 1000-row cap and add cancellation tokens.
4. Build the statement gateway on top of `fosta-lang` (sqlparser classification), with `SchemaCache` levels 1-2.
5. UI: the view registry, lazy engine folders and signals stores. Add the contract test suite with testcontainers.
6. Set `panic = "unwind"` and run each engine call in its own task.

**Next:**

1. PostgreSQL and SQLite, which check that the SQL views can be reused.
2. Redis and Elasticsearch/OpenSearch, which check the registry with non-SQL views.
3. ClickHouse.
4. Tree-sitter completion.
5. The web edition reuses `fosta-core`, with binary WebSocket frames.

**Later:**

- `fosta-remote`, the published driver protocol, and Oracle as its first user.
- The DuckDB analytics component.
- An ADBC loader for Snowflake, BigQuery and Flight SQL.
- A standalone LSP wrapper.
- WASM extensions for renderers and formatters.
- A signed plugin registry.

**Measure before committing:**

- Arrow IPC vs JSON on real Fosta result sets in WebView2: time to first rows, and at 100k rows.
- DuckDB's size on Windows.
- Whether `panic = "unwind"` changes the binary size noticeably.
