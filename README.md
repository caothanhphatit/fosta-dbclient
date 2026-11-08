# Fosta Studio

A lightweight database client (Tauri 2 + Rust, plain TypeScript frontend) for **eleven engines**: MariaDB, MySQL, SQL Server, MongoDB,
PostgreSQL, SQLite, Redis, Elasticsearch, OpenSearch, ClickHouse and Oracle. It runs as a desktop app or as a web edition in Docker, with
**workspaces** and **startup tasks**: it can run `kubectl port-forward` when a workspace opens and reconnect when the tunnel drops.

## Features

- **Eleven engines**, each a self-contained adapter in `src-tauri/src/engines/<id>/` behind the engine contract
  (`src-tauri/src/contract/`), so workspaces, safety rules and the assistant work the same on all of them. Every engine takes an SSH
  tunnel and TLS settings except SQLite (a file). Per-engine notes are in [docs/engines](docs/engines).
  - **MariaDB and MySQL** (`mysql_async`): typed grid, design view, ER diagrams, Stop with `KILL QUERY`, transactions across runs;
    vector tools on MariaDB 11.7+.
  - **SQL Server** (`tiberius`, TLS on by default): `schema.table` names (`dbo` is hidden), `OFFSET/FETCH` paging, batches that
    return every result set.
  - **MongoDB** (`mongodb`): collections as tables with BSON-typed columns and edits, JSON filters, Atlas URIs, query tabs that run
    database commands as JSON (`{ "find": "products", "filter": {...} }`).
  - **PostgreSQL**: schemas, materialized views, functions, sequences, types and extensions; arrays, `jsonb` and `uuid` values; Stop
    cancels on the server; `postgres.explain`.
  - **SQLite**: a database file (read-only, create, `:memory:`) with attached databases; edits by primary key or `rowid`.
  - **ClickHouse** (HTTP interface): streaming queries with `KILL QUERY`, edits as asynchronous mutations on MergeTree tables, a
    Parts and mutations view and a Server view with running queries and the query log.
  - **Oracle**: schemas, packages, procedures and PL/SQL source with compile errors, `OCIBreak` to stop a statement, edits by key or
    `ROWID`. Needs Oracle Instant Client; see the note below.
  - **Redis** (and Valkey): key browser with `SCAN`, TTL and memory, an editor for each data type (strings, hashes, lists, sets, sorted
    sets, streams), a console with completion and a server view. Redis Cluster and Sentinel are not supported yet.
  - **Elasticsearch and OpenSearch**: indices, data streams and aliases, a document grid typed by the mapping, a Dev Tools console,
    ES|QL and SQL tabs, `search_after` paging past 10,000 documents, Mapping as the design view.
- **Workspaces**: each workspace (project, customer, environment) has its own connections, startup tasks and tabs. Switch from the rail on
  the left or with `Ctrl+1…9`; tabs of other workspaces stay open.
- **Startup tasks**: background commands (e.g. `kubectl port-forward`) with restart and backoff, live logs, and child processes killed when
  the app exits. Auto-start tasks run when their workspace opens.
- **Connections**: saved profiles, parameters or a connection string; passwords live in Windows Credential Manager (an encrypted file in the web edition), never in the config file.
- **SSH tunnel and TLS**: password, key file or agent authentication, one jump host, host key checks (accept new, strict or off),
  keepalives and automatic reconnect. TLS modes run from `disable` to `verifyFull`, with your own CA, a client certificate, a server
  name or a pinned SHA-256 fingerprint. See [docs/engines/ssh-tls.md](docs/engines/ssh-tls.md).
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
- **Vector tools** (MariaDB 11.7+ only): embedding search, column/index health checks, ANN recall versus exact search.
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

### Oracle Instant Client

