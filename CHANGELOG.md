# Changelog

All notable changes to Fosta Studio are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Tunnel commands**: a command bound to a connection (e.g. `kubectl port-forward`), started and awaited before connecting, restarted
  with backoff while connected, stopped on disconnect. Failures show `TUNNEL_FAILED` with the log tail; the log opens from the
  connection's menu.

### Fixed

- **Oracle in the web edition**: the Docker image now includes `libaio`, and the server puts its Instant Client download folder on
  `LD_LIBRARY_PATH` at start, so the client downloaded from the connection dialog loads without a restart. On other Linux hosts the
  load error names the folder to put on `LD_LIBRARY_PATH`.
- **Redis key tree**: a page now ends where a `SCAN` call ends, so no key is skipped while the server rehashes.
- **SQL Server on Linux and in the web edition**: an untrusted certificate shows the same "Accept invalid certificates" hint as on
  Windows.

## [0.1.0] - 2026-11-08

First public release for Windows, plus a web edition for Docker.

### Added

- **Eleven engines** behind one engine contract, so workspaces, safety rules, the assistant and the grid work the same on each:
  - **MariaDB and MySQL**: tree, typed grid, design view, ER diagram, Stop with `KILL QUERY`, transactions across runs, statement timeouts,
    and vector tools for MariaDB 11.7 and newer.
  - **SQL Server**: schemas, bracket quoting, batches that return every result set and row count, TLS on by default.
  - **MongoDB**: collections as tables with BSON-typed columns and edits, JSON filters, Atlas URIs, query tabs that run database commands.
  - **PostgreSQL**: databases, schemas, tables, views, materialized views, functions, sequences, types and extensions; arrays, `jsonb`,
    `uuid`, `numeric` and `timestamptz` values; Stop that cancels on the server; SQLSTATE, detail, hint and error position;
    `postgres.explain`.
  - **SQLite**: open a file (read-only, create, `:memory:`) with optional attached databases; edits by primary key or `rowid`; Stop by
    interrupt; `sqlite.explain`.
  - **Redis** (and Valkey): a key browser per database with `SCAN`, type, TTL and memory; typed editors for string, hash, list, set,
    sorted set and stream values; TTL changes; a server view with `INFO` and the slow log; a console with Stop.
  - **Elasticsearch and OpenSearch**: indices, data streams and aliases; a document grid typed by the mapping with structured and Query
    DSL filters; `search_after` with a point in time beyond 10,000 documents; **Mapping** as the design view; a Dev Tools console
    (`METHOD /path` with JSON or NDJSON bodies) plus ES|QL and SQL tabs; the OpenSearch SQL plugin and basic auth.
  - **ClickHouse**: databases, tables, views, materialized views and dictionaries; keyset paging; streaming query tabs that stop with
    `KILL QUERY`; edits as asynchronous mutations on MergeTree tables; Parts and mutations and Server views with running queries and the
    query log.
  - **Oracle**: schemas with tables, views, materialized views, sequences, synonyms, packages, procedures and functions; PL/SQL source
    with compile errors; ROWID edits for tables without a primary key; `OCIBreak` to stop a statement; ORA codes with the error position.
    Oracle Instant Client is downloaded from Oracle on first use (pinned checksum, progress and cancel), or you point to an installed one.
- **SSH tunnel and TLS** for every network engine: password, key file or agent authentication, one jump host, host key checks,
  keepalives and automatic reconnect; TLS modes from `disable` to `verifyFull` with a custom CA, client certificate, server name or a
  pinned SHA-256 fingerprint.
- **Column-aware completion and a formatter** in the query editor for every SQL dialect (tables, columns, aliases, schemas), plus
  completion for MongoDB commands and Redis commands.
- **Streaming results with a real cancel**: the first rows show at once, and Stop cancels the statement on the server.
- **Optimistic concurrency on edits**: a grid save is checked against the values you loaded, and nothing is applied if a row changed.
  A conflict dialog shows what the row holds now, with Reload and Overwrite. No transaction is held open while you edit.
- **Structured errors**: server errors keep their code, position, detail and failing statement and show in a dockable error panel with a count badge in the
  status bar and a jump to the error line. The application log and copyable diagnostics help when you report a problem.
