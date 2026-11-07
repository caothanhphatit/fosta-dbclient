# Fosta Studio architecture

Fosta Studio is a [Tauri 2](https://tauri.app) desktop app: a Rust process owns the database connections, secrets and child processes, and a
webview renders a plain TypeScript UI. They talk only through Tauri commands (request/response) and events/channels (streams).

## Layers

```
 UI (src/)                     Tauri commands (src-tauri/src/commands/)        Drivers (src-tauri/src/drivers/)
 ───────────────────────       ─────────────────────────────────────────       ──────────────────────────────────
 features/* views & dialogs    connections.rs  data.rs  tasks.rs               trait Driver (mod.rs)
        │                      workspaces.rs   ai.rs    vector.rs  files.rs     ├─ mysql.rs   MariaDB / MySQL (mysql_async)
        │                      history.rs                                       │
        ▼                               │                                       ├─ mssql.rs   SQL Server (tiberius)
 core/api.ts ── invoke() ─────────────► │ ── gateway/ ── DbManager ──────────►  └─ mongo.rs   MongoDB (mongodb)
             ◄─ events / Channel ────── │                                       types.rs     shared data shapes
                                        ├─ ConfigStore (config.rs) → fosta-studio.json
                                        ├─ HistoryStore (history.rs) → fosta-studio-history.jsonl
                                        ├─ AuditLog (audit.rs) → fosta-studio-audit.jsonl
                                        ├─ secrets.rs → Windows Credential Manager
                                        ├─ TaskManager (tasks.rs, job.rs) → child processes
                                        └─ ai/ → LLM provider HTTP APIs
```

- **UI** (`src/`). `main.ts` is the app shell: it builds the layout, wires the feature modules together and holds the current config.
  `app/navigation.ts` opens tabs; `core/` has the shared pieces (`api.ts` is the only module that calls `invoke`, `types.ts` mirrors the
  Rust types). Each `features/<area>/` folder owns its views and dialogs.
- **Commands** (`src-tauri/src/commands/`). Thin functions that read `AppState`, check arguments and delegate. Their names, argument
  names and JSON shapes are the IPC contract with `core/api.ts`; `main.rs` registers every command.
- **Statement gateway** (`src-tauri/src/gateway/`). Every statement the app runs on a caller's behalf passes through it before it reaches
  a driver; see [Statement gateway and safety rules](#statement-gateway-and-safety-rules).
- **Drivers** (`src-tauri/src/drivers/`). `DbManager` keeps one `Arc<dyn Driver>` per open connection and maps query-tab sessions to
  their connection. A driver turns engine results into the shared shapes in `drivers/types.rs` (`ColumnMeta`, `ResultSet`,
  `TablePage`, `TableDesign`, `RowChange`, …), so the UI never sees engine-specific types. Cells travel as text (`Option<String>`);
  binary values as `0x…` hex, MariaDB vectors as a short summary.

`AppState` (in `main.rs`) holds the `ConfigStore`, the `DbManager`, the `TaskManager`, the in-flight assistant runs and the query
`HistoryStore`.

## Data flow

### Browse

1. The tree (`features/connections/tree.ts`) calls `api.connect(id)`. The backend loads the profile, takes the password from the dialog
   or Credential Manager (`PASSWORD_REQUIRED` makes the UI ask), opens the driver and returns the server version.
2. Opening a table creates a `TableView` (`features/table/tableView.ts`). It sends a `BrowseRequest` (offset, limit, order, filter) to
   `browse_table`. The gateway checks the filter first (a single condition; a MongoDB query document). SQL drivers build `SELECT … WHERE <filter> ORDER BY … LIMIT/OFFSET` (or `OFFSET/FETCH`), plus a `COUNT(*)` and the
   primary key; MongoDB runs `find` with the JSON filter and `count_documents`.
3. The page is rendered by the virtualised `DataGrid` (`features/grid/grid.ts`). Per-column filters are turned into a SQL condition
   or a MongoDB query by `features/grid/columnFilter.ts`. Hidden columns (`features/grid/columnVisibility.ts`, the Columns popover in
   `columnsPopover.ts`) are only left out of the drawing: the grid keeps their data, filters and pending edits, and copies/exports
   still include them. Table views remember them in localStorage (`db-studio.columns.<connection>.<database>.<table>`).

### Edit

1. Cell edits, inserts and deletes are kept client-side as pending changes (the pending bar shows how many). The large cell editor
   (`features/grid/cellEditor.ts`, Shift+Enter) writes into the grid the same way; it is a plain textarea, so CodeMirror stays out of
   the main bundle.
2. Saving sends them as one `apply_changes` call: a list of `RowChange` (`insert`, `update`, `delete`) where `key` holds the primary-key
   cells of the row (all non-vector columns when the table has no key).
3. `apply_changes` goes through the gateway as a write (caller `gridSave`, or `import` for imports, whose batches share one
   confirmation). SQL drivers run all statements in one transaction with bound parameters (`UPDATE … WHERE key LIMIT 1`, `TOP (1)` on SQL Server);
   binary columns accept `0x…` hex. MongoDB applies `insert_one` / `update_one($set)` / `delete_one` per change.

### Query

1. A query tab (`features/query/queryView.ts`, loaded lazily with CodeMirror) calls `open_session`: MariaDB/MySQL and SQL Server open a
   dedicated connection per tab, so `BEGIN … COMMIT` can span several runs.
2. `run_sql` is classified and checked by the gateway, then sends the editor text (or the selection) to that session and returns every result set (up to 1000 rows each, flagged as
   truncated). MongoDB takes a database command as JSON.
3. `cancel_sql` issues `KILL QUERY <thread id>` on MariaDB/MySQL; the other drivers report that cancelling is not supported.
4. After each run the tab sends a `HistoryEntry` (text, connection id and name, driver, database, start time, duration, rows or
   affected rows per set, or the error, and whether Stop was pressed) to `history_append` without waiting for it. The History panel
   (`features/query/historyPanel.ts`) reads it back with `history_list` (newest first, filtered by connection and search text, paged
   with `before`), and can `history_delete` entries or `history_clear` everything.

5. **Transactions**: the toolbar's auto-commit switch and Begin / Commit / Rollback call `sql_transaction` (always allowed, also in
   Read-only workspaces). Every run returns the session's `TransactionState` as the engine reports it (`@@in_transaction` on MariaDB, the
   performance schema on MySQL, `@@TRANCOUNT` and `@@OPTIONS` on SQL Server); MongoDB commands run without server sessions, so the tab
   says transactions are not supported. Closing a tab or the window with an open transaction asks first (the server rolls it back), and a
   reminder appears when a transaction has been open for five minutes.

