# Changelog

All notable changes to Fosta Studio are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Assistant panel: selecting a database in the tree updates the panel at once (its context, the Learn button and the
  "Learn this database" chip), instead of after reopening the panel or one click late.
- Assistant panel: markdown tables in answers render as tables instead of one line of pipes.
- Assistant panel: each connection and database has its own conversation, so switching the tab or the tree selection to
  another database no longer sends the previous one's history; going back shows the earlier chat. The query bar's and
  the stage cards' "Ask" go to the conversation of that collection's database.
- Assistant panel: the query bar's and the stage cards' "Ask" show the user's own words (e.g. "Filter orders: paid
  orders over 100") instead of the instructions sent to the model.
- Assistant panel: documents the assistant sampled are labelled "Sampled documents from <collection>" (or "Previewed a
  pipeline on <collection>") instead of "Ran a read-only query", with ObjectIds and dates shown as `ObjectId("…")` and
  `ISODate("…")` instead of Extended JSON.
- Assistant panel: a "Thinking…" row shows while the model works between tool calls.
- Assistant panel: the quick prompts of a MongoDB collection say "Query this collection" and ask for documents, not rows.

### Security

- Assistant: rows and documents that `run_read_query` and `sample_documents` send to the AI provider, and show in the
  chat, are redacted like learned profiles. Fields named like credentials (`password`, `token`, `api_key`, `pin`,
  `card`…) keep their name and show `[redacted]` at any depth (nested documents, dotted keys, arrays, JSON columns), and
  secret-looking values (API keys, tokens, password hashes, passwords in URLs) are replaced. Before, a sampled
  `contact.password` reached the provider although the learned profile marked it sensitive.
- Learn this database: columns and fields named like personal data (email, phone, address, person names such as
  `full_name`, IP and MAC addresses) and network address types (`inet`, `cidr`, `macaddr`) are no longer read for value
  hints, as 0.3.0 promised. IP and MAC address values are kept only as a format, a value list that mixes emails, phone
  numbers or addresses with other values is dropped, and password hashes (`$2b$…`, `$argon2…`) count as secrets.

### Changed

- Assistant: with a learned profile, `schema_overview` is described as the fallback for objects the profile misses or
  when it is stale, so the model looks in the profile (`search_profile`, `get_object_profile`) before reading the whole
  schema live.

## [0.3.0] - 2026-11-12

This release also includes the fixes and additions of
[0.2.1](https://github.com/caothanhphatit/fosta-dbclient/blob/main/CHANGELOG.md#021---2026-11-11), which was not
published on its own.

### MongoDB collection workspace

- **Open a collection as a workspace**: double-clicking a collection opens it (the grid stays in the menu as "Open as
  grid"), with sections for Documents, Aggregations, Schema, Indexes, Explain and Validation. Views open in the
  workspace too. Every read goes through the safety gateway, and each section's state is kept with the open tabs.
- **Query bar**: Filter, Project, Sort, Collation, Skip and Limit in mongosh syntax (`ObjectId("…")`, `ISODate("…")`,
  unquoted keys, braces optional), errors underlined where they are, and completion of field paths, query operators,
  BSON constructors and the field's own values. Ctrl+Enter runs; ArrowUp / ArrowDown and the history button recall
  recent filters of the collection. When a field does not parse, the documents shown are dimmed and marked as the
  previous query's.
- **Document views**: a List (a virtualised tree whose fields expand on demand), JSON, or a read-only Table (nested
  fields as dotted-path columns, arrays as "[n items]" that open in a side panel, mixed types flagged), paged by `_id`
  where the sort allows it, with the matching count (estimated without a filter, counted on views) and the server time.
  Switching modes keeps the document at the top. BSON values read as what they are: ObjectId with its creation time on
  hover, dates in local time with UTC on hover, Decimal128, Long, Binary/UUID, Regex and Timestamp, and any value
  copies as shell syntax.
- **Inferred references**: fields like `customerId` / `customer_id` holding an ObjectId that matches a document of
  another collection show as references (probed in the background, bounded and cached); opening one shows the document
  in a side panel with a link to its collection filtered by `_id`.
- **Edit documents** in the List and Table modes: edit a value with its type (String, Number keeping the stored type,
  Double, Int32, Int64, Decimal128, Boolean, Date, ObjectId, Null, or an object / array in mongosh syntax), add, rename
  and remove fields and array items, insert, duplicate and delete documents. Edits stay pending (marked in the views,
  counted in the pending bar) until Save; Review shows each changed document as a diff. A save writes nested edits by
  dotted path (`$set` / `$unset` of `address.city`, not the whole `address`), checks only the edited paths against what
  was loaded, opens the conflict dialog when someone changed one of them, and goes through the workspace's rules like a
  grid save.
- **Bulk changes**: the query bar's More menu has "Update matching documents…" (a before/after preview of the first
  documents and the match count, simulated without writing; operators it cannot simulate exactly are refused) and
  "Delete matching documents…" (count and the first documents), each for the bar's current filter.
- **Saved queries**: Save keeps the bar's fields (as typed) under a name, stored with the workspace of the connection in
  `fosta-studio.json`; the Saved dropdown lists the collection's saved queries first, then the others of the database,
  with search, rename and delete, and applying one of another collection opens it in that collection's tab. Query-bar
  runs in the query history panel have "Open in collection", which opens the workspace with the bar filled in.
- **Aggregations**: build pipelines as stage cards. Pick the operator from the common stages (each with a short
  description and a template), edit the stage in mongosh syntax with completion of the fields that reach that stage
  (sampled from the previous stage's output), accumulators in `$group`, expression operators and collection names for
  `$lookup` / `$unionWith` / `$out` / `$merge`; turn stages on and off, reorder them by dragging (or ArrowUp / ArrowDown
  on the handle), fold, duplicate and delete them. Each card previews its output on a bounded sample of the collection:
  the count, the time and the first documents, a `$lookup`'s joined array opened, the error on the stage that failed,
  and why later stages did not run. `$out` / `$merge` are never previewed. Run executes the pipeline through the safety
  gateway (`$out` / `$merge` confirmed there) and shows the result as a List, JSON or Table; runs go to the query
  history. Code exports the pipeline as mongosh, Node.js or Python. Documents › More › "Open as aggregation" turns the
  query bar into `$match` / `$sort` / `$skip` / `$limit` / `$project` stages, and pipelines save and load with the saved
  queries.
- **Schema**: analyses a sample of the collection (up to 10,000 documents: random, the first ones, or those matching a
  filter) and lists every field path as a tree, into sub-documents and arrays of documents: how often it is present,
  its BSON types (mixed types highlighted), array lengths and element types, the most frequent values as small bars for
  low-cardinality fields, min / max of numbers and dates, and inferred references. "Filter by this field" puts a filter
  in the Documents query bar (has the field, is missing, equals a top value, is of a type) and "Copy path" copies it.
  The analysed paths feed the query bar's completion.
- **Indexes**: keys with their direction or type (text, 2dsphere, hashed, wildcard), properties (unique, sparse, partial
  filter, TTL, collation, hidden), size, and how many operations used each since when, with an "Unused" hint for
  indexes no query used in a week. Each index copies as a mongosh `createIndex` line, can be hidden from the query
  planner and shown again, or dropped (never `_id_`; Guarded workspaces ask for the collection name). "Create index"
  opens a form with key rows (field completion, 1 / -1 / text / 2dsphere / hashed), a name, unique / sparse / hidden, a
  partial filter, TTL, collation and wildcard projection, and a live preview of the command; creating goes through the
  safety gateway as DDL.
- **Explain**: the query plan of the Documents query or of the Aggregations pipeline ("Explain" in the query bar and in
  the Aggregations header), as the planner's choice only or with execution statistics (run within the workspace's
  timeout; `$out` / `$merge` are refused). The plan is a tree of stage cards with the index, keys, filter, documents and
  keys examined against documents returned, and time; the winning plan is highlighted and rejected plans fold under it.
  Badges and warnings point out collection scans, in-memory sorts, many documents examined per document returned and
  filtered fields no index serves, and an index is suggested by the Equality, Sort, Range rule unless one already
  covers the query ("Create index…" opens the Indexes form prefilled). Classic, slot-based (MongoDB 7 and 8) and sharded
  plans are read; any other shape shows as raw JSON, which is always one click away.
- **Validation**: level (strict, moderate, off), action (error, warn) and the validator in a mongosh-syntax editor with
  `$jsonSchema` completion. "Generate from schema" builds a `$jsonSchema` from the analysed sample: a type per field (a
  list for mixed fields), required fields present in at least a chosen share of documents (100% by default), nested
  documents and array items, and optional enums for string fields with few values. "Check existing documents" counts
  the documents the draft would refuse and lists a few. Apply shows a line diff of the old and new validator, warns
  when existing documents fail strict rules (with a one-click switch to moderate), and goes through the safety gateway
  as DDL (`collMod`); Remove rules empties the validator. Server-side JavaScript is refused in validators.
- **The assistant in the workspace**: it answers with `mongo-filter` and `mongo-pipeline` blocks in mongosh syntax,
  which "Apply to query bar", "Open in Aggregations", "Insert as stage(s)" and "Replace stage" put into the collection
  after parsing them; replacing your own filter or stages shows a diff first, and nothing runs until you do. The query
  bar's "Ask" turns a sentence into a filter, and each stage card's "Ask" explains the stage, suggests the next one or
  fixes its error. The assistant can open a collection's Documents or Aggregations with its suggestion pre-filled, and
  where the workspace lets it read data, read up to 20 documents of a collection or of a pipeline stage (values cut,
  shown in the chat and audited). It sees the schema analysis (fields and types, not values), the indexes, the explain
  summary and the validation rules.

### The assistant learns your database and the app

- **Learn this database**: the assistant studies a database once and keeps what it learned, so every chat knows it
  without looking it up again. Start it from the book button in the assistant panel, the "Learn this database" chip of
  a new chat, or a database or connection in the tree. Learning reads the structure (tables, views, collections,
  columns, keys, indexes, foreign keys and relations inferred from column names; Elasticsearch mappings and aliases).
  Only where the assistant may read data on the connection does it also sample MongoDB documents (nested field paths
  and references) and Redis key names (key patterns, never values), and, with the "Sample column values" box ticked, a
  few distinct values per column (enum lists and formats); elsewhere learning is metadata only, says so, and drops
  value hints learned earlier. The dialog lists the steps with counts and can be cancelled.
- **Nothing goes to the model while learning**. Afterwards the dialog shows what a summary would send (objects,
  requests, estimated tokens, model and provider, with a model picker) and lets you "Summarise" or "Skip (structure
  only)".
- **What the assistant knows**: a tab with the summary, your notes (for the database or one object), and every object
  with its signature, a description you can edit (marked "edited by you", never replaced by the model), columns with
  value hints and relations; Refresh, "Refresh changed only" and Delete. When tables or columns change the panel says
  "Schema changed — refresh". Settings → "Assistant may learn databases" (on by default) and Edit workspace →
  "Assistant may learn databases" turn it off.
- **The assistant knows Fosta Studio itself**: for questions about the app it searches the app's own help (how-to
  entries and the user docs, built into the app, no network) and quotes the UI labels in English, and it can offer a
  button for one of the app's actions (the MCP server settings, the LLM providers, a new connection, copy
  diagnostics…). The button shows the help entry's title, is marked as a Fosta Studio action, and runs only when you
  click it.