- **Compact density** by default with a Comfortable option, and a richer status bar (connection context, safety preset, task summary,
  error badge and version).
- **Statement gateway with safety rules on every engine**: every statement the app runs is classified as read, write, DDL, destructive
  or administrative, then checked against the workspace's preset (Open, Guarded, Read-only or Custom). Risky statements ask for
  confirmation (even Open asks once before DROP, TRUNCATE or UPDATE/DELETE without WHERE, with a "Don't ask again" box), Guarded
  makes destructive ones ask you to type the object or workspace name, Production workspaces start read-only, and blocked,
  confirmed and relaxed operations go to a local audit log.
  - Statements that run something else are rated by what they run: `EXPLAIN ANALYZE`/`ANALYSE`, `EXECUTE`/`EXEC`, `PREPARE`,
    `sp_executesql` and `EXECUTE IMMEDIATE`. Switching a session or transaction to read-write, changing `sql_mode`, `SET ROLE`,
    and SELECTs that call functions with side effects (`pg_terminate_backend`, `dblink`, `LOAD_FILE`, `OPENQUERY`, `UTL_HTTP`,
    ClickHouse `url()`/`remote()`…) are administrative.
  - Read-only runs are enforced by the database too: read-only sessions re-applied before every run on PostgreSQL, MySQL/MariaDB,
    ClickHouse and SQLite, a read-only transaction on Oracle, a rolled-back transaction on SQL Server, and a session-level check on
    MongoDB, Redis and Elasticsearch/OpenSearch.
  - Making a Production workspace's rules looser always needs its name typed, including a connection's own rules, moving a
    connection out of it, or removing the Production icon.
- **Saved secrets stay with their server**: changing a connection's host, port, user, SSH route or TLS settings (or an AI or
  embedding endpoint) without typing the password or key again forgets the saved one, so it is never sent to the new target.
  Certificate pinning checks the server's own certificate only.
- **AI agent** (`Ctrl+Shift+A`) with the provider of your choice (Fosta, OpenAI, Gemini, Claude or any OpenAI-compatible endpoint such
  as Ollama) and your own key. It looks up tables, columns and relations on every ready engine, writes and explains queries, fixes
  errors, reads task logs and opens tables, diagrams or query tabs.
  - Every code block has **Run**, Insert and New tab. Run is your own run: it follows the workspace safety rules and shows up to 200
    rows under the block.
  - An optional read-only query tool lets the assistant look at real data (one statement, at most 50 rows, 10 seconds, cancelled with
    Stop). Writes, schema changes and administration are refused and handed back as a code block. Each run is shown in the chat and
    written to the audit log. Turn it off in Settings or per workspace; the rows it reads go to your AI provider.
- **Web edition**: run Fosta Studio in a browser from one Docker image
  (`docker run -p 8080:8080 -v fosta-studio-data:/data ghcr.io/caothanhphatit/fosta-studio`), protected by an access token, with encrypted
  secrets in the data volume and a sign-out that ends every session.
- **Workspaces**: each workspace has its own connections, startup tasks and tabs. Switch from the rail or with `Ctrl+1…9`.
- **Startup tasks**: background commands such as `kubectl port-forward` that start with their workspace, restart with backoff, show live
  logs and stop when the app exits.
- **Connections**: saved profiles with parameters or a connection string; passwords are kept in Windows Credential Manager, never in a
  file.
- **Data grid**: paging, `WHERE` and per-column filters, sorting, inline editing, inserts and deletes, show or hide columns, a cell editor
  (`Shift+Enter`) with JSON format and validation, and a record panel that follows foreign keys.
- **Import and export**: import CSV, TSV and JSON; export CSV, CSV for Excel, JSON, SQL `INSERT` statements and Markdown.
- **Query tabs**: syntax highlighting, several statements per run, Begin / Commit / Rollback with an auto-commit switch, a warning
  before closing a tab with an open transaction, and per-tab history with search that stays on this machine.
- **Design view** with columns and DDL, and an interactive **ER diagram** per database.
- Light, dark and system themes. Settings are stored in `fosta-studio.json` next to the executable.

[Unreleased]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.0