Oracle needs Oracle Instant Client 19c or newer on the machine. The first time you connect, Fosta Studio offers to download Instant
Client Basic Light from Oracle (pinned checksum, progress and cancel; it is Oracle's software under
[its license](https://www.oracle.com/downloads/licenses/instant-client-lic.html)) or to use a folder with an installed client (the
connection's *Instant Client folder* setting). In the Docker image, unpack a client into the data volume and enter that folder; Linux
also needs `libaio`.

Configuration (workspaces, connections, tasks, LLM providers) is stored in `fosta-studio.json` next to the `.exe`, so the app is portable (in the web edition: in the `/data` volume).

## Getting started

### Run with Docker (web edition)

The web edition runs the same backend and UI in a container; you use it in a browser. Once the image is published to GitHub
Container Registry:

```sh
docker run -d --name fosta-studio -p 8080:8080 -v fosta-studio-data:/data ghcr.io/caothanhphatit/fosta-studio
docker logs fosta-studio
```

Until then, build it from this repository with `docker build -t fosta-studio .` and use `fosta-studio` as the image name.

1. **Sign in**: on first start the log prints a sign-in address with a random access token, e.g.
   `http://localhost:8080/?token=3f9c…`. Open it; the token is exchanged for a session cookie (30 days) and removed from the address.
   The token is kept in the data volume; print the address again with
   `docker exec fosta-studio /app/fosta-studio-server token`.
   Signing out (`POST /api/logout`) ends **every** session, in all browsers: there is one access token per server, not per user.
   If the token may have leaked, replace it with `docker exec fosta-studio /app/fosta-studio-server rotate-token`: the old token
   and all sessions stop working at once, without a restart, and the new sign-in address is printed. (With `FOSTA_STUDIO_TOKEN`
   set, change that variable and recreate the container instead.)
2. **Data**: workspaces, connections, query history, the access token and the encrypted saved passwords live in the `/data` volume.
   Saved passwords are encrypted with a key that is also stored there (`fosta-studio-secret.key`) unless you set
   `FOSTA_STUDIO_SECRET_KEY`; **if the key is lost, saved passwords cannot be recovered** (re-enter them).
3. **Databases on your machine**: inside the container, `localhost` is the container itself. Use `host.docker.internal` as the
   host (Docker Desktop provides it; on Linux add `--add-host=host.docker.internal:host-gateway` to `docker run`). The connection
   dialog shows this hint when you type `localhost` or `127.0.0.1`.
4. **Upgrading**: `docker pull` the new image, then remove and re-create the container with the same `-v fosta-studio-data:/data`;
   everything in the volume is kept.

`deploy/docker-compose.yml` runs the same container, optionally with a MariaDB to try it out
(`docker compose -f deploy/docker-compose.yml --profile demo up -d`).

| Variable                       | Default                     | Meaning                                                                                                     |
| ------------------------------ | --------------------------- | ----------------------------------------------------------------------------------------------------------- |
| `FOSTA_STUDIO_TOKEN`           | generated, in `/data`       | Access token to sign in with (at least 16 characters).                                                      |
| `FOSTA_STUDIO_AUTH`            | `token`                     | `off` disables sign-in. Only for a trusted machine or behind your own authenticating proxy; logs a warning. |
| `FOSTA_STUDIO_SECRET_KEY`      | generated, in `/data`       | 64 hex characters (`openssl rand -hex 32`) that encrypt saved passwords and API keys.                       |
| `FOSTA_STUDIO_DATA`            | `/data`                     | Data directory.                                                                                             |
| `FOSTA_STUDIO_LISTEN`          | `0.0.0.0:8080` in the image | Address and port to listen on.                                                                              |
| `FOSTA_STUDIO_PUBLIC_URL`      | `http://localhost:<port>`   | Base of the sign-in address printed in the log.                                                             |
| `FOSTA_STUDIO_ALLOWED_ORIGINS` | none                        | Extra origins allowed to call the API, comma-separated (e.g. `https://studio.example.com` behind a proxy).  |
| `FOSTA_STUDIO_ALLOWED_HOSTS`   | any (loopback if auth off)  | Host names requests may use, comma-separated.                                                               |
| `FOSTA_STUDIO_SECURE_COOKIE`   | off                         | `on` marks the session cookie `Secure`; set it when an HTTPS proxy is in front.                             |
| `FOSTA_STUDIO_TASKS`           | off                         | `on` allows startup tasks. They run inside the container, which ships no extra programs (no `kubectl`).     |

Differences from the desktop app: exports and diagram images are downloaded by the browser, there is no "Show in folder", startup
tasks are off unless `FOSTA_STUDIO_TASKS=on`, saved passwords are in the encrypted file instead of Windows Credential Manager, and
assistant requests go from the server to the provider. There is one access token per server, not separate user accounts.
For access over a network, put the server behind HTTPS (a reverse proxy) and set `FOSTA_STUDIO_SECURE_COOKIE=on`.

### Desktop app from source

Requirements: Node 20+, Rust stable (1.88+) and either the MSVC or the GNU toolchain on Windows; see
[CONTRIBUTING.md](CONTRIBUTING.md#toolchain) for details.

```powershell
npm install
npm run tauri dev
```

Preview the UI in a browser without any database (a mock backend with sample data, dev mode only):

```powershell
npm run dev
```

Build a portable `.exe` (no installer); the result is `src-tauri\target\release\fosta-studio.exe`:

```powershell
npm run tauri build -- --no-bundle
```

Test databases (with sample data) run in Docker; see
[CONTRIBUTING.md](CONTRIBUTING.md#test-databases-and-live-tests).

## Keyboard shortcuts

| Keys                      | Action                                 |
| ------------------------- | -------------------------------------- |
| Ctrl+1 … 9                | Switch workspace                       |
| Ctrl+N                    | New connection                         |
| Ctrl+T                    | New query                              |
| Ctrl+Shift+A              | Toggle the assistant panel             |
| Double-click a connection | Connect                                |
| Double-click a table      | Open its data                          |
| Ctrl+Enter / F9           | Run SQL (the selection, or everything) |
| Enter / F2 / double-click | Edit the selected cell                 |
| Shift+Enter               | Open the selected cell in the editor   |
| Ctrl+S                    | Save grid changes                      |
| F5                        | Reload the table                       |

## Documentation

- [CONTRIBUTING.md](CONTRIBUTING.md): setup, toolchain, tests, code style, adding a driver.
- [ARCHITECTURE.md](ARCHITECTURE.md): layers, data flow, the assistant's safety model, config and secrets.
- [ROADMAP.md](ROADMAP.md): what is available today and what comes next.

## License

Fosta Studio is free software under the [GNU General Public License v3.0](LICENSE). Contributions are accepted under the
[Contributor License Agreement](CLA.md), which lets the maintainer also offer commercial licenses (for example to companies that
want to embed Fosta Studio without the GPL's obligations) while the project itself stays GPL.

### About Fosta

Fosta, the assistant's default provider, is a separate AI API service run by the author of Fosta Studio. It is listed first for
convenience only: every other provider (OpenAI, Gemini, Claude, or any OpenAI-compatible endpoint such as a local Ollama) works the
same way, nothing in Fosta Studio requires a Fosta account, and requests only go to the provider you configure.