### Security

- `open_collection_workspace` is pinned to the chat's connection and database like `sample_documents`, and both refuse
  system collections (`system.*`) and the `admin`, `local` and `config` databases. A workspace the assistant opens no
  longer runs its first page: Documents says the query was suggested and waits for Find. Previously a document holding a
  prompt injection could make the app read `admin.system.users` on its own in an Open workspace, and the next chat
  message sent the result to the provider.
- Sampled documents reach the model only when reading the view's collection is a plain read for the rules (not
  administration, not a system collection or server database).
- The assistant's collection tools refuse stages that write (`$out`, `$merge`) and stages naming another database
  (`{ db, coll }` in `$lookup`, `$unionWith`, `$merge`). `mongo-pipeline` blocks that do so are flagged in the chat, and
  their buttons ask before applying them.
- MongoDB classification: an aggregation with `$out` and a TTL index (`expireAfterSeconds`, also through `createIndexes`
  and `collMod`) are destructive instead of DDL; bulk updates and deletes whose filter trivially matches every document
  (`{ $expr: true }`, `{ _id: { $exists: true } }`, `{ $and: [{}] }`, …) are destructive like an empty filter; database and
  collection names with leading or trailing spaces are refused instead of trimmed after classification;
  `system.backup_users`, `system.new_users`, `system.profile`, `oplog.rs`, `startup_log`, the whole `local` database and the
  stages `$planCacheStats`, `$listSampledQueries` and `$queryStats` are administration.
