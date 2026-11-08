# Changelog

All notable changes to Fosta Studio are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Transactions**: Begin is disabled while a transaction is open, and the backend refuses a nested Begin. On MariaDB/MySQL a second
  Begin used to commit the open transaction silently; on SQL Server it nested. Running DDL in an open MariaDB/MySQL transaction asks
  first, since it commits.
- **Toolbars and tabs**: toolbars wrap instead of hiding controls behind an invisible scrollbar, so the pager and the transaction
  controls are reachable from 1024 px wide. The tab strip scrolls with the mouse wheel, keeps the active tab in view and lists every tab
  when they overflow.
- **Dialogs**: Esc and Enter act on the top-most dialog only, and global shortcuts are ignored while a dialog is open.
- **MongoDB**: grid edits keep the stored BSON type (dates, numbers, ObjectIds; nested values through Extended JSON), query-tab
  commands read past the first batch of 101 documents, and grid columns show their BSON types.
- **SQL Server**: column filters, foreign-key navigation and `INSERT` export use bracket quoting; batches return every result set and
  row count in order; server errors are readable; an untrusted TLS certificate explains how to trust it.
- **Grids**: SQL type names instead of driver wire types; Read-only workspaces make grids read-only up front; stale page loads no
  longer overwrite newer ones.
- **Query history** records execution time, not the time spent in a confirmation dialog. Text typed while the editor loads is kept.
- **Toasts** appear at the top centre instead of covering dialog footers and the Apply/Discard bar.
- **Export** stops when its dialog is closed; an empty file name gets the chosen format's extension; CSV for Excel uses the locale's
  list separator.
- **Assistant**: hosted providers need an API key before they can be saved.
- **Web edition**: no `127.0.0.1` connection is seeded inside a container; unknown `/api/*` paths return a JSON 404 after the sign-in
  check; a failed sign-in no longer leaves the browser on the form post.
- **Security**: the desktop app runs under a strict Content Security Policy; "Show in folder" only opens files the app exported;
  unknown driver ids are refused instead of opening MySQL.
- The tree no longer shows a stale `~0` row estimate for tables that hold data.

## [0.1.0] - 2026-11-08

First public release for Windows.

### Added

- **Databases**: MariaDB, MySQL, SQL Server and MongoDB, connected with parameters or a connection string. MongoDB connection URIs
  work with Atlas, and SQL Server uses TLS by default.
- **Connections**: saved connection profiles; passwords are kept in Windows Credential Manager, never in a file.
- **Workspaces**: each workspace has its own connections, startup tasks and tabs. Switch from the rail or with `Ctrl+1…9`; tabs of other
  workspaces stay open.
- **Startup tasks**: background commands such as `kubectl port-forward` that start with their workspace, restart with backoff when they
  drop, show live logs, and stop when the app exits.
- **Database tree**: connections, databases, tables and views, with search.
- **Data grid**: paging, `WHERE` and per-column filters, sorting, inline editing, inserting and deleting rows, all saved in one
  transaction.
  - Show or hide columns, remembered per table.
  - A cell editor (`Shift+Enter`) for long values, with JSON format, minify and validation.
  - A record panel that follows foreign keys.
- **Import and export**: import CSV, TSV and JSON; export CSV, CSV for Excel, JSON, SQL `INSERT` statements and Markdown.
- **Query editor**: syntax highlighting, table-name completion, several statements per run, Stop, and one session per tab so
  transactions can span runs.
- **Query history**: every run is recorded per tab with search, and can be inserted or opened in a new tab. History stays on this
  machine and can be turned off or cleared in Settings.
- **Design view** with columns and DDL, and an interactive **ER diagram** per database.
- **MongoDB**: collections open as tables, filters take a JSON query, and query tabs run database commands as JSON.
- **Vector tools** for MariaDB 11.7 and newer: embedding search, column and index health checks, and ANN recall compared with exact
  search.
- **Safety rules per workspace**: every statement the app runs is classified as read, write, DDL, destructive or administrative and
  checked against the workspace's preset (Open, Guarded, Read-only or Custom). Risky statements ask for confirmation, destructive ones
  ask you to type the table or workspace name, and Production workspaces start read-only. Blocked, confirmed and relaxed operations are
  recorded in a local audit log.
- **Transactions in query tabs**: Begin, Commit, Rollback and an auto-commit switch, with the current state shown in the toolbar and a
  warning before closing a tab with an open transaction. Statement timeouts on MariaDB/MySQL and MongoDB.
- **AI agent** (`Ctrl+Shift+A`) with the provider of your choice (Fosta, OpenAI, Gemini, Claude or any OpenAI-compatible endpoint such
  as Ollama) and your own key. It looks up tables, columns and relations, writes and explains queries, fixes errors, reads task logs,
  and opens tables, diagrams or query tabs for you. Statements run only when you run them.
- **Web edition**: run Fosta Studio in a browser from one Docker image
  (`docker run -p 8080:8080 -v fosta-studio-data:/data ghcr.io/caothanhphatit/fosta-studio`), protected by an access token, with encrypted
  secrets in the data volume and sign-out that ends every session.
- Light, dark and system themes.
- Portable configuration: settings are stored in `fosta-studio.json` next to the executable.
- The app version is shown in the status bar.

[Unreleased]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.0