## Statement gateway and safety rules

Code: `src-tauri/src/gateway/` (backend), `src/features/safety/` (dialogs, the Safety section, pure helpers).

- **Callers**: query tabs (`run_sql`), grid filters (`browse_table`, also the vector-search filter), grid saves and imports
  (`apply_changes`), `create_database`, transaction controls and the assistant. The gateway knows which one is calling.
- **Classification** (`gateway/classify.rs`): SQL is parsed with `sqlparser` in the engine's dialect (MySQL for MariaDB/MySQL, MsSql for
  SQL Server, Generic as a fallback). Every statement, including statements nested in CTEs, `EXPLAIN ANALYZE` and control flow, gets a
  category: _read_, _transaction_, _write_ (INSERT/UPDATE/DELETE/MERGE/REPLACE, procedure calls), _DDL_, _destructive_ (DROP, TRUNCATE,
  UPDATE/DELETE without WHERE, ALTER … DROP) or _administrative_ (users, grants, KILL, SET GLOBAL, settings that switch off the app's own
  backstops). MySQL's `/*! … */` comments are classified as the code the server runs. SQL the parser does not understand is _unknown_
  and treated like a write. MongoDB commands are classified by name (`aggregate` with `$out`/`$merge` is a write, an empty
  delete filter is destructive).
