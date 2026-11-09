# Features

The full feature list of Fosta Studio. The [README](../README.md) has the short version.

- **Eleven engines**, each a self-contained adapter in `src-tauri/src/engines/<id>/` behind the engine contract
  (`src-tauri/src/contract/`), so workspaces, safety rules and the assistant work the same on all of them. Every engine takes an SSH
  tunnel and TLS settings except SQLite (a file). Per-engine notes are in [docs/engines](engines).
  - **MariaDB and MySQL** (`mysql_async`): typed grid, design view, ER diagrams, Stop with `KILL QUERY`, transactions across runs;
    vector tools on MariaDB 11.7+.
  - **SQL Server** (`tiberius`, TLS on by default): `schema.table` names (`dbo` is hidden), `OFFSET/FETCH` paging, batches that
    return every result set.
  - **MongoDB** (`mongodb`): collections as tables with BSON-typed columns and edits, JSON filters, Atlas URIs, query tabs that run
    database commands as JSON (`{ "find": "products", "filter": {...} }`).
  - **PostgreSQL**: schemas, materialized views, functions, sequences, types and extensions; arrays, `jsonb` and `uuid` values; Stop
    cancels on the server; `postgres.explain`; vector tools with pgvector.
  - **SQLite**: a database file (read-only, create, `:memory:`) with attached databases; edits by primary key or `rowid`.
  - **ClickHouse** (HTTP interface): streaming queries with `KILL QUERY`, edits as asynchronous mutations on MergeTree tables, a
    Parts and mutations view and a Server view with running queries and the query log.
  - **Oracle**: schemas, packages, procedures and PL/SQL source with compile errors, `OCIBreak` to stop a statement, edits by key or
    `ROWID`. Needs Oracle Instant Client; see the note below.
  - **Redis** (and Valkey): key browser with `SCAN`, TTL and memory, an editor for each data type (strings, hashes, lists, sets, sorted
    sets, streams), a console with completion and a server view. Redis Cluster and Sentinel are not supported yet.
  - **Elasticsearch and OpenSearch**: indices, data streams and aliases, a document grid typed by the mapping, a Dev Tools console,
    ES|QL and SQL tabs, `search_after` paging past 10,000 documents, Mapping as the design view.
- **Workspaces**: each workspace (project, customer, environment) has its own connections, commands and tabs. Switch from the rail on
  the left or with `Ctrl+1…9`; tabs of other workspaces stay open. `Ctrl+B` (or the sidebar button) hides the connections and
  commands; the rail stays.
- **Commands**: background programs of a workspace (e.g. `kubectl port-forward`) with restart and backoff, live logs, and child
  processes killed when the app exits. Each starts *with the workspace* or *when a connection needs it*. The Commands panel and the
  toolbar's Commands menu start, stop, restart and open the log of each; the panel shows which connections use a command.
- **A connection's command**: Edit connection → Command picks one of the workspace's commands, or *New command…* (a
  `kubectl port-forward` template on the connection's port). Connect reuses the command when it runs, else starts it and waits until
  it is ready: its output contains the ready text, or else the connection's host and port accept connections (timeout 30 s by
  default). If it exits or times out, connect fails with `COMMAND_FAILED` and the last log lines. Connections share a command; one
  started for them stops when the last of them disconnects (or is deleted, or picks another command), one started with the workspace
  or by hand keeps running. Test connection uses the running command or starts it just for the test. Deleting a command clears it
  from its connections; moving a connection to another workspace clears its command. Right-click the connection → Command log. In
  the web edition commands need `FOSTA_STUDIO_TASKS=on`.
- **Connections**: saved profiles, parameters or a connection string; passwords live in Windows Credential Manager (an encrypted file in the web edition), never in the config file.
- **SSH tunnel and TLS**: password, key file or agent authentication, one jump host, host key checks (accept new, strict or off),
  keepalives and automatic reconnect. TLS modes run from `disable` to `verifyFull`, with your own CA, a client certificate, a server
  name or a pinned SHA-256 fingerprint. See [docs/engines/ssh-tls.md](engines/ssh-tls.md).
