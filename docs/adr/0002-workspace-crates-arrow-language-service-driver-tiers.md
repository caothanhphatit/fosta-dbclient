# ADR 0002: Crate workspace, Arrow results, one language service, two driver tiers

- Status: proposed (2026-11-08), pending the owner's approval; refines [ADR 0001](0001-hexagonal-core-and-engine-modules.md)
- Evidence: [research notes](../research/2026-11-architecture-research.md) with sources

## Context

ADR 0001 chose a hexagonal core with engine modules. Before the refactor starts we need the concrete shape: how the code is split,
how results travel to the UI, where SQL intelligence lives, and how engines with heavy native dependencies (Oracle) or third-party
engines plug in. Goals from the owner: a clean, readable structure, speed with large results, many engines, flexibility, and no
heavy legacy platform.

## Decision

1. **Cargo workspace** instead of one crate:
   - `fosta-proto` (wire types shared by the core, front ends and remote drivers),
   - `fosta-core` (domain services, statement gateway, ports; no Tauri, no HTTP),
   - `fosta-engine` (capability traits and the engine-described object tree from ADR 0001),
   - `fosta-lang` (the language service, see 4),
   - one crate per engine (`fosta-engine-mysql`, `-postgres`, `-sqlite`, `-mssql`, `-mongo`, `-clickhouse`, `-redis`, `-elastic`),
     each behind a Cargo feature so builds can leave engines out,
   - `fosta-remote` (host side of out-of-process drivers, see 5),
   - thin front ends `fosta-tauri` (desktop) and `fosta-web` (Docker web edition).
2. **Crash containment**: `panic = "unwind"` in release (today `abort` turns any driver panic into an app crash) and every engine
   call runs in its own task, so a failing engine reports an error instead of closing the app.
3. **Arrow for results**: engines produce typed Apache Arrow record batches, streamed to the UI as binary frames (Tauri channel
   raw bytes on desktop, binary WebSocket frames on the web edition) and decoded in a Web Worker (Flechette). The grid reads
   columns directly. This replaces JSON arrays of strings, keeps types (the basis for typed editing and MongoDB Extended JSON),
   removes the fixed row cap, and makes cancel and back-pressure natural. Adopted only after a measurement in WebView2 shows a
   clear win over the current path on 100k+ row results.
4. **One language service in the core** (`fosta-lang`), modelled on language servers: tolerant parsing while typing
   (tree-sitter), validation and the gateway's statement classification (sqlparser-rs dialects), and a schema cache filled by
   introspection. Providers per language: SQL dialects, MongoDB shell syntax, Redis commands, Elasticsearch Query DSL and ES|QL.
   Editors, the assistant's SQL self-check and the gateway all use it. Its API is LSP-shaped so it can later run as a standalone
   language server.
5. **Two driver tiers, one contract**: first-party engines with pure-Rust clients are compiled in (MySQL/MariaDB, SQL Server,
   MongoDB, PostgreSQL, SQLite, ClickHouse, Redis, Elasticsearch/OpenSearch). Engines that need native vendor libraries (Oracle
   first) and third-party engines run **out of process**, speaking versioned JSON-RPC 2.0 over stdio with `Content-Length` framing
   (so binary Arrow frames share the pipe) and a manifest. Both tiers implement the same capabilities and pass the same tests.
   WASM components and dynamic libraries are not used for now (no TLS/SQL interfaces in WASI yet; dylibs give no isolation).
6. **Elasticsearch over HTTP** (`reqwest`; the official Rust client is still alpha), OpenSearch supported by the same engine with
   detected differences (no ES|QL; SQL/PPL; ISM instead of ILM).
7. **DuckDB as an optional component** for local analytics over results and files (CSV, Parquet, JSON) and cross-engine joins;
   it adds tens of megabytes, so it is downloaded on first use rather than bundled.
8. **UI**: plain TypeScript with a view registry keyed by engine, node kind and capability; lazy `import()` per engine;
   `@preact/signals-core` for shared state; Web Workers for decoding and exports; a framework island only for a view that needs
   it. Third-party UI, if ever, runs in sandboxed iframes.
9. **Contract tests per capability** that every engine (both tiers) must pass, run locally with testcontainers and in CI as an
   engine-by-version matrix with service containers.

## Consequences

- The repository layout states the architecture: a contributor adding an engine creates one crate, implements the capabilities it
  supports, registers views and passes the contract tests.
- Big results get faster and typed; the Navicat-style row cap disappears.
- The out-of-process tier lets Oracle (and community engines) exist without risking the app or bloating the binary.
- Cost: a staged refactor (workspace split, capabilities and tree, Arrow streaming, language service, UI registry) before wiring
  the six new engines; each stage must keep the live tests green.
- Phasing: now — workspace split, capabilities and tree, crash containment, gateway on `fosta-lang`, UI registry and signals,
  contract tests, Arrow behind a measurement; next — PostgreSQL and SQLite, Redis and Elasticsearch/OpenSearch, ClickHouse;
  later — out-of-process protocol with Oracle as first user, DuckDB, ADBC, standalone language server, signed plugin registry.
