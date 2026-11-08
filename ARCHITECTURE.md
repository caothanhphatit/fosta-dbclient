# Fosta Studio architecture

Fosta Studio is a [Tauri 2](https://tauri.app) desktop app: a Rust process owns the database connections, secrets and child processes, and a
webview renders a plain TypeScript UI. They talk only through commands (request/response) and events/channels (streams). The same
backend and UI also ship as a web edition in a Docker image (see [Two front-ends](#two-front-ends-desktop-and-web)).

## Layers

```
 UI (src/)                     Commands (src-tauri/src/commands/)       Engines (src-tauri/src/engines/<id>/)
 ───────────────────────       ──────────────────────────────────       ─────────────────────────────────────
 features/* views & dialogs    connections  engines  objects  query     descriptor.rs  EngineDescriptor
 engines/<id>/ UI modules      capability   vector   lang     ai …      adapter.rs     impl Engine / Connection
        │                               │                               … (catalog, session, values, url)
        ▼                               │                               common/        shared SQL toolkit
 core/api.ts ── invoke() ─────────────► │ ── gateway/ ── runtime/ ─────► contract/ (traits and DTOs)
             ◄─ events / Channel ────── │      ConnectionManager, registry, crash containment
                                        ├─ ConfigStore (config.rs) → fosta-studio.json
                                        ├─ HistoryStore (history.rs) → fosta-studio-history.jsonl
                                        ├─ AuditLog (audit.rs) → fosta-studio-audit.jsonl
                                        ├─ secrets.rs → Windows Credential Manager
                                        ├─ TaskManager (tasks.rs, job.rs) → child processes
                                        └─ ai/ → LLM provider HTTP APIs
```

- **UI** (`src/`). `main.ts` is the app shell: it builds the layout, wires the feature modules together and holds the current config.
  `app/navigation.ts` opens tabs; `core/` has the shared pieces (`api.ts` is the only module that calls `invoke`, `contract.ts` mirrors
  the Rust contract types, `engines.ts` serves the engine descriptors, `views.ts` the view registry; `transport.ts` carries the calls).
  Each `features/<area>/` folder owns its views and dialogs; `src/engines/<id>/` holds an engine's own views and is the only UI folder
  that may know engine ids.
- **Commands** (`src-tauri/src/commands/`). Plain functions that take `&AppState`, check arguments and delegate. Their names, argument
  names and JSON shapes are the contract with `core/api.ts`; `for_each_command!` in `commands/mod.rs` lists them for both front-ends.
- **Statement gateway** (`src-tauri/src/gateway/`). Every statement the app runs on a caller's behalf passes through it before it reaches
  an engine, in both editions; see [Statement gateway and safety rules](#statement-gateway-and-safety-rules). Its structured errors
  (`CONFIRMATION_REQUIRED`, `BLOCKED`, …) reach the UI as the same object over Tauri and over HTTP.
- **Contract** (`src-tauri/src/contract/`). The engine ports (ADR 0001): `Engine` (descriptor, `classify`, `connect`), `Connection`
  (tree `children`, `describe`, `open_session`, `invoke`, optional `TabularData`, `SchemaGraphSource`, `DatabaseAdmin`) and `Session`
  (`run` streaming `StreamEvent`s, `transaction`), plus every DTO the UI sees: typed `Value`s (`{t, v}` on the wire), `ColumnInfo`,
  `ObjectRef`, `ObjectNode`, `Page`, `RowChangeSet`, structured `AppError`s. Nothing outside an engine sees engine-specific types.
- **Runtime** (`src-tauri/src/runtime/`). `registry.rs` lists the engines of this build (one Cargo feature `engine-<id>` each);
  `ConnectionManager` keeps one `Arc<dyn Connection>` per open connection and the query-tab sessions, and runs every engine call through
  crash containment (`contain.rs`: a panic becomes `ENGINE_CRASHED` and marks only that connection crashed).
- **Engines** (`src-tauri/src/engines/<id>/`). One self-contained adapter per engine; `engines/common/` holds the shared SQL toolkit
  (statement splitter and classifier, SQL builder with bound values, typed catalog helpers). Commands, the gateway and the assistant
  reach engines only through the registry and the contract, never an engine module (`architecture_tests.rs` checks it).

`AppState` (in `lib.rs`) holds the `ConfigStore`, the `ConnectionManager`, the `TaskManager`, the in-flight assistant runs, the query
`HistoryStore` and the edition's `Capabilities`.

## Two front-ends: desktop and web

`src-tauri` is one crate: a library with the whole backend (`lib.rs`: config, secrets, the contract, runtime and engines, assistant, history, tasks,
vector tools and the command bodies) and two binaries.

|                  | Desktop (`fosta-studio`, feature `desktop`, default) | Web (`fosta-studio-server`, feature `web`)            |
| ---------------- | ---------------------------------------------------- | ----------------------------------------------------- |
| Entry            | `main.rs`: Tauri window                              | `server/main.rs`: axum + tokio, the Docker image      |
| Commands         | `#[tauri::command]` wrappers                         | `POST /api/invoke/<command>` (`server/api.rs`)        |
| Assistant stream | Tauri `Channel`                                      | NDJSON lines in the `ai_chat` response                |
| Task events      | Tauri events (`listen`)                              | Server-Sent Events, `GET /api/events`                 |
| Secrets          | Windows Credential Manager (`KeyringStore`)          | `secrets/file.rs`: XChaCha20-Poly1305 file in `/data` |
| Data directory   | next to the `.exe`                                   | `FOSTA_STUDIO_DATA` (`/data` in the image)            |
| Exports          | written to ~/Downloads, "Show in folder"             | downloaded by the browser                             |

Both front-ends are generated from `for_each_command!`, so a command added there is reachable from both with the same name, arguments
and JSON; `ai_chat` (it streams) and the desktop-only `save_download` / `reveal_file` are written by hand. What differs sits behind
small traits: `secrets::SecretStore` (installed once at startup), `events::EventEmitter` (task status and log lines) and `ai::ChatSink`
(assistant events). `Capabilities` tells the UI what the edition supports (`capabilities` command); the UI hides the rest
(`core/capabilities.ts`: elements marked `data-requires`). The web server's build needs no Tauri or WebKit, and uses rustls instead
of OpenSSL (the TLS stack is chosen per target in `Cargo.toml`: SChannel on Windows, rustls elsewhere).

### Web transport

`core/transport.ts` picks the transport per call: Tauri when `__TAURI_INTERNALS__` exists (the desktop app, and `npm run dev`, whose
mock installs it), otherwise HTTP. `api.ts` is unchanged apart from that.

- `POST /api/invoke/<command>` takes the same JSON arguments as `invoke` and returns the same JSON. Errors are `{ "error": "…" }`:
  404 unknown command, 400 arguments that do not parse, 422 the command failed (the message the desktop app would show), 401 not
  signed in, 403 failed CSRF checks, 429 too many sign-in attempts.
- `ai_chat` answers with NDJSON: `{ "event": … }` per streamed event, then `{ "result": … }` or `{ "error": … }`. Closing the response
  cancels the run. Server-Sent Events carry the broadcast task events; an `EventSource` reconnects by itself. Both need no extra
  protocol (no WebSocket upgrade) and pass through ordinary reverse proxies.

### Web security

- **Authentication**: one access token per server (`FOSTA_STUDIO_TOKEN`, or generated on first start, saved with owner-only
  permissions and printed once in the log). `/?token=…` or the sign-in form exchanges it for an `HttpOnly`, `SameSite=Strict` session
  cookie carrying its issue time, signed with HMAC-SHA256 under a key derived from the token and a random signing key
  (`fosta-studio-session-key`), valid 30 days. Signing out writes the time to `fosta-studio-sessions-since` and cookies issued before
  it are refused, so it signs out every browser (one token per server). `fosta-studio-server rotate-token` replaces the token and the
  signing key; the running server re-reads these files when their modification time changes, so it takes effect on the next request
  without a restart. Failed attempts are
  rate-limited per address and overall. `FOSTA_STUDIO_AUTH=off` disables this with a loud warning and restricts `Host` to loopback names.
- **CSRF**: API calls need the `X-Fosta-Studio` header (cross-site pages cannot send it without a CORS preflight, which is never
  approved), an `Origin` equal to the server's (or listed in `FOSTA_STUDIO_ALLOWED_ORIGINS`) and no cross-site `Sec-Fetch-Site`.
  No CORS headers are sent.
- **Headers**: a strict Content-Security-Policy (scripts only from the server; inline styles allowed for CodeMirror),
  `frame-ancestors 'none'`, `X-Content-Type-Options`, `Referrer-Policy: same-origin`, `Cache-Control: no-store` on the API.
- **Startup tasks** run programs, so they are off in the web edition unless `FOSTA_STUDIO_TASKS=on`; `TaskManager` refuses to start
  them otherwise.

## Data flow

### Browse

1. The tree (`features/connections/tree.ts`) calls `api.connect(id)`. The backend loads the profile, takes the password from the dialog
   or Credential Manager (`PASSWORD_REQUIRED` makes the UI ask), connects through the engine's adapter and returns a `ConnectInfo`
   (server version, capabilities, query languages).
   The tree then shows the engine's `ObjectNode`s from `tree_children`, loaded per node and page by page (`treeModel.ts` keeps the
   loaded pages, expansion and the search filter); node icons, row counts (`detail`), badges and menu entries (`NodeAction`s: a view, a
   query template, an engine capability or a builtin) come from the engine.
2. A double-click opens the node's default view through `app/navigation.ts` `openObject`, which picks the view from the registry
   (`core/views.ts` `resolveView`) and hands it a `ViewContext` bound to the connection. For tables that is a `TableView` (`features/table/tableView.ts`). It sends a `BrowseRequest` (object, structured or raw filter, sort, offset or keyset cursor) to
   `browse_object`. The gateway checks the filter first (structured conditions by the engine, a raw SQL filter as a single condition, a
   MongoDB query document). SQL engines build `SELECT … WHERE <filter> ORDER BY … LIMIT/OFFSET` (or `OFFSET/FETCH`) with bound values;
   MongoDB runs `find`. The `Page` carries typed values, the key columns and, where the engine has them, row versions.
3. The page is rendered by the virtualised `DataGrid` (`features/grid/grid.ts`). Per-column filters are turned into a SQL condition
   or a MongoDB query by `features/grid/columnFilter.ts`. Hidden columns (`features/grid/columnVisibility.ts`, the Columns popover in
   `columnsPopover.ts`) are only left out of the drawing: the grid keeps their data, filters and pending edits, and copies/exports
   still include them. Table views remember them in localStorage (`db-studio.columns.<connection>.<database>.<table>`).

### Edit

1. Cell edits, inserts and deletes are kept client-side as pending changes (the pending bar shows how many). The large cell editor
   (`features/grid/cellEditor.ts`, Shift+Enter) writes into the grid the same way; it is a plain textarea, so CodeMirror stays out of
   the main bundle.
2. Saving sends them as one `apply_edits` call: a `RowChangeSet` (`insert`, `update`, `delete`) where each update and delete carries
   its key and a concurrency check (the row version, or the original values).
3. `apply_edits` goes through the gateway as a write (caller `gridSave`, or `import` for imports, whose batches share one
   confirmation). SQL engines apply all changes in one transaction with bound parameters; a change that matches no row fails the whole
   set with `CONFLICT` and the current values, and the grid offers to reload or overwrite. MongoDB undoes the writes it already made.

### Query

1. A query tab (`features/query/queryView.ts`, loaded lazily with CodeMirror) calls `open_session`: SQL engines open a dedicated
   connection per tab, so `BEGIN … COMMIT` can span several runs. The session's language picks the editor support (`LanguageSupport`).
2. `run` is classified by the engine and checked by the gateway, then streams `StreamEvent`s (statement, columns, row batches,
   notices, done) over a channel while the engine runs the editor text (or the selection); the row limit (1000 by default) marks a set
   truncated. MongoDB takes a database command as JSON.
3. `cancel_run` cancels the running statement where the engine can (`KILL QUERY` on MariaDB/MySQL, an attention on SQL Server); other
   engines answer `UNSUPPORTED_CAPABILITY`.
4. After each run the tab sends a `HistoryEntry` (text, connection id and name, driver, database, start time, duration, rows or
   affected rows per set, or the error, and whether Stop was pressed) to `history_append` without waiting for it. The History panel
   (`features/query/historyPanel.ts`) reads it back with `history_list` (newest first, filtered by connection and search text, paged
   with `before`), and can `history_delete` entries or `history_clear` everything.

5. **Transactions**: the toolbar's auto-commit switch and Begin / Commit / Rollback call `transaction` (always allowed, also in
   Read-only workspaces). Every run returns the session's `TransactionState` as the engine reports it (`@@in_transaction` on MariaDB, the
   performance schema on MySQL, `@@TRANCOUNT` and `@@OPTIONS` on SQL Server); MongoDB commands run without server sessions, so the tab
   says transactions are not supported. Closing a tab or the window with an open transaction asks first (the server rolls it back), and a
   reminder appears when a transaction has been open for five minutes.

## Statement gateway and safety rules

Code: `src-tauri/src/gateway/` (backend), `src/features/safety/` (dialogs, the Safety section, pure helpers).

- **Callers**: query tabs (`run`), grid filters (`browse_object`, also the vector-search filter), grid saves and imports
  (`apply_edits`), `create_database`, engine views (`invoke_capability`), transaction controls and the assistant. The gateway knows
  which one is calling.
- **Classification** (`Engine::classify`, shared SQL classifier in `engines/common/sqlclassify.rs`): SQL is parsed with `sqlparser` in the engine's dialect (MySQL for MariaDB/MySQL, MsSql for
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
- **The assistant never changes anything**: each tool is registered as metadata-only, view-opening or read-only rows
  (`assistant_tool_effect`). `run_read_query` (`gateway/assistant.rs`) classifies one statement with the engine's `classify`, allows
  only Read statements the rules allow outright (a confirmation is never handed to the model), and runs it in a session of its own
  opened read-only, with 50 rows and a 10 s timeout. A filter from `open_table` is checked like a grid filter, then only pre-filled and
  marked as suggested; the table loads when the user applies it.
- **Run buttons in the chat** are the user's own runs: `run` on a session of their own, with the workspace rules (confirm, typed confirm,
  block) and at most 200 rows (`maxRows`).
