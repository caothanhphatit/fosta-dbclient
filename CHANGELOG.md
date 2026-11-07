# Changelog

All notable changes to Fosta Studio are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The app version is shown in the status bar.

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
- **AI assistant** (`Ctrl+Shift+A`) with the provider of your choice (Fosta, OpenAI, Gemini, Claude or any OpenAI-compatible endpoint
  such as Ollama) and your own key. It sees only metadata and the text you share, and never runs queries: you insert its code and run it
  yourself.
- Light, dark and system themes.
- Portable configuration: settings are stored in `fosta-studio.json` next to the executable.

[Unreleased]: https://github.com/caothanhphatit/fosta-dbclient/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/caothanhphatit/fosta-dbclient/releases/tag/v0.1.0
