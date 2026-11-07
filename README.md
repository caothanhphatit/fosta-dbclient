# Fosta Studio


A lightweight desktop database client (Tauri 2 + Rust, plain TypeScript frontend) for **MariaDB, MySQL, SQL Server and MongoDB**, with
**workspaces** and **startup tasks**: it can run `kubectl port-forward` when a workspace opens and reconnect when the tunnel drops.

## Features

- **Several engines**: MariaDB and MySQL (`mysql_async`), SQL Server (`tiberius`, TLS on by default) and MongoDB (`mongodb`, connection URIs
  for Atlas). Each engine is a driver in `src-tauri/src/drivers/` behind the shared `Driver` trait.
  - SQL Server: tables show as `schema.table` (`dbo` is hidden), paging uses `OFFSET/FETCH`, DDL is rebuilt from `INFORMATION_SCHEMA`.
  - MongoDB: collections show as tables whose columns are the fields seen on the current page; nested values show as JSON. The filter box
    takes a JSON query and the query tab runs database commands as JSON (`{ "find": "products", "filter": {...} }`).
- **Workspaces**: each workspace (project, customer, environment) has its own connections, startup tasks and tabs. Switch from the rail on
  the left or with `Ctrl+1…9`; tabs of other workspaces stay open.
- **Startup tasks**: background commands (e.g. `kubectl port-forward`) with restart and backoff, live logs, and child processes killed when
  the app exits. Auto-start tasks run when their workspace opens.
- **Connections**: saved profiles, parameters or a connection string; passwords live in Windows Credential Manager, never in a file.
- Tree of connections → databases → tables/views, with search.
- **Table data**: paging, `WHERE` or per-column filters, sorting, inline cell editing, insert/delete rows, submit in one transaction, a
  record panel that follows foreign keys, import (CSV/TSV/JSON) and export (CSV, CSV for Excel, JSON, SQL INSERT, Markdown).
  - **Columns**: show or hide columns (toolbar button or a column header's right-click menu); the choice is remembered per table, and
    exports still include hidden columns.
  - **Cell editor** (`Shift+Enter`): a large editor for long values with line/char counts, word wrap, NULL, and Format / Minify with
    validation for JSON; binary and vector values open read-only. Saving adds a normal pending edit.
- **Query editor**: syntax highlighting, table-name completion, several statements per run, Stop (`KILL QUERY`), and one session per tab
  (`BEGIN`/`COMMIT` can span runs).
  - **Query history**: every run (text, connection, database, time, duration, rows or error) is listed in the tab's History panel, with
    search; Insert or open an entry in a new tab. History stays on this machine, in `fosta-studio-history.jsonl` next to the `.exe` (newest
    5,000 runs), and can be turned off or cleared under Settings.
- **Design view** (columns + DDL) and an interactive **ER diagram** per database.
- **Vector tools** (MariaDB 11.7+ only): embedding search, column/index health checks, ANN recall versus exact search.
- **Assistant** (right panel, `Ctrl+Shift+A`): chat with a model of your choice (Fosta by default, OpenAI, Gemini, Claude, or any
  OpenAI-compatible endpoint such as Ollama) using your own key, stored in Credential Manager; requests go straight from your machine to
  the provider. The model only sees table/column metadata, the editor text, errors and logs you ask about. **It never runs queries**: every
  code block has Insert / New tab buttons so you run it yourself.
- Light, dark or system theme.

Configuration (workspaces, connections, tasks, LLM providers) is stored in `fosta-studio.json` next to the `.exe`, so the app is portable.

## Getting started

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

Test databases (MariaDB, SQL Server and MongoDB with sample data) run in Docker; see
[CONTRIBUTING.md](CONTRIBUTING.md#test-databases-and-live-tests).

## Keyboard shortcuts

| Keys                         | Action                                  |
| ---------------------------- | --------------------------------------- |
| Ctrl+1 … 9                   | Switch workspace                        |
| Ctrl+N                       | New connection                          |
| Ctrl+T                       | New query                               |
| Ctrl+Shift+A                 | Toggle the assistant panel              |
| Double-click a connection    | Connect                                 |
| Double-click a table         | Open its data                           |
| Ctrl+Enter / F9              | Run SQL (the selection, or everything)  |
| Enter / F2 / double-click    | Edit the selected cell                  |
| Shift+Enter                  | Open the selected cell in the editor    |
| Ctrl+S                       | Save grid changes                       |
| F5                           | Reload the table                        |

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