- **Relaxing rules**: the workspace dialog lists what becomes possible and an example of what could go wrong; relaxing a Production
  workspace needs its name typed (checked again by `save_workspace`). A workspace looser than its baseline shows a badge on the rail and
  in the status bar.

## Audit log

`audit.rs` appends one JSON object per blocked, confirmed or relaxed operation and per assistant query (outcome `ran`) to `fosta-studio-audit.jsonl` next to `fosta-studio.json`
(time, caller, workspace, connection, category, statement excerpt, outcome, detail), with the same append-and-compact scheme as the query
history (newest 20,000 entries). It stays on this machine and never holds passwords.

## Query history

`history.rs` keeps every run in `fosta-studio-history.jsonl` next to `fosta-studio.json`: one JSON object per line, appended under a mutex
so runs from several tabs never interleave, on tokio's blocking pool so the UI never waits for the disk. When the file grows past
5,000 entries plus 10% it is rewritten (temporary file, then rename) with the newest 5,000. Statements longer than 64 KiB are cut.
The history **never leaves this machine**; it holds only what the user ran (never passwords), and `queryHistory: false` in
`fosta-studio.json` (Settings → Record query history) stops recording without deleting what is there.

## The assistant

Code: `src-tauri/src/ai/` (backend) and `src/features/assistant/` (panel, provider dialog, model switcher).

