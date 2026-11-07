# Roadmap

What is planned, in order. Sizes are rough (S = days, M = a week or two, L = several weeks). Items move as priorities change;
open an issue to discuss any of them before starting work.

## Current phase

In this order, so that work touching the same code does not overlap:

1. **Grid and query tab conveniences**: show/hide columns, a cell editor for long values and JSON, query history. (in progress)
2. **Statement gateway** (section 0) **and transactions**: query tabs get an auto-commit switch plus Begin / Commit / Rollback, a
   transaction indicator reported by the engine (MySQL session status, SQL Server `@@TRANCOUNT`, MongoDB sessions), a warning
   before closing a tab with an open transaction, and an idle-transaction reminder. Commit and Rollback go through the gateway.
3. **Data profiling and data generation**:
   - *Profiling* a table or a query result: per column null share, distinct count (approximate on large tables), min/max,
     top values, length statistics, and histograms for numbers and dates; computed with server-side aggregate queries on a
     configurable sample, cancellable, time-limited. MongoDB uses `$sample` plus aggregation and also reports mixed types.
   - *Generation* of test rows by rules, not by the model: generators chosen from column types and names (names, emails,
     phone numbers, addresses, dates in a range, numbers in a range, enums, JSON templates), foreign keys filled from existing
     parent rows, nullable share, a seed for repeatable data, preview before insert, batched inserts through the gateway. The
     assistant can propose rules but does not insert.
4. **Backup and restore** (logical, no external tools required): MySQL/MariaDB and SQL Server dumps built by the app (schema
   DDL plus batched INSERTs, chosen tables, optional data), MongoDB as one JSON Lines file per collection in Extended JSON;
   progress, cancel, compression; restore replays a dump through the gateway with a summary and confirmation first. Native
   `BACKUP DATABASE` / `mongodump` support when the tools are available, and scheduled backups later with local servers.

## 0. Statement gateway

Every statement the app sends to a database goes through one gateway in the backend (`DbManager`) instead of five separate paths
(query tabs, grid filters, grid edits, imports, the assistant).

- **Caller-aware**: the gateway knows whether a statement comes from a query tab, a grid filter, a grid save, an import or the
  assistant, and applies that caller's rules.
- **Parsed, not pattern-matched**: statements are parsed per dialect (`sqlparser`). A grid filter must be a single boolean
  expression; stacked statements, comments and anything that is not an expression are rejected. Filters run as prepared statements
  where the engine supports it. MongoDB filters must be a query document; commands are classified by name.
- **Classification**: read, write (INSERT/UPDATE/DELETE/MERGE), DDL, destructive (DROP, TRUNCATE, DELETE/UPDATE without WHERE),
  administrative (users, grants, KILL, server settings).
- **Rules per workspace**, starting from presets: *Open* (everything allowed), *Guarded* (confirm writes, block destructive
  statements without confirmation), *Read-only* (reads only, enforced with read-only transactions where the engine has them), and
  *Custom*. A workspace marked Production defaults to Read-only.
- **The assistant never executes**: its filters and queries are pre-filled for the user to run, whatever the rules say.
- **User-configurable, with loud warnings**: users may relax any rule, including per connection. Relaxing shows what becomes possible
  and an example of what could go wrong; relaxing a Production workspace requires typing the workspace name; a relaxed workspace
  shows a persistent warning badge in the rail and the status bar; every relaxation is recorded in the local audit log.
- **Engine-level backstops**: statement timeouts, result row limits, read-only transactions, and documentation recommending a
  read-only database user for production.
- **Local audit log** of blocked, confirmed and relaxed operations, with the caller.

## 1. Hardening before wider use

Findings of the architecture review that should land before the project is promoted.

- **Assistant filters never run on their own** (part of the gateway): `open_table` from the assistant pre-fills the filter; a test
  checks that assistant tools never mutate data. (S)
- **Assistant hygiene**: untrusted content (editor text, task logs, errors, table comments) moves out of the system prompt into
  clearly delimited data blocks; the system prompt stays static so prompt caching works; handle `max_tokens`/`refusal` stop
  reasons; strict tool schemas validated again in the backend; log every tool call; a small eval set run against the Docker sample
  databases. Keep the chat renderer free of images and links (no exfiltration channel) and test it. (M)
- **Fewer hallucinated queries**: parse every SQL block the assistant writes and check table and column names against the schema
  (flag or send back for a fix before showing it); a "Check" button that runs `EXPLAIN` (never `EXPLAIN ANALYZE`) to validate a
  query without executing it. (M)
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

### Assistant-built databases (after sections 0, 3 and 5)

"Create a sales database on PostgreSQL with customers, orders and products, and fill it with 10,000 rows": the assistant answers
with a **plan card** (server to create, schema DDL with an ER preview, data-generation rules with sample rows) that the user can
edit and must approve. The app, not the model, then creates the server, applies the DDL through the gateway (create-only, no
DROP or changes to existing data) and runs the rule-based generator, with progress and a choice to retry or remove what was
created if a step fails. Allowed only on servers the app created or Development workspaces, never Production; every step is
audited. Works in the desktop app with local servers and in the Docker web edition.

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