- Tree of connections → databases → tables/views, with search.
- **Table data**: paging, `WHERE` or per-column filters, sorting, inline cell editing, insert/delete rows, a
  record panel that follows foreign keys, import (CSV/TSV/JSON) and export (CSV, CSV for Excel, JSON, SQL INSERT, Markdown).
  - **Columns**: show or hide columns (toolbar button or a column header's right-click menu); the choice is remembered per table, and
    exports still include hidden columns.
  - **Safe edits**: a save is checked against the values you loaded. If a row changed meanwhile you get a conflict dialog (Reload or
    Overwrite) instead of a silent overwrite, and no transaction is held open while you edit.
  - **Cell editor** (`Shift+Enter`): a large editor for long values with line/char counts, word wrap, NULL, and Format / Minify with
    validation for JSON; binary and vector values open read-only. Saving adds a normal pending edit.
- **Query editor**: syntax highlighting, column-aware completion and a formatter for every SQL dialect, completion for MongoDB and
  Redis commands, several statements per run, streaming results, a Stop that cancels on the server, and one session per tab
  (`BEGIN`/`COMMIT` can span runs). Errors keep their code, position and statement and show in an error panel; the application log
  and copyable diagnostics help with bug reports.
  - **Transactions**: an auto-commit switch and Begin / Commit / Rollback in the toolbar, with the transaction state reported by the
    database after every run (engines without transactions, such as MongoDB and Redis, say so). Closing a tab or the app with an
    open transaction asks first, and a reminder appears when a transaction stays open for five minutes.
  - **Query history**: every run (text, connection, database, time, duration, rows or error) is listed in the tab's History panel, with
    search; Insert or open an entry in a new tab. History stays on this machine, in `fosta-studio-history.jsonl` next to the `.exe` (newest
    5,000 runs), and can be turned off or cleared under Settings.
- **Design view** (columns + DDL) and an interactive **ER diagram** per database.
- **Vector tools** (MariaDB 11.7+, PostgreSQL with pgvector, Elasticsearch 8+, OpenSearch with the k-NN plugin): embedding search, column/index health checks, ANN recall versus exact
  search (seeded, so a run can be repeated).
- **Safety rules per workspace**: a statement gateway on every engine. Every statement the app runs (query tabs, grid filters and
  edits, imports, console requests, new databases) is parsed and classified as read, write, DDL, destructive or administrative, then checked against the workspace's rules: _Open_ (everything runs;
  DROP, TRUNCATE and UPDATE/DELETE without WHERE ask once first), _Guarded_ (confirm writes and DDL, type the table or workspace name for destructive and administrative statements), _Read-only_ (reads only, with
  read-only sessions on MariaDB/MySQL) or _Custom_. A workspace marked Production starts Read-only. A plain confirmation has a
  _Don't ask again_ box that stops asking for that kind of statement in the workspace (typed confirmations and Production
  workspaces always ask). Grid filters must be a single
  condition. Relaxing rules shows what becomes possible; relaxed workspaces carry a warning badge, and blocked, confirmed and relaxed
  operations are recorded in `fosta-studio-audit.jsonl` on this machine. Statement timeouts apply on every engine that supports them.
  For production databases, also connect with a read-only login.
- **AI agent** (right panel, `Ctrl+Shift+A`): ask in plain words and the agent looks up tables, columns and relations, writes and
  explains queries, fixes errors, reads task logs, and opens tables, diagrams or query tabs for you. Use the model of your choice (Fosta by
  default, OpenAI, Gemini, Claude, or any OpenAI-compatible endpoint such as Ollama) with your own key, stored in Credential Manager;
  requests go straight from your machine to the provider. The model sees table/column metadata and the editor text, errors and logs you
  share. Every code block has Run, Insert and New tab buttons: Run is your own run, under the workspace safety rules, and a table
  filter it proposes is pre-filled for you to apply.
  - **Read-only query tool** (optional, on by default): the agent can run one read-only statement at a time to look at real data (at
    most 50 rows, 10 seconds, cancelled by Stop). Anything that writes, changes the schema or administers the server is refused and
    handed back as a code block. Each run is shown in the chat and written to the audit log. Turn it off in Settings or per workspace;
    the rows it reads are sent to your AI provider, and Production and Read-only workspaces stay off unless you allow them.
- Light, dark or system theme.
- **Updates**: once a day the app asks GitHub for the latest release (only the app version is sent, as the user agent; turn it off
  in Settings → Check for updates, or check now from the same menu). A newer version shows *Update X.Y.Z* in the status bar. The
  installed Windows app downloads it in the background and installs it when you quit, or right away with *Restart to update*
  (open transactions and unsaved edits are asked about first); after updating, a note links to what's new. The portable app links
  to the release page, and the web edition shows the Docker commands (see [web-edition.md](web-edition.md#updating)).

## Oracle Instant Client
Oracle needs Oracle Instant Client 19c or newer on the machine. The first time you connect, Fosta Studio offers to download Instant
Client Basic Light from Oracle (pinned checksum, progress and cancel; it is Oracle's software under
[its license](https://www.oracle.com/downloads/licenses/instant-client-lic.html)) or to use a folder with an installed client (the
connection's *Instant Client folder* setting). In the Docker image, unpack a client into the data volume and enter that folder; Linux
also needs `libaio`.

Configuration (workspaces, connections, tasks, LLM providers) is stored in `fosta-studio.json` next to the `.exe`, so the app is portable (in the web edition: in the `/data` volume).
