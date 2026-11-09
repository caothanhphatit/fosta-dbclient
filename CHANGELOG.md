# Changelog

All notable changes to Fosta Studio are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Workspace commands**: startup tasks become the workspace's *Commands* (panel, and a toolbar menu to start, stop, restart or
  open the log of each). A command starts with the workspace or when a connection needs it, and a connection can use one (Edit
  connection → Command, or *New command…* with a `kubectl port-forward` template): connect starts it if needed and waits until it
  is ready. Connections share a command; one started for them stops with the last of them. Failures show `COMMAND_FAILED` with the
  log tail; the log opens from the connection's menu.
- **Hide the sidebar** with `Ctrl+B` or its button; the tabs take the width, the workspace rail stays.
- **Vector tools on PostgreSQL (pgvector)**: `vector` and `halfvec` columns, HNSW and IVFFlat indexes with their metric, similarity
  search by cosine, L2 or inner product, health checks and index recall, when the extension is installed in the database. The dev
  and CI image is now `pgvector/pgvector`.
- **Vector tools on Elasticsearch and OpenSearch**.
- **Vector tools on Oracle 23ai**: `VECTOR` columns, IVF and HNSW indexes, `VECTOR_DISTANCE` search, health checks and recall.
- **Vector tools on Redis 8**: search indexes with `VECTOR` fields (FLAT or HNSW), KNN search, health checks and recall.
- **Vector tools on ClickHouse**: `Array(Float32)` columns and `vector_similarity` indexes, search, health checks and recall.
- **Updates**: a daily check for a new release (Settings → Check for updates, on by default; *Check for updates now* any time) and
  an *Update X.Y.Z* item in the status bar. The installed Windows app downloads the update in the background and installs it on
  quit or with *Restart to update*, asking first about open transactions and unsaved edits, then shows what's new once. The
  portable app links to the release; the web edition shows the Docker commands, and `deploy/docker-compose.yml` has an optional
  Watchtower service (`--profile autoupdate`).

- **Production hint**: saving a connection whose name, host, database or URL says prod, production, prd or live, outside a Production
  workspace, offers to move it there (read-only by default).
- **Security and privacy** page ([docs/security.md](docs/security.md)) and [SECURITY.md](SECURITY.md); CI audits Rust and npm
  dependencies.
- **MCP server**: Claude Code, Claude Desktop, Cursor, VS Code and Zed can use saved connections without seeing passwords
  (Settings → MCP server, off by default). Streamable HTTP on `127.0.0.1` with a bearer token kept in Credential Manager, a stdio
  bridge (`fosta-studio mcp`, `fosta-studio-server mcp`), and `/mcp` on the web edition behind its access token. Each workspace
  opts in (*MCP clients: Off / Read-only / Ask before writes*; Production and Read-only workspaces at most Read-only). Tools:
  `list_connections`, `list_databases`, `list_tables`, `describe_table`, `schema_overview`, `run_read_query` and `run_query`, which
  the user approves statement by statement in a dialog naming the client. Every call is audited; see [docs/mcp.md](docs/mcp.md).
- **Tabs come back after a restart**: each workspace's open tabs (tables, query tabs with their text, language and database,
  diagrams, command logs) and the active tab are saved in `fosta-studio-tabs.json` next to the config (the data volume on the web) and
  restored on start. A restored tab connects only when it is shown; a tab whose connection is gone is dropped with a note. A file
  that cannot be read is never overwritten; one that does not parse is moved to `fosta-studio-tabs.json.corrupt`. Two windows of the
  web edition merge their tabs instead of overwriting each other's (a tab closed in one window but open in the other comes back).
- **Unsaved query text**: closing a query tab whose text was edited and not run asks first, and quitting lists those tabs (once:
  restored text left unchanged is not listed again). *Close all*, and deleting a workspace or a connection, ask one question for
  all tabs that would lose something; declining keeps the workspace or connection.
- **Query results**: click a column header (or use its menu) to sort the loaded rows (numbers and decimals exactly, timestamps by
  instant); *Refresh names* reloads the completion names.

### Fixed

- **Menus** no longer end partly outside the window (they were measured during their opening animation) and scroll when taller
  than the window; the arrow keys move through them.
- **Scripts with many statements**: the result tabs update incrementally, and above 50 result sets one list of them replaces the
  tabs (click one to open it), so a 15,000-statement script no longer freezes the window.
- **Stop** in a query tab shows *Stopped* with the rows received so far instead of an error.
- **Tab titles** name the connection, and the database, when two tabs share a title.
- **Lost sessions**: after the backend or the server restarted, a query tab reconnects and reopens its session on the next run, and
  the tree no longer shows a connection the backend lost as connected. The run goes again on its own only when every statement
  reads; otherwise (and when an open transaction was lost) nothing runs and the tab says the session was reset, since settings such
  as `SET ROLE`, `search_path` and temporary tables are gone.
- **Completion** names reload after a tree Refresh and after DDL runs in the tab; a new query tab on SQLite starts in `main`.
- **MariaDB/MySQL query tabs** stop a read at the row cap instead of reading the rest (`SELECT *` on 1M rows: 3.4 s → 0.14 s).
- **Paging quickly no longer exhausts the connection pool**: a replaced or closed page read is cancelled on the server (KILL
  QUERY, PostgreSQL cancel); a busy MariaDB/MySQL pool says so instead of asking whether the server is reachable; page reads
  stop after 120 s when the rules set no timeout.
- **Last page** reads from the end (reversed order, no deep OFFSET) and counts exactly, so it is right on tables larger than
  their statistics and fast when sorted.
- **CSV and JSON export of every row** streams to the file (desktop) or the browser's download (web) instead of building it in
  the page; the files are unchanged (except integers beyond 2^53, now exact in JSON).
- **PostgreSQL scripts** run in about one round trip per statement (a 3,750-statement script: 22.8 s → 4.5 s).
- **Vector index recall**: a row as close as the exact k-th result now counts as a hit, so tables with duplicate vectors no longer
  report a recall near 0.
- **Oracle in the web edition**: the Docker image now includes `libaio`, and the server puts its Instant Client download folder on
  `LD_LIBRARY_PATH` at start, so the client downloaded from the connection dialog loads without a restart. On other Linux hosts the
  load error names the folder to put on `LD_LIBRARY_PATH`.
- **Redis key tree**: a page now ends where a `SCAN` call ends, so no key is skipped while the server rehashes.
- **SQL Server on Linux and in the web edition**: an untrusted certificate shows the same "Accept invalid certificates" hint as on
  Windows.

### Security

- **SQLite**: the bundled SQLite library is updated (`libsqlite3-sys` 0.39, `rusqlite` 0.41) for CVE-2026-119375
  (RUSTSEC-2026-0459), reachable through a crafted database file.
- **MariaDB/MySQL**: a page read cancelled just as it finished no longer leaves a `KILL QUERY` behind for the next statement on
  that pooled connection (another tab's page or grid save failed with "Query execution was interrupted").
- **Data files**: the open tabs file, the configuration, the query history and the audit log are written readable by their owner
  only (mode 0600 on Linux, the web edition's volume); files from earlier versions are restricted on the next start.
- **CSV exports**: a cell that a spreadsheet would run as a formula (starting with `=`, `+`, `-`, `@`, a tab or a carriage return) is
  written with a leading single quote and quoted, in the page exporters and the streamed export alike; plain numbers such as
  `-5` are unchanged.

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
