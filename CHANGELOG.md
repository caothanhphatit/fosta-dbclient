# Changelog

All notable changes to Fosta Studio are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- A MongoDB collection workspace: double-clicking a collection opens it (the grid stays in the menu as "Open as grid").
  Its Documents section has a query bar with Filter, Project, Sort, Collation, Skip and Limit in mongosh syntax
  (`ObjectId("…")`, `ISODate("…")`, unquoted keys, braces optional), errors underlined where they are, and completion
  of field paths, query operators, BSON constructors and the field's own values. Ctrl+Enter runs; ArrowUp / ArrowDown
  and the history button recall recent filters of the collection. Documents show as a List (a virtualised tree whose
  fields expand on demand), as JSON, or as a read-only Table (nested fields as dotted-path columns, arrays as
  "[n items]" that open in a side panel, mixed types flagged), paged by `_id` where the sort allows it, with the matching
  count (estimated without a filter, counted on views) and the server time. Switching modes keeps the document at the
  top. BSON values read as what they are: ObjectId with its creation time on hover, dates in local time with UTC on
  hover, Decimal128, Long, Binary/UUID, Regex and Timestamp, and any value copies as shell syntax. Fields like
  `customerId` / `customer_id` holding an ObjectId that matches a document of another collection show as inferred
  references (probed in the background, bounded and cached); opening one shows the document in a side panel with a link
  to its collection filtered by `_id`. Views open in the workspace too. When a field does not parse, the documents shown
  are dimmed and marked as the previous query's. Every read goes through the safety gateway, and the filter, sort and
  view are kept with the open tabs.
  Documents of a collection can be edited in the List and Table modes: edit a value with its type (String, Number
  keeping the stored type, Double, Int32, Int64, Decimal128, Boolean, Date, ObjectId, Null, or an object / array in
  mongosh syntax), add, rename and remove fields and array items, insert, duplicate and delete documents. Edits stay
  pending (marked in the views, counted in the pending bar) until Save; Review shows each changed document as a diff.
  A save writes nested edits by dotted path (`$set` / `$unset` of `address.city`, not the whole `address`), checks only
  the edited paths against what was loaded, opens the conflict dialog when someone changed one of them, and goes
  through the workspace's rules like a grid save. The query bar's More menu has "Update matching documents…" (a
  before/after preview of the first documents and the match count, simulated without writing; operators it cannot
  simulate exactly are refused) and "Delete matching documents…" (count and the first documents), each for the bar's
  current filter.
  Saved queries: Save keeps the bar's fields (as typed) under a name, stored with the workspace of the connection in
  `fosta-studio.json`; the Saved dropdown lists the collection's saved queries first, then the others of the database,
  with search, rename and delete, and applying one of another collection opens it in that collection's tab. Query-bar
  runs in the query history panel have "Open in collection", which opens the collection workspace with the bar filled
  in.

### Changed

- A first start no longer adds a "Local MariaDB" connection that may not exist. A workspace without connections shows a
  "Connect your first database" card on the welcome screen and a New connection button in the sidebar. Existing settings
  are not changed.

## [0.2.1] - 2026-11-11

### Added

- A "Community (Discord)" link in the settings menu and on the welcome screen, and the Discord invite in the README.
- The embedding and AI provider dialogs can use a key already saved for the same API ("Use the key saved for …") instead of
  having it typed again; only keys saved for the same host and path are offered.
- Provider presets in the embedding dialog: OpenAI, Gemini (OpenAI-compatible), Azure OpenAI (a URL template to complete,
  key sent as `api-key`) and Ollama (local, no key). A preset fills the URL, the authentication and a default model; a
  model you typed is kept. The URL field says it takes a base URL or the full `…/embeddings` URL and shows where requests go.

### Fixed

- Editing the embedding settings keeps the saved key when only the model or dimensions change, and the dialog shows it is
  kept. An endpoint given as a base URL (`…/v1`) gets `/embeddings` added; it was posted as it was and failed with HTTP 404.
- Changing the embedding endpoint dropped the saved key silently, and requests then went out without one (Gemini answered
  404, not 401). Saving now asks for the key again or a saved one ("No key" and local URLs need none), and a failed request
  that sent no key says "no API key was sent" next to the HTTP status.
