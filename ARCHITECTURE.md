# DB Studio architecture

DB Studio is a [Tauri 2](https://tauri.app) desktop app: a Rust process owns the database connections, secrets and child processes, and a
webview renders a plain TypeScript UI. They talk only through Tauri commands (request/response) and events/channels (streams).

## Layers

```
 UI (src/)                     Tauri commands (src-tauri/src/commands/)        Drivers (src-tauri/src/drivers/)
 ───────────────────────       ─────────────────────────────────────────       ──────────────────────────────────
 features/* views & dialogs    connections.rs  data.rs  tasks.rs               trait Driver (mod.rs)
        │                      workspaces.rs   ai.rs    vector.rs  files.rs     ├─ mysql.rs   MariaDB / MySQL (mysql_async)
        ▼                               │                                       ├─ mssql.rs   SQL Server (tiberius)
 core/api.ts ── invoke() ─────────────► │ ── DbManager ──────────────────────►  └─ mongo.rs   MongoDB (mongodb)
             ◄─ events / Channel ────── │                                       types.rs     shared data shapes
                                        ├─ ConfigStore (config.rs) → db-studio.json
                                        ├─ secrets.rs → Windows Credential Manager
                                        ├─ TaskManager (tasks.rs, job.rs) → child processes
                                        └─ ai/ → LLM provider HTTP APIs
```

- **UI** (`src/`). `main.ts` is the app shell: it builds the layout, wires the feature modules together and holds the current config.
  `app/navigation.ts` opens tabs; `core/` has the shared pieces (`api.ts` is the only module that calls `invoke`, `types.ts` mirrors the
  Rust types). Each `features/<area>/` folder owns its views and dialogs.
- **Commands** (`src-tauri/src/commands/`). Thin functions that read `AppState`, check arguments and delegate. Their names, argument
  names and JSON shapes are the IPC contract with `core/api.ts`; `main.rs` registers every command.
- **Drivers** (`src-tauri/src/drivers/`). `DbManager` keeps one `Arc<dyn Driver>` per open connection and maps query-tab sessions to
  their connection. A driver turns engine results into the shared shapes in `drivers/types.rs` (`ColumnMeta`, `ResultSet`,
  `TablePage`, `TableDesign`, `RowChange`, …), so the UI never sees engine-specific types. Cells travel as text (`Option<String>`);
  binary values as `0x…` hex, MariaDB vectors as a short summary.

`AppState` (in `main.rs`) holds the `ConfigStore`, the `DbManager`, the `TaskManager` and the in-flight assistant runs.

## Data flow

### Browse

1. The tree (`features/connections/tree.ts`) calls `api.connect(id)`. The backend loads the profile, takes the password from the dialog
   or Credential Manager (`PASSWORD_REQUIRED` makes the UI ask), opens the driver and returns the server version.
2. Opening a table creates a `TableView` (`features/table/tableView.ts`). It sends a `BrowseRequest` (offset, limit, order, filter) to
   `browse_table`. SQL drivers build `SELECT … WHERE <filter> ORDER BY … LIMIT/OFFSET` (or `OFFSET/FETCH`), plus a `COUNT(*)` and the
   primary key; MongoDB runs `find` with the JSON filter and `count_documents`.
3. The page is rendered by the virtualised `DataGrid` (`features/grid/grid.ts`). Per-column filters are turned into a SQL condition
   or a MongoDB query by `features/grid/columnFilter.ts`.

### Edit

1. Cell edits, inserts and deletes are kept client-side as pending changes (the pending bar shows how many).
2. Saving sends them as one `apply_changes` call: a list of `RowChange` (`insert`, `update`, `delete`) where `key` holds the primary-key
   cells of the row (all non-vector columns when the table has no key).
3. SQL drivers run all statements in one transaction with bound parameters (`UPDATE … WHERE key LIMIT 1`, `TOP (1)` on SQL Server);
   binary columns accept `0x…` hex. MongoDB applies `insert_one` / `update_one($set)` / `delete_one` per change.

### Query

1. A query tab (`features/query/queryView.ts`, loaded lazily with CodeMirror) calls `open_session`: MariaDB/MySQL and SQL Server open a
   dedicated connection per tab, so `BEGIN … COMMIT` can span several runs.
2. `run_sql` sends the editor text (or the selection) to that session and returns every result set (up to 1000 rows each, flagged as
   truncated). MongoDB takes a database command as JSON.
3. `cancel_sql` issues `KILL QUERY <thread id>` on MariaDB/MySQL; the other drivers report that cancelling is not supported.

## The assistant

Code: `src-tauri/src/ai/` (backend) and `src/features/assistant/` (panel, provider dialog, model switcher).

- **Never executes anything.** The model can read metadata and ask the UI to open a view; it has no tool that runs a query or changes
  data. SQL it writes appears in code blocks with Insert / New tab buttons, and the user runs it.
- **Tools** (`ai/tools.rs`), offered only when a connection is open:
  - `list_databases`, `list_tables`, `describe_table` (columns and DDL), `schema_overview` (all tables, columns and foreign keys): read
    catalog metadata through the same `Driver` calls the UI uses. No row data is read.
  - `open_diagram`, `open_table` (optionally filtered), `open_query` (a new tab with the text in the editor): emit a `UiAction` event; the
    UI opens the view, nothing runs.
- **Context** (`ai/prompt.rs`): the system prompt lists the engine, database, table names, and what the user shares from the active tab
  (editor text, last error, the tail of a task log), with size limits.
- **Transport**: the user's own key goes straight from this machine to the provider. `ai/anthropic.rs` speaks the Messages API, and
  `ai/openai.rs` speaks `chat/completions` (OpenAI, Gemini's compatibility endpoint, Fosta, and custom endpoints such as Ollama). Replies
  stream to the UI over a Tauri `Channel`; the user can stop a run (`ai_cancel`).

## Config and secrets

- **`db-studio.json`** next to the executable (portable): workspaces with their connections and startup tasks, the active workspace, LLM
  provider connections, the active model, and the embedding API settings. No secrets. Old layouts (flat `connections` + `tasks`, the
  single `ai` setting) are migrated on load (`config.rs`).
- **Windows Credential Manager** (`secrets.rs`, service `db-studio`): connection passwords under the connection id, LLM keys under
  `ai-key-<id>`, the embedding key under `embedding-api-key`. A password in a pasted connection string is moved there, never into the file.
- **localStorage** (`db-studio.*`): UI preferences only (theme, assistant panel open/closed, diagram node positions).

## Startup tasks

`tasks.rs` runs each task as a child process, streams stdout/stderr lines to the UI as events, marks it running when a line matches the
"ready" pattern, and restarts it with backoff when it exits. On Windows `job.rs` puts children in a job object that is killed with the
app, so a `kubectl port-forward` never outlives DB Studio.

## The dev mock

`src/dev/devMock.ts` replaces the Tauri IPC with `mockIPC` when the UI runs in a plain browser (`npm run dev`, no `__TAURI_INTERNALS__`).
It answers the same commands with sample workspaces, tables and results, so the UI can be reviewed without a database. `main.ts`
imports it dynamically behind `import.meta.env.DEV`, so it is not part of the production bundle.

## How to add a database driver

1. **Backend driver**: add `src-tauri/src/drivers/<engine>.rs` with a connection type and `impl Driver for …`. Return the shared types
   from `drivers/types.rs`; render cells as text. Implement at least `list_databases`, `create_database`, `list_tables`,
   `describe_table`, `browse_table`, `apply_changes` (one transaction where the engine allows), `open_session` / `close_session` /
   `run_query` / `cancel_query`. Override `table_relations` and `schema_bulk` when the engine has foreign keys, and `close` to release
   pools.
2. **Register it**: `pub mod <engine>;` in `drivers/mod.rs` and a match arm on the profile's `driver` id in `drivers::open`. Add the
   crate to `Cargo.toml` and, if it has its own error type, a `From` conversion in `error.rs`.
3. **Frontend description**: add the id to `DriverId` in `src/core/types.ts` and an entry to `DRIVERS` in `src/core/drivers.ts`
   (default port and user, labels, `filterKind` `sql` or `json`, `supportsCancel`, extra `options` such as TLS flags).
4. **Connection strings**: teach `src/features/connections/connectionString.ts` to parse and format the engine's string, with tests in
   `connectionString.test.ts`. Passwords must come back separately, never inside `options`.
5. **Dialect details**: check identifier quoting in filters (`core/sql.ts`, `features/grid/columnFilter.ts`), the assistant's hints in
   `ai/prompt.rs`, and the export/import paths if the engine needs different SQL.
6. **Dev mock**: give `src/dev/devMock.ts` a sample connection so the UI can be previewed.
7. **Test data and live tests**: add a container and seed data to `dev/`, and a `live_<engine>` test in `src-tauri/src/live_tests.rs`
   that exercises listing, describing, browsing with a filter and a query.
8. **Docs**: list the engine in both READMEs.