- **Filters** (`gateway/filter.rs`): a SQL filter must parse as exactly `SELECT 1 WHERE <condition>` — one statement, no comments, no
  `;`, no extra clauses, no functions that sleep, lock or read files. MariaDB/MySQL browse queries also run as prepared statements. A
  MongoDB filter must be a JSON query document without server-side JavaScript operators.
- **Rules** (`gateway/rules.rs`), stored per workspace as `safety` in `fosta-studio.json` (and optionally per connection):

  | Preset    | Read                                                                                                          | Write   | DDL     | Destructive   | Administrative | Timeout |
  | --------- | ------------------------------------------------------------------------------------------------------------- | ------- | ------- | ------------- | -------------- | ------- |
  | Open      | allow                                                                                                         | allow   | allow   | allow         | allow          | none    |
  | Guarded   | allow                                                                                                         | confirm | confirm | typed confirm | typed confirm  | 60 s    |
  | Read-only | allow                                                                                                         | block   | block   | block         | block          | 60 s    |
  | Custom    | any of allow / confirm / typed confirm / block per category, plus multi-statement runs, timeout and row limit |         |         |               |                |         |

  Workspaces saved before the gateway keep working as before (Open); a workspace marked Production (shield icon) defaults to Read-only;
  new workspaces start Guarded.

- **Confirmations**: a call that needs one fails with a structured error `{ code: "CONFIRMATION_REQUIRED", level, token, expected,
categories, tables, statements, … }`; `BLOCKED` has the same shape without a token. The UI shows what will run and why, asks for the
  table or workspace name for a typed confirmation, and repeats the call with `confirmation: { token, typed }`. Tokens are random,
  single-use, valid for five minutes and bound to the caller, connection, session and exact statement text (an import run's token covers
  its batches).
- **Engine backstops**: Read-only sessions use `SET SESSION TRANSACTION READ ONLY` (and `START TRANSACTION READ ONLY`) on
  MariaDB/MySQL; statement timeouts use `max_statement_time` on MariaDB, `max_execution_time` on MySQL (SELECT only) and `maxTimeMS` for
  MongoDB browsing. SQL Server has no per-statement timeout or read-only session the app can switch on: there the gateway enforces the
  rules, and a read-only login is recommended for production. The row limit applies to query results on every engine.
- **The assistant never executes**: each tool is registered as metadata-only or view-opening (`assistant_tool_effect`). A filter from
  `open_table` is checked like a grid filter, then only pre-filled and marked as suggested; the table loads when the user applies it.
- **Relaxing rules**: the workspace dialog lists what becomes possible and an example of what could go wrong; relaxing a Production
  workspace needs its name typed (checked again by `save_workspace`). A workspace looser than its baseline shows a badge on the rail and
  in the status bar.

## Audit log

`audit.rs` appends one JSON object per blocked, confirmed or relaxed operation to `fosta-studio-audit.jsonl` next to `fosta-studio.json`
(time, caller, workspace, connection, category, statement excerpt, outcome, detail), with the same append-and-compact scheme as the query
history (newest 20,000 entries). It stays on this machine and never holds passwords.

## Query history

`history.rs` keeps every run in `fosta-studio-history.jsonl` next to `fosta-studio.json`: one JSON object per line, appended under a mutex
so runs from several tabs never interleave, on Tauri's blocking pool so the UI never waits for the disk. When the file grows past
5,000 entries plus 10% it is rewritten (temporary file, then rename) with the newest 5,000. Statements longer than 64 KiB are cut.
The history **never leaves this machine**; it holds only what the user ran (never passwords), and `queryHistory: false` in
`fosta-studio.json` (Settings → Record query history) stops recording without deleting what is there.

## The assistant