- A text search whose embedding has the wrong size now says so ("The model returned 768 dimensions, the column is
  VECTOR(1024)") before the search runs, instead of a generic dimension error.

## [0.2.0] - 2026-11-09

### Added

- **Workspace Commands**: startup tasks become the workspace's *Commands* (panel, and a toolbar menu to start, stop, restart or
  open the log of each). A command starts with the workspace or when a connection needs it, and a connection can be bound to one
  (Edit connection → Command, or *New command…* with a `kubectl port-forward` template): connect starts it if needed and waits
  until it is ready. Connections share a command; one started for them stops with the last of them. Failures show
  `COMMAND_FAILED` with the log tail; the log opens from the connection's menu.
- **MCP server**: Claude Code, Claude Desktop, Cursor, VS Code and Zed can use saved connections without seeing passwords
  (Settings → MCP server, off by default). Streamable HTTP on `127.0.0.1` with a bearer token kept in Credential Manager, a stdio
  bridge (`fosta-studio mcp`, `fosta-studio-server mcp`), and `/mcp` on the web edition behind its access token. Each workspace
  opts in (*MCP clients: Off / Read-only / Ask before writes*; Production and Read-only workspaces at most Read-only). Tools:
  `list_connections`, `list_databases`, `list_tables`, `describe_table`, `schema_overview`, `run_read_query` and `run_query`, which
  the user approves statement by statement in a dialog naming the client. Every call is audited; see [docs/mcp.md](docs/mcp.md).
- **Updates**: a daily check for a new release (Settings → Check for updates, on by default; *Check for updates now* any time) and
  an *Update X.Y.Z* item in the status bar. The installed Windows app downloads the update in the background and installs it on
  quit or with *Restart to update*, asking first about open transactions and unsaved edits, then shows what's new once. The
  portable app links to the release; the web edition shows the Docker commands, and `deploy/docker-compose.yml` has an optional
  Watchtower service (`--profile autoupdate`).
- **Vector tools** (columns, indexes, similarity search, health checks and index recall) on more engines, in addition to MariaDB
  from 0.1.0:
  - **PostgreSQL (pgvector)**: `vector` and `halfvec` columns, HNSW and IVFFlat indexes with their metric, search by cosine, L2 or
    inner product, when the extension is installed in the database. The dev and CI image is now `pgvector/pgvector`.
  - **Oracle 23ai**: `VECTOR` columns, IVF and HNSW indexes, `VECTOR_DISTANCE` search.
  - **Elasticsearch and OpenSearch**.
  - **Redis 8**: search indexes with `VECTOR` fields (FLAT or HNSW), KNN search.
  - **ClickHouse**: `Array(Float32)` columns and `vector_similarity` indexes.
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
- **New logo** (an F made of data rows) in the app icon, welcome screen, web favicon, website and README, and **engine logos** for
  PostgreSQL, SQLite, ClickHouse, Oracle, Redis, Elasticsearch and OpenSearch.
- **Hide the sidebar** with `Ctrl+B` or its button, and **hide the workspace rail** with its small arrow (`Ctrl+Shift+B`).
- **Production hint**: saving a connection whose name, host, database or URL says prod, production, prd or live, outside a Production
  workspace, offers to move it there (read-only by default).
- **Trust**: a security and privacy page ([docs/security.md](docs/security.md)), [SECURITY.md](SECURITY.md) with how to report a
  vulnerability, issue templates (bug report, feature request), and CI audits of Rust and npm dependencies.

### Changed

- **Performance**:
  - MariaDB/MySQL query tabs stop a read at the row cap instead of reading the rest (`SELECT *` on 1M rows: 3.4 s → 0.14 s).
  - Paging quickly no longer exhausts the connection pool: a replaced or closed page read is cancelled on the server (KILL
    QUERY, PostgreSQL cancel); a busy MariaDB/MySQL pool says so instead of asking whether the server is reachable; page reads
    stop after 120 s when the rules set no timeout.
  - The last page reads from the end (reversed order, no deep OFFSET) and counts exactly, so it is right on tables larger than
    their statistics and fast when sorted.
  - CSV and JSON export of every row streams to the file (desktop) or the browser's download (web) instead of building it in the
    page; the files are unchanged (except integers beyond 2^53, now exact in JSON).
  - PostgreSQL scripts run in about one round trip per statement (a 3,750-statement script: 22.8 s → 4.5 s).

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
- **Web edition**: wrong MCP tokens at `/mcp` are limited per client in a bucket of their own, so an MCP client with a stale token
  can no longer lock every browser out of sign-in for 15 minutes.

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

[Unreleased]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.0