- Change streams are refused by the full aggregation and by explain, not only by the preview (collecting one never ended
  and kept the connection busy).
- Without the read setting, values are cut from the server errors in the assistant's context (duplicate keys, quoted
  literals).
- Saved queries: ids at most 200 characters, one saved query at most 256 KB and a workspace's saved queries at most 8 MB.
- Learned databases keep no secrets or identifiers: system and credential objects are left out, columns named like
  credentials or personal data are never sampled, secret-looking text is redacted, and Redis key patterns and MongoDB
  field paths replace values used as keys (emails, ids, IP addresses, phone numbers, tokens) with placeholders. A
  learning run audits its reads as one summary entry per phase (reads by kind, failures, why it stopped) instead of one
  entry per read. See [docs/security.md](docs/security.md).

### Changed

- A first start no longer adds a "Local MariaDB" connection that may not exist. A workspace without connections shows a
  "Connect your first database" card on the welcome screen and a New connection button in the sidebar. Existing settings
  are not changed.

### Fixed

- The text selection in the query editor is visible again: a translucent accent colour, dimmer when the editor is not
  focused, no longer hidden under the active-line highlight.

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

- **Workspace Commands**: startup tasks become the workspace's _Commands_ (panel, and a toolbar menu to start, stop, restart or
  open the log of each). A command starts with the workspace or when a connection needs it, and a connection can be bound to one
  (Edit connection → Command, or _New command…_ with a `kubectl port-forward` template): connect starts it if needed and waits
  until it is ready. Connections share a command; one started for them stops with the last of them. Failures show
  `COMMAND_FAILED` with the log tail; the log opens from the connection's menu.