- **Never changes anything.** The model reads metadata, asks the UI to open a view and, where the user allows it, runs read-only
  queries. Code it writes appears in blocks with Run / Insert / New tab buttons: Run is the user's run through the gateway, with the
  result in a compact grid under the block (200 rows, "Open in tab" for the rest) and errors in the structured error shape. Which fence
  runs in which language, the quick prompts and the translation targets come from the engine descriptors (`LanguageSpec.fences`,
  `EngineDescriptor.assistant`).
- **Tools** (`ai/tools.rs`), offered only when a connection is open:
  - `list_databases`, `list_tables`, `describe_table` (columns, keys, indexes and DDL), `schema_overview` (all tables, columns and
    foreign keys): read catalog metadata through the contract (`ai/catalog.rs`: the tree's `children`, `describe`, `schema_graph`),
    so every engine gets them without engine code. No row data is read.
    Key-value engines list at most 200 keys per database, with a note; engines without databases (Elasticsearch) list their
    top-level folders.
  - `open_diagram`, `open_table` (with an optional filter that is only pre-filled as a suggestion), `open_query` (a new tab with the
    text in the editor): emit a `UiAction` event; the UI opens the view, nothing runs.
  - `run_read_query`: one read-only statement through `gateway::assistant` (Read only, 50 rows, cells cut at 200 characters, 10 s,
    cancelled with the chat). Registered with the model only when Settings → "Allow the assistant to run read-only queries" is on for
    the workspace (default on; off for Read-only and Production workspaces unless the workspace allows it). The rows go to the AI
    provider; each run shows in the chat as an expandable tool call and is written to the audit log and the application log.
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
- **Web edition**: the same accounts in `fosta-studio-secrets.enc` in the data directory, a JSON map encrypted with XChaCha20-Poly1305;
  the key comes from `FOSTA_STUDIO_SECRET_KEY` or `fosta-studio-secret.key` next to it (generated, owner-only). The server refuses
  to start rather than overwrite a store it cannot decrypt. The access token is in `fosta-studio-token`, the cookie signing key in
  `fosta-studio-session-key` and the last sign-out time in `fosta-studio-sessions-since`.
- **`fosta-studio-history.jsonl`** next to the executable: the query history (see above). No secrets.
- **`fosta-studio-audit.jsonl`** next to the executable: the safety audit log (see above). No secrets.
- **localStorage** (`db-studio.*`): UI preferences only (theme, assistant panel open/closed, diagram node positions, hidden columns per
  table, the history panel and cell-editor wrap toggles).

## Startup tasks

`tasks.rs` runs each task as a child process, streams stdout/stderr lines to the UI as events, marks it running when a line matches the
"ready" pattern, and restarts it with backoff when it exits. On Windows `job.rs` puts children in a job object that is killed with the
app, so a `kubectl port-forward` never outlives Fosta Studio.

## The dev mock

`src/dev/mock/` (a command router plus one module per engine in `src/dev/mock/engines/`) replaces the Tauri IPC with `mockIPC` when the UI runs in a plain browser (`npm run dev`, no `__TAURI_INTERNALS__`).
It answers the same commands with sample workspaces, tables and results, so the UI can be reviewed without a database. `main.ts`
imports it dynamically behind `import.meta.env.DEV`, so it is not part of the production bundle. Because it installs
`__TAURI_INTERNALS__`, `core/transport.ts` routes calls to it rather than to the web server.

## How to add an engine

An engine is one folder on each side plus test data; shared code does not change. `src-tauri/src/engines/sqlite/` is a small complete
example, `engines/mysql/` a large one.

1. **Descriptor** (`src-tauri/src/engines/<id>/descriptor.rs`): an `EngineDescriptor` with the id, label, icon, family, SQL dialect,
   the capability upper bound, the connection form (host or file, default port and user, database label, extra `fields`, auth methods,
   TLS, SSH), the query languages and the filter support. The UI renders the connection dialog and the engine pickers from it. Start
   with `status: Planned` (connect refuses with `ENGINE_NOT_WIRED`), then `Preview`, then `Ready`.
2. **Adapter** (`src-tauri/src/engines/<id>/adapter.rs`): `impl Engine` (descriptor, `classify`, `connect`, optionally
   `parse_connection_string` / `format_connection_string` with tests; passwords come back separately, never inside `options`) and
   `impl Connection` (`children` for the tree with stable node ids and cursor paging, `describe`, `open_session`, `capabilities` as the
   server's subset of the descriptor). Add `TabularData` (browse with structured filters, keyset paging where possible; apply in one
   transaction with concurrency checks and `CONFLICT`), `SchemaGraphSource`, `DatabaseAdmin` and `invoke` for engine operations as the
   engine allows. `classify` must cover every session language and every `invoke` name (unknown names are refused); `classify_common`
   handles row changes, transactions and "New database". Honour the `CallContext` limits (row limit, timeout, read-only session) and map
   driver errors to structured `AppError`s (`ENGINE_ERROR` with SQLSTATE or vendor code, `TIMEOUT`, `CANCELLED`, `CONNECTION_ERROR`, …). The SQL toolkit
   in `engines/common/` (splitter, classifier, builder, typed values) covers most of a SQL engine.
3. **Register it**: `pub mod <id>;` behind a Cargo feature `engine-<id>` in `engines/mod.rs` (added to `engines-all`), and one line in
   `runtime/registry.rs`. Commands, the gateway and the assistant pick it up from the registry; they must not import the module.
4. **Contract tests** (`engines/<id>/contract_tests.rs`): a `Fixture` (connection from `FOSTA_CONTRACT_<ID>_*` with defaults from the
   compose file, the seeded `contract_items` table, typed columns, a sleep statement, classification samples) and
   `contract_tests!(contract_<id>, fixture())`, which generates the suites (connect, tree, describe, browse, edit, query, cancel,
   transactions, classify); a suite whose capability is not advertised asserts `UNSUPPORTED_CAPABILITY`. Engine-specific checks go in
   the same file as `contract_<id>_*` tests.
5. **Test data and CI**: `dev/compose/<id>.yml` (image tag from an environment variable, test-only credentials), seeds in
   `dev/seed/<id>/`, and `ci/engines/<id>.json` (compose file, versions, paths, test filter) so `.github/workflows/live.yml` runs the
   suites on pull requests that touch the engine and nightly. Run locally with `docker compose -f dev/compose/<id>.yml up -d --wait` and
   `cargo test contract_<id> -- --ignored --test-threads=1`.
6. **UI module** (`src/engines/<id>/index.ts`): an `EngineUiModule` with the engine's own views, editor languages and node icons
   (found by `import.meta.glob`, loaded lazily); shared views cover tables, documents and queries. Add the id to `KnownEngineId` in
   `src/core/types.ts`.
7. **Dev mock**: `src/dev/mock/engines/<id>.ts` (descriptor, sample tree, browse, run) and a sample connection in
   `src/dev/mock/state.ts`, so the UI can be reviewed without a server.
8. **Contract fixtures**: if a contract type changed, regenerate the JSON samples with
   `FOSTA_UPDATE_FIXTURES=1 cargo test contract::fixtures`; vitest checks them against `src/core/contract.ts`.
9. **Docs**: list the engine in both READMEs.
