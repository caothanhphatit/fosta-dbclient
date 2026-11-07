# Changelog

All notable changes to Fosta Studio are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
