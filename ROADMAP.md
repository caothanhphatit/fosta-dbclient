# Roadmap

What is planned, in order. Sizes are rough (S = days, M = a week or two, L = several weeks). Items move as priorities change;
open an issue to discuss any of them before starting work.

## 1. Hardening before wider use

Findings of the architecture review that should land before the project is promoted.

- **Assistant filters never run on their own.** `open_table` from the assistant pre-fills the filter instead of applying it; browse
  queries use prepared statements on MySQL/MariaDB and reject stacked statements in SQL Server filters, so a prompt-injected filter
  cannot write. Add a test that assistant tools never mutate data. (S)
- **Content Security Policy** for the webview, and saved passwords/keys only used with the host, user or endpoint they were saved
  for. (S)
- **Per-dialect SQL in the UI**: column filters, foreign-key navigation and the INSERT export quote identifiers and literals for the
  connected engine (today they assume MySQL). (S)
- **Safe config writes**: write to a temp file and rename, keep a backup, recover from a corrupt file; single-instance guard. (S)
- **Logging**: a rotating log file with secrets redacted, and a "Copy diagnostics" action for bug reports. (S)
- **TLS for MySQL/MariaDB** with certificate options, like SQL Server. (S–M)
- **Tests and CI**: unit tests for every SQL/BSON builder, `--locked` builds, dependency auditing, Dependabot. (S–M)

## 2. Database safety

Nothing the app does should be able to hold locks or load a server by surprise.

- Open-transaction indicator per query tab, and a warning before closing a tab with an open transaction. (M)
- Statement timeouts on every engine, and Stop for SQL Server and MongoDB (MySQL already has it). (L)
- Stop reading results at the row limit instead of draining the whole result set. (L)
- Estimated row counts when there is no filter; exact counts on demand; key-based paging; exports streamed to disk by the backend. (M)
- **Production safe mode** per workspace: read-only or confirm-every-write, enforced in the backend for the grid, query tabs,
  imports and the assistant. (M)

These rest on a refactor of the driver layer: per-engine capabilities, session objects (run, cancel, transaction state), a schema
level in object names, and structured errors. (L)

## 3. MongoDB experience

Organised like MongoDB Compass, but faster to use. No Compass code, assets or branding.

- **Query bar** with shell syntax (`ObjectId()`, `ISODate()`, unquoted keys, regex literals), autocomplete of fields and operators,
  inline validation, history and saved queries. (M)
- **Documents** in List / JSON / Table views that keep the selection, virtualised trees, flattened nested columns, readable
  ObjectId and dates, click-through on inferred references, document diff. (M)
- **Aggregation builder**: one card per stage with its own preview, documents in/out and timing, enable/disable and reorder stages,
  `$lookup` preview, export to driver code, and assistant tools that edit stages (never run them). (L)
- **Schema analysis** with configurable sampling and mixed-type detection; **explain plan** as a diagram with index hints. (M)
- Requires typed values (EJSON) over IPC, real Mongo sessions, cursor paging and `maxTimeMS`. (L)

## 4. More engines

- PostgreSQL (schemas, `CancelToken`, `COPY` for import/export). (L)
- SQLite (a file instead of a server). (M)
- Redis later. Connection profiles get an endpoint type (TCP, file, URI) and typed per-driver options first. (M)

## 5. Local database servers

Create, start, stop and delete database servers for development without leaving the app.

- **Native first** (no Docker, no virtualisation, no admin rights): download official portable builds of MariaDB, MySQL,
  PostgreSQL and MongoDB into the user's app-data folder from a checksummed manifest kept in this repository; initialise a data
  directory per server; run it as a supervised child process bound to `127.0.0.1` on a free port; generate a password and store it
  in the OS credential store; create the matching connection automatically. SQLite needs only a file. SQL Server through LocalDB.
- **Docker as an option** when Docker is already running: same templates, through the Docker Engine API, touching only containers
  the app created (labelled), data in named volumes.
- Lifecycle: start, stop, restart, logs, CPU/RAM, upgrade a version on the same data, delete the server (keep or delete data, the
  latter confirmed by typing its name), backup/restore, load sample data.
- One machine-wide list of servers that workspaces reference. (L)

## 6. Web edition (Docker image)

`docker run` the whole app (backend and UI) and use it in a browser, with no local install. Useful for teams and servers.

- The backend's commands become HTTP/WebSocket endpoints (the command layer is already thin); the UI's `api` module switches
  transport; streaming (assistant, task logs) moves to WebSocket or SSE.
- Needs what the desktop app does not: authentication and users, an encrypted secret store instead of the OS keychain, per-user
  workspaces, and documented network access from the container to the databases.
- Startup tasks and local servers stay desktop-only, or run inside the container where it makes sense. (L)

## 7. Accounts, sync and teams

Optional services; the app stays fully usable offline without an account.

- End-to-end encrypted sync of workspaces and connection profiles between machines; secrets never synced in clear text. (L)
- Team sharing of workspaces and saved queries with roles; audit log; Production policies. (L)
- Config split into synced and machine-local parts, with per-entity versions; startup tasks received by sync stay disabled until
  approved on each machine. (M)

## 8. Platforms and distribution

- macOS and Linux builds: per-platform credential stores, process-group cleanup for startup tasks, OS config directory with an
  opt-in portable mode, rustls instead of native TLS. (M)
- Signed installers and automatic updates. (M)

## Decided

- License: GPL-3.0 with a CLA (see [CLA.md](CLA.md)); commercial licenses may be offered alongside.
- The assistant writes queries and opens views; it never executes statements.
- No UI framework for the shell, grid, diagram and editor; revisit for the aggregation builder only.
