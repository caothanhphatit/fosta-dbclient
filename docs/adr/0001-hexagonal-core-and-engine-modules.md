# ADR 0001: Hexagonal core with engine modules

- Status: accepted (2026-11-08); implementation scheduled after the statement gateway and the Docker web edition land
- Deciders: project owner, maintainers

## Context

Fosta Studio started as a MariaDB client. Its backend has one `Driver` trait shaped around SQL tables (list databases, list
tables, describe, browse a page, apply row changes, run a query), and its UI has one set of views (grid, table tab, query tab)
that every engine shares, varied by a few flags (`filterKind`, `supportsCancel`, `objectsLabel`).

That fits MariaDB, MySQL and SQL Server, but not engines with a different shape:

- MongoDB is forced into a table: documents are flattened into columns and nested values become JSON strings, so users get a
  weaker SQL client instead of a document browser.
- Redis (keys and typed values), Elasticsearch (indices, mappings, Query DSL / ES|QL, aggregations, ingest pipelines, cluster
  health) and analytical engines each need their own objects, views and query languages.
- The app is getting a second front end (an HTTP/WebSocket server for the Docker web edition) next to the Tauri desktop app,
  and a second secret store (an encrypted file next to Windows Credential Manager).
- The architecture review (2026-11-07) listed the missing capability model as the main blocker for new engines and for the
  MongoDB and database-safety work.

## Decision

### Backend: ports and adapters

- **Core (domain and application services)**: browsing, querying, row edits, the statement gateway and safety rules,
  transactions, the assistant, query history, audit log, schema graph. The core knows nothing about Tauri, HTTP, a specific
  database or a specific secret store.
- **Inbound adapters** call the core: Tauri commands (desktop) and an HTTP/WebSocket server (web edition). Command names and
  JSON shapes stay the same in both.
- **Outbound ports** (traits the core depends on), each with adapters:
  - Engine capabilities (below), implemented by one adapter per engine.
  - `SecretStore`: Windows Credential Manager (desktop), encrypted file (web), in-memory (tests).
  - Config, history and audit storage: JSON / JSON Lines files.
  - Event sink: Tauri events and channels, or WebSocket messages.
  - Process runner for startup tasks and local servers; LLM providers (Anthropic, OpenAI-compatible).
- Ports exist only where there are at least two adapters or where tests need a substitute. No generic repository layer.

### Engines: modules declared by capabilities

Each engine is a module that declares what it can do instead of implementing one SQL-shaped trait:

- **Core engine port**: connect, server info, sessions (run, cancel, transaction state, close), statement classification for the
  gateway, and a `capabilities()` description sent to the UI.
- **Optional capability traits**, implemented only where they make sense: tabular browse and edit (SQL engines), document browse
  and edit (MongoDB, Elasticsearch), key-value (Redis), search (Elasticsearch, later others), schema and DDL, explain, aggregation,
  vector search, streaming results, estimated counts.
- **Object tree described by the engine**: node kinds (database, schema, table, view, collection, key prefix, index, data stream,
  alias, template, pipeline, ...) with their children and actions, instead of a hard-coded database/table tree. Objects are
  addressed by a structured reference (`{ database, schema?, name, kind }`), not dotted strings.
- **Typed values** across IPC for engines that need them (Extended JSON for MongoDB, typed cells for SQL) instead of text-only
  cells.

### Frontend: an engine view registry

- Engine modules register views per node kind and capability (grid for SQL tables, document views for collections, a typed key
  browser for Redis, index tabs for Elasticsearch). Opening a node picks the registered view.
- Editors are registered per query language: SQL dialects, MongoDB shell syntax, Redis commands, Elasticsearch Query DSL /
  ES|QL / SQL, each with its own autocomplete and assistant tools.
- Shared building blocks stay shared (grid, JSON tree, code editor, diagram, dialogs). Engine views are lazy-loaded so the app
  stays small however many engines it supports.
- Views receive their `api` through the constructor so they can be tested against fixtures.

## Consequences

- New engines are added as modules (adapter + registered views) without changing the core; the per-engine checklist in
  ARCHITECTURE.md becomes "implement the ports you support, register views".
- MongoDB moves to document views, which is the base for the planned Compass-plus experience.
- Planned engines: PostgreSQL, SQLite, ClickHouse and Oracle reuse the SQL views; Redis and Elasticsearch get their own views.
- Cost: one refactor pass over drivers, commands, the tree and the table/query views before the new engines. Done in steps,
  with the live tests against the Docker databases as the safety net; the IPC contract changes only where typed values require
  it, with a migration.
- Rejected alternative: keep one `Driver` trait and add engine flags. Cheaper now, but every non-SQL engine would stay a weaker
  table view and the flags would multiply.