- **MCP server**: Claude Code, Claude Desktop, Cursor, VS Code and Zed can use saved connections without seeing passwords
  (Settings → MCP server, off by default). Streamable HTTP on `127.0.0.1` with a bearer token kept in Credential Manager, a stdio
  bridge (`fosta-studio mcp`, `fosta-studio-server mcp`), and `/mcp` on the web edition behind its access token. Each workspace
  opts in (_MCP clients: Off / Read-only / Ask before writes_; Production and Read-only workspaces at most Read-only). Tools:
  `list_connections`, `list_databases`, `list_tables`, `describe_table`, `schema_overview`, `run_read_query` and `run_query`, which
  the user approves statement by statement in a dialog naming the client. Every call is audited; see [docs/mcp.md](docs/mcp.md).
- **Updates**: a daily check for a new release (Settings → Check for updates, on by default; _Check for updates now_ any time) and
  an _Update X.Y.Z_ item in the status bar. The installed Windows app downloads the update in the background and installs it on
  quit or with _Restart to update_, asking first about open transactions and unsaved edits, then shows what's new once. The
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
  restored text left unchanged is not listed again). _Close all_, and deleting a workspace or a connection, ask one question for
  all tabs that would lose something; declining keeps the workspace or connection.
- **Query results**: click a column header (or use its menu) to sort the loaded rows (numbers and decimals exactly, timestamps by
  instant); _Refresh names_ reloads the completion names.
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
- **Stop** in a query tab shows _Stopped_ with the rows received so far instead of an error.
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

[Unreleased]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.0