Code: `src-tauri/src/ai/` (backend) and `src/features/assistant/` (panel, provider dialog, model switcher).

- **Never executes anything.** The model can read metadata and ask the UI to open a view; it has no tool that runs a query or changes
  data. SQL it writes appears in code blocks with Insert / New tab buttons, and the user runs it.
- **Tools** (`ai/tools.rs`), offered only when a connection is open:
  - `list_databases`, `list_tables`, `describe_table` (columns and DDL), `schema_overview` (all tables, columns and foreign keys): read
    catalog metadata through the same `Driver` calls the UI uses. No row data is read.
  - `open_diagram`, `open_table` (with an optional filter that is only pre-filled as a suggestion), `open_query` (a new tab with the
    text in the editor): emit a `UiAction` event; the UI opens the view, nothing runs.
- **Context** (`ai/prompt.rs`): the system prompt lists the engine, database, table names, and what the user shares from the active tab
  (editor text, last error, the tail of a task log), with size limits.
- **Transport**: the user's own key goes straight from this machine to the provider. `ai/anthropic.rs` speaks the Messages API, and
  `ai/openai.rs` speaks `chat/completions` (OpenAI, Gemini's compatibility endpoint, Fosta, and custom endpoints such as Ollama). Replies
  stream to the UI over a Tauri `Channel`; the user can stop a run (`ai_cancel`).

## Config and secrets

- **`fosta-studio.json`** next to the executable (portable): workspaces with their connections and startup tasks, the active workspace, LLM
  provider connections, the active model, the embedding API settings and the `queryHistory` switch. No secrets. Old layouts (flat
  `connections` + `tasks`, the single `ai` setting) are migrated on load (`config.rs`).
- **Windows Credential Manager** (`secrets.rs`, service `fosta-studio`; secrets saved under the pre-rename service `db-studio` are moved on first read, and `db-studio.json` / `db-studio-history.jsonl` are renamed on first start): connection passwords under the connection id, LLM keys under
  `ai-key-<id>`, the embedding key under `embedding-api-key`. A password in a pasted connection string is moved there, never into the file.
- **`fosta-studio-history.jsonl`** next to the executable: the query history (see above). No secrets.
- **`fosta-studio-audit.jsonl`** next to the executable: the safety audit log (see above). No secrets.
- **localStorage** (`db-studio.*`): UI preferences only (theme, assistant panel open/closed, diagram node positions, hidden columns per
  table, the history panel and cell-editor wrap toggles).

## Startup tasks

`tasks.rs` runs each task as a child process, streams stdout/stderr lines to the UI as events, marks it running when a line matches the
"ready" pattern, and restarts it with backoff when it exits. On Windows `job.rs` puts children in a job object that is killed with the
app, so a `kubectl port-forward` never outlives Fosta Studio.

## The dev mock

`src/dev/devMock.ts` replaces the Tauri IPC with `mockIPC` when the UI runs in a plain browser (`npm run dev`, no `__TAURI_INTERNALS__`).
It answers the same commands with sample workspaces, tables and results, so the UI can be reviewed without a database. `main.ts`
imports it dynamically behind `import.meta.env.DEV`, so it is not part of the production bundle.

## How to add a database driver

1. **Backend driver**: add `src-tauri/src/drivers/<engine>.rs` with a connection type and `impl Driver for …`. Return the shared types
   from `drivers/types.rs`; render cells as text. Implement at least `list_databases`, `create_database`, `list_tables`,
   `describe_table`, `browse_table`, `apply_changes` (one transaction where the engine allows), `open_session` / `close_session` /
   `run_query` / `cancel_query`, and `engine` (it picks the gateway's SQL dialect). Honour the `Limits` the gateway passes (row limit,
   timeout, read-only session) where the engine allows, and override `transaction` when it has transactions. Override `table_relations`
   and `schema_bulk` when the engine has foreign keys, and `close` to release pools. Drivers only execute: classification and rules
   live in the gateway.
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
