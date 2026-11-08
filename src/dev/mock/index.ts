/**
 * Browser-only stand-in for the Tauri backend, used by `npm run dev` when the page is opened in a plain browser (no
 * `__TAURI_INTERNALS__`). Never bundled into the desktop build.
 *
 * - `index.ts` (this file): the command router, connections and sessions, diagnostics, tasks, the old commands.
 * - `state.ts`: config, workspaces, history. `gateway.ts`: the safety rules. `ai.ts`: the assistant.
 * - `engines/<id>.ts`: one module per engine (collected with `import.meta.glob`), exporting `descriptor`, `order` and, for
 *   the ready ones, `connectInfo`, `tree`, `describe`, `browse`, `apply`, `run`, `invoke`, … over `catalog.ts`.
 *
 * Conflict demo (optimistic concurrency): open a table, edit a cell, then in the browser console run
 * `__fostaMock.touchRow("c-local", "products", 1)` (every text and number column of row id 1 changes; pass a column name
 * as the 4th argument to change only that one) or `__fostaMock.deleteRow("c-local", "products", 1)`, then save:
 * `apply_edits` fails with `CONFLICT`. Also reachable as the mock-only command
 * `mock_touch_row { connectionId, table, key, remove?, column? }`. Tables are named as in the tree (`dbo.Customers`).
 *
 * Tree paging demo: `__fostaMock.setTreePageSize(2)` caps every `tree_children` page at 2 nodes, so the tree shows its
 * "Load more" row; `setTreePageSize(null)` restores the requested limit.
 */
import type { Channel } from "@tauri-apps/api/core";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type {
  BrowseRequest,
  ConnectInfo,
  ObjectRef,
  RowChangeSet,
  RunRequest,
  RunSummary,
  SessionInfo,
  SessionOptions,
  StreamEvent,
  TransactionOp,
  TransactionState,
  TreeChildren,
  TreeRequest,
} from "../../core/contract";
import { errorCode, errorText } from "../../core/errors";
import { splitStatements } from "../../core/language";
import type {
  AiConnection,
  AiSettings,
  Caller,
  ChatEvent,
  ChatRequest,
  Confirmation,
  ConnectionProfile,
  Diagnostics,
  HistoryEntry,
  HistoryQuery,
  BrowseRequest as OldBrowseRequest,
  RowChange as OldRowChange,
  Workspace,
} from "../../core/types";
import { PRODUCTION_ICON, presetSettings, relaxations, safetyOf } from "../../features/safety/safety";
import { chatRuns, deleteAiConnection, draftKey, fetchModels, probeChat, saveAiConnection, setAiActive, streamChat } from "./ai";
import { mockError, NOT_CONNECTED } from "./errors";
import { gate, refuseOperation, statementInfo } from "./gateway";
import * as legacy from "./legacy";
import { VECTOR_DIMS } from "./shop";
import { state } from "./state";
import type { EngineMock, MockSession } from "./types";

const modules = import.meta.glob<EngineMock>("./engines/*.ts", { eager: true });
const engines = Object.values(modules).sort((a, b) => a.order - b.order);
const engineById = new Map(engines.map((engine) => [engine.descriptor.id, engine]));

interface Connection {
  profile: ConnectionProfile;
  engine: EngineMock;
  info: ConnectInfo;
}

const connections = new Map<string, Connection>();
const sessions = new Map<string, MockSession>();
const recentErrors: Diagnostics["recentErrors"] = [];

// --- Connections ----------------------------------------------------------------------------------------------------------

function profiles(): ConnectionProfile[] {
  return state.config.workspaces.flatMap((workspace) => workspace.connections);
}

function savedProfile(id: string): ConnectionProfile {
  const profile = profiles().find((item) => item.id === id);
  if (!profile) throw mockError("ERROR", `Connection '${id}' no longer exists`);
  return profile;
}

/** Private 10.x addresses stand for servers the mock cannot reach. */
function unreachable(profile: ConnectionProfile): boolean {
  return /^10\./.test(profile.host.trim());
}

function open(profile: ConnectionProfile): Connection {
  const engine = engineById.get(profile.driver);
  if (!engine) throw mockError("UNKNOWN_ENGINE", `This build of Fosta Studio does not know the engine '${profile.driver}'.`, { engine: profile.driver });
  if (engine.descriptor.status === "planned" || !engine.connectInfo)
    throw mockError("ENGINE_NOT_WIRED", `${engine.descriptor.label} support is planned but not available in this build of Fosta Studio yet.`, {
      engine: engine.descriptor.id,
    });
  if (unreachable(profile))
    throw mockError("CONNECTION_ERROR", `Can't connect to server on '${profile.host}' (110 "Connection timed out")`, {
      engine: engine.descriptor.id,
      vendorCode: 2002,
      kind: "connect",
      retryable: true,
      hint: "Check that the server is running and reachable from this machine: VPN, firewall, or a port-forward startup task.",
      cause: [`connect to ${profile.host}:${profile.port}`, "operation timed out after 10 s"],
    });
  return { profile, engine, info: engine.connectInfo(profile) };
}

/** The open connection; the mock connects lazily (like a reconnect) when the UI skipped `connect`. */
function connection(connectionId: string): Connection {
  const existing = connections.get(connectionId);
  if (existing) return existing;
  const profile = profiles().find((item) => item.id === connectionId);
  if (!profile) throw mockError("ERROR", NOT_CONNECTED);
  const opened = open(profile);
  connections.set(connectionId, opened);
  return opened;
}

function need<K extends keyof EngineMock>(engine: EngineMock, member: K, capability: string): NonNullable<EngineMock[K]> {
  const value = engine[member];
  if (!value) throw mockError("UNSUPPORTED_CAPABILITY", `${engine.descriptor.label} does not offer ${capability}.`, { engine: engine.descriptor.id });
  return value as NonNullable<EngineMock[K]>;
}

// --- Sessions and streaming -----------------------------------------------------------------------------------------------

function session(sessionId: string): MockSession {
  const found = sessions.get(sessionId);
  if (!found) throw mockError("ERROR", "This query tab lost its session. Close it and open a new query.");
  return found;
}

function openSession(connectionId: string, sessionId: string, options: SessionOptions | null): SessionInfo {
  const { info } = connection(connectionId);
  const language = info.languages.find((item) => item.id === options?.language) ?? info.languages.find((item) => item.default) ?? info.languages[0];
  const created: MockSession = {
    sessionId,
    connectionId,
    language: language?.id ?? "sql",
    database: options?.database ?? null,
    transaction: { supported: info.capabilities.includes("transactions"), open: false, autoCommit: true, failed: false },
    running: null,
  };
  sessions.set(sessionId, created);
  return { sessionId, language: created.language, transaction: created.transaction, cancellable: info.capabilities.includes("cancel") };
}

function transaction(sessionId: string, op: TransactionOp): TransactionState {
  const current = session(sessionId);
  const state = current.transaction;
  if (!state.supported && op !== "status") throw mockError("UNSUPPORTED_CAPABILITY", "This connection does not support transactions.");
  if (op === "begin" && state.open) throw mockError("ERROR", "A transaction is already open in this tab. Commit or roll it back before starting a new one.");
  const next = { ...state };
  if (op === "begin") next.open = true;
  if (op === "commit" || op === "rollback" || op === "autoCommitOn") next.open = false;
  if (op === "autoCommitOn") next.autoCommit = true;
  if (op === "autoCommitOff") next.autoCommit = false;
  current.transaction = next;
  return next;
}

type Internals = { runCallback(id: number, data: unknown): void };

/** Sends events through a Tauri channel the way the backend does (ordered by index, then an end marker). */
function channelEmitter<T>(channel: Channel<T>): { emit(message: T): void; end(): void } {
  const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
  let index = 0;
  return {
    emit: (message) => internals.runCallback(channel.id, { index: index++, message }),
    end: () => internals.runCallback(channel.id, { index, end: true }),
  };
}

function classify(engine: EngineMock, text: string) {
  return engine.classify ? engine.classify(text) : splitStatements(text).map(({ from, to }) => statementInfo("unknown", "STATEMENT", [], text.slice(from, to)));
}

async function run(sessionId: string, request: RunRequest, confirmation: Confirmation | null, channel: Channel<StreamEvent> | null): Promise<RunSummary> {
  const current = session(sessionId);
  const { engine } = connection(current.connectionId);
  const runner = need(engine, "run", "queries");
  gate(state.config, current.connectionId, "queryTab", classify(engine, request.text), confirmation);
  const sender = channel ? channelEmitter(channel) : null;
  const controller = new AbortController();
  current.running = controller;
  const started = performance.now();
  try {
    const statements = await runner(request, (event) => sender?.emit(event), { session: current, signal: controller.signal });
    return { statements, elapsedMs: Math.round(performance.now() - started), stopped: false, transaction: current.transaction };
  } finally {
    current.running = null;
    sender?.end();
  }
}

async function runSqlLegacy(sessionId: string, sql: string, database: string | null, confirmation: Confirmation | null) {
  const current = session(sessionId);
  const { engine } = connection(current.connectionId);
  gate(state.config, current.connectionId, "queryTab", classify(engine, sql), confirmation);
  const controller = new AbortController();
  current.running = controller;
  try {
    return await legacy.runSql(engine, sql, database, { session: current, signal: controller.signal });
  } finally {
    current.running = null;
  }
}

// --- Contract operations --------------------------------------------------------------------------------------------------

/** Mock-only cap on `tree_children` pages (`__fostaMock.setTreePageSize`), to show the tree's "Load more" row. */
let treePageSize: number | null = null;

function treeChildren(connectionId: string, request: TreeRequest): TreeChildren {
  const { engine } = connection(connectionId);
  const filter = request.filter?.trim().toLowerCase();
  const all = need(engine, "tree", "a tree")(request.parent).filter((node) => !filter || node.label.toLowerCase().includes(filter));
  const start = Number(request.cursor ?? 0) || 0;
  const limit = Math.min(request.limit ?? 500, treePageSize ?? Number.MAX_SAFE_INTEGER);
  const nodes = all.slice(start, start + limit);
  return { nodes, nextCursor: start + limit < all.length ? String(start + limit) : null, totalHint: all.length };
}

/** The gateway checks a raw filter is one condition (no `;`, no second statement). */
function checkFilter(request: BrowseRequest): void {
  if (request.filter.kind !== "raw" || request.filter.language !== "sql") return;
  if (splitStatements(request.filter.text).length > 1)
    throw mockError("INVALID_FILTER", "The filter must be a single condition; it cannot contain ';' or a second statement.", {
      hint: "Write only what follows WHERE, e.g. status = 'ACTIVE' AND stock > 0",
    });
}

function applyEdits(connectionId: string, changes: RowChangeSet, caller: Caller | null, confirmation: Confirmation | null) {
  const { engine } = connection(connectionId);
  const table = changes.object.schema ? `${changes.object.schema}.${changes.object.name}` : changes.object.name;
  gate(
    state.config,
    connectionId,
    caller ?? "gridSave",
    [statementInfo("write", "CHANGES", [table], `${changes.changes.length} change(s) to ${table}, each matched by its key and the values read`)],
    confirmation,
  );
  return need(engine, "apply", "row edits")(changes);
}

function invokeCapability(connectionId: string, name: string, args: unknown) {
  const { engine } = connection(connectionId);
  const result = engine.invoke?.(name, args);
  if (result === undefined) refuseOperation(state.config, connectionId, name);
  return result;
}

/** Mock-only: another client changes (or deletes) a row. */
function touchRow(connectionId: string, table: string, key: unknown, remove = false, column?: string): string {
  const { engine } = connection(connectionId);
  const message = need(engine, "touchRow", "the conflict demo")(table, key, remove, column);
  console.info(`[dev mock] ${message}`);
  return message;
}

/** The legacy vector commands, answered by MariaDB's `vector.*` operations where they have an equivalent. */
function legacyVector(connectionId: string, command: string, payload: Record<string, unknown>): unknown {
  const { engine } = connection(connectionId);
  switch (command) {
    case "vector_info":
      return engine.invoke?.("vector.info", payload) ?? { supported: false, columns: [], indexes: [] };
    case "fetch_vector":
      return engine.invoke?.("vector.fetch", payload) ?? [];
    case "vector_health":
      return [
        { label: "Dimensions", status: "ok", value: String(VECTOR_DIMS), detail: "Every sampled row has 4 values" },
        { label: "NULL vectors", status: "ok", value: "0", detail: "No row is missing a vector" },
        { label: "Zero vectors", status: "warn", value: "3", detail: "Cosine distance is undefined for all-zero vectors" },
        { label: "Vector index", status: "info", value: "idx_embedding", detail: "cosine, M=6" },
      ];
    case "index_recall":
      return engine.invoke?.("vector.recall", payload);
    default: {
      const request = payload.request as { connectionId: string; database: string; table: string; k: number; filter: string | null };
      const page = legacy.browseTable(engine, { ...request, offset: 0, limit: request.k, orderBy: null, descending: false });
      const distance = { name: "distance", typeName: "double", numeric: true, binary: false, primary: false, vectorDims: null };
      const sql = "SELECT *, VEC_DISTANCE_COSINE(`embedding`, ?) AS distance FROM `products` ORDER BY distance LIMIT ?";
      return {
        result: {
          columns: [...page.columns, distance],
          rows: page.rows.map((row, index) => [...row, (0.05 + index * 0.031).toFixed(4)]),
          affectedRows: 0,
          truncated: false,
        },
        plan: {
          columns: [
            { ...distance, name: "type", typeName: "varchar", numeric: false },
            { ...distance, name: "key", typeName: "varchar", numeric: false },
          ],
          rows: [["range", "idx_embedding"]],
          affectedRows: 0,
          truncated: false,
        },
        indexUsed: "idx_embedding",
        sql,
        runnableSql: sql.replace("?", "VEC_FromText('[0.12,-0.4,0.33,0.9]')").replace("?", String(request.k)),
        elapsedMs: 4,
        queryDims: VECTOR_DIMS,
        primaryKey: page.primaryKey,
      };
    }
  }
}

/** A connection string split into profile fields (the backend's adapters parse every engine's own formats). */
function parseConnectionString(engineId: string, text: string) {
  const engine = engineById.get(engineId);
  if (!engine) throw mockError("UNKNOWN_ENGINE", `This build of Fosta Studio does not know the engine '${engineId}'.`, { engine: engineId });
  const value = text.trim();
  const base = {
    engine: engineId,
    host: "",
    port: engine.descriptor.connection.defaultPort ?? 0,
    user: "",
    database: null,
    file: null,
    url: null,
    options: {},
    tls: null,
    ssh: null,
    auth: null,
  };
  if (/^[\w+]+:\/\//.test(value)) {
    const url = new URL(value.replace(/^[\w+]+:/, "http:"));
    const scheme = /^([\w+]+):/.exec(value)?.[1] ?? "";
    const known = engine.descriptor.connection.url?.schemes ?? [];
    if (!known.includes(scheme))
      throw mockError("ERROR", `A ${engine.descriptor.label} connection string starts with ${known.map((item) => `${item}://`).join(" or ")}.`);
    const options = Object.fromEntries(url.searchParams.entries());
    return {
      spec: {
        ...base,
        host: decodeURIComponent(url.hostname),
        port: Number(url.port) || base.port,
        user: decodeURIComponent(url.username),
        database: decodeURIComponent(url.pathname.replace(/^\//, "")) || null,
        url: scheme.endsWith("+srv") ? value.replace(/:[^:@/]*@/, "@") : null,
        options,
      },
      password: url.password ? decodeURIComponent(url.password) : null,
    };
  }
  if (/server\s*=/i.test(value)) {
    const parts = Object.fromEntries(
      value
        .split(";")
        .map((part) => part.split("="))
        .filter((pair) => pair.length === 2)
        .map(([key, item]) => [key.trim().toLowerCase(), item.trim()]),
    );
    const [host, port] = String(parts.server ?? parts["data source"] ?? "").split(",");
    return {
      spec: {
        ...base,
        host,
        port: Number(port) || base.port,
        user: parts["user id"] ?? parts.uid ?? "",
        database: parts.database ?? parts["initial catalog"] ?? null,
      },
      password: parts.password ?? parts.pwd ?? null,
    };
  }
  throw mockError(
    "ERROR",
    `Not a ${engine.descriptor.label} connection string. Example: ${engine.descriptor.connection.url?.example ?? "scheme://user@host:port/database"}`,
  );
}

// --- History and workspaces -----------------------------------------------------------------------------------------------

function appendHistory(entry: HistoryEntry): HistoryEntry | null {
  if (!state.config.queryHistory) return null;
  state.historySequence += 1;
  const stored = { ...entry, id: `mock-${Date.now().toString(16)}-${state.historySequence}` };
  state.history = [...state.history, stored].slice(-5000);
  return stored;
}

function listHistory(query: HistoryQuery): HistoryEntry[] {
  const search = query.search?.trim().toLowerCase();
  return state.history
    .filter((entry) => !query.connectionId || entry.connectionId === query.connectionId)
    .filter((entry) => query.before === undefined || entry.startedAt < query.before)
    .filter((entry) => !search || [entry.sql, entry.connectionName, entry.database ?? ""].some((text) => text.toLowerCase().includes(search)))
    .reverse()
    .sort((a, b) => b.startedAt - a.startedAt)
    .slice(0, query.limit || 200);
}

function saveWorkspace(meta: Workspace, relaxConfirmation: unknown) {
  const config = state.config;
  const existing = config.workspaces.find((item) => item.id === meta.id);
  const before = existing ? safetyOf(existing) : presetSettings(meta.icon === PRODUCTION_ICON ? "readOnly" : "guarded");
  const after = meta.safety ?? before;
  const production = existing?.icon === PRODUCTION_ICON || meta.icon === PRODUCTION_ICON;
  const name = existing?.name ?? meta.name;
  if (production && relaxations(before, after).length > 0 && String(relaxConfirmation ?? "").trim() !== name.trim()) {
    throw mockError("RELAX_CONFIRMATION_REQUIRED", `Relaxing the rules of the Production workspace needs its name typed: "${name}".`, { expected: name });
  }
  state.config = existing
    ? {
        ...config,
        workspaces: config.workspaces.map((item) =>
          item.id === meta.id ? { ...item, name: meta.name, color: meta.color, icon: meta.icon, safety: after } : item,
        ),
      }
    : { ...config, workspaces: [...config.workspaces, { ...meta, safety: after, connections: [], tasks: [] }] };
  return state.config;
}

function diagnostics(): Diagnostics {
  return {
    appVersion: "0.1.0-dev",
    edition: "desktop",
    os: "windows",
    arch: "x86_64",
    osVersion: "Browser dev mock",
    logFolder: "C:/Fosta Studio/fosta-studio-logs",
    debugLogging: Boolean(state.config.debugLogging),
    connections: [...connections.values()].map((item) => ({ engine: item.engine.descriptor.id, version: item.info.server.version, crashed: false })),
    recentErrors: recentErrors.slice(-20),
  };
}

// --- The router -----------------------------------------------------------------------------------------------------------

function handle(command: string, payload: Record<string, unknown> = {}): unknown {
  const str = (key: string) => String(payload[key] ?? "");
  const confirmation = (payload.confirmation as Confirmation | null | undefined) ?? null;
  switch (command) {
    case "plugin:event|listen":
      return 1;
    case "plugin:event|unlisten":
    case "plugin:window|show":
    case "plugin:window|set_title":
      return null;

    // Config, workspaces, tasks, files
    case "get_config":
      return state.config;
    case "set_active_workspace":
      state.config = { ...state.config, activeWorkspace: str("id") };
      return state.config;
    case "save_workspace":
      return saveWorkspace(payload.workspace as Workspace, payload.relaxConfirmation);
    case "delete_workspace":
      state.config = { ...state.config, workspaces: state.config.workspaces.filter((item) => item.id !== payload.id) };
      if (state.config.activeWorkspace === payload.id) state.config.activeWorkspace = state.config.workspaces[0].id;
      return state.config;
    case "save_connection":
    case "delete_connection":
    case "save_task":
    case "delete_task":
    case "save_embedding_settings":
      return state.config;
    case "capabilities":
      return { edition: "desktop", tasks: true, revealFile: true, browserDownloads: false, secretStore: "Windows Credential Manager", inContainer: false };
    case "task_statuses":
      return [{ id: "t-a-1", state: "running", pid: 4242, restarts: 0, message: null }];
    case "task_logs":
      return ["▶ kubectl port-forward -n shop svc/mariadb 13306:3306", "Forwarding from 127.0.0.1:13306 -> 3306", "Handling connection for 13306"];
    case "start_task":
    case "stop_task":
    case "restart_task":
    case "reveal_file":
    case "open_log_folder":
      return null;
    case "save_download":
      return ["C:", "Users", "you", "Downloads", str("name")].join("\\");
    case "history_append":
      return appendHistory(payload.entry as HistoryEntry);
    case "history_list":
      return listHistory(payload.query as HistoryQuery);
    case "history_delete": {
      const ids = new Set(payload.ids as string[]);
      const before = state.history.length;
      state.history = state.history.filter((entry) => !ids.has(entry.id));
      return before - state.history.length;
    }
    case "history_clear":
      state.history = [];
      return null;
    case "set_query_history":
      state.config = { ...state.config, queryHistory: Boolean(payload.enabled) };
      return state.config;
    case "set_debug_logging":
      state.config = { ...state.config, debugLogging: Boolean(payload.enabled) };
      return state.config;
    case "diagnostics":
      return diagnostics();

    // Assistant
    case "save_ai_connection":
      return saveAiConnection(payload.connection as AiConnection, (payload.apiKey as string | null) ?? null);
    case "delete_ai_connection":
      return deleteAiConnection(str("id"));
    case "set_ai_active":
      return setAiActive(str("connectionId"), str("model"));
    case "test_ai":
      return probeChat(payload.settings as AiSettings, draftKey(payload));
    case "list_ai_models":
      return fetchModels(payload.settings as AiSettings, draftKey(payload));
    case "ai_chat":
      return streamChat(payload.request as ChatRequest, payload.onEvent as Channel<ChatEvent>);
    case "ai_cancel":
      chatRuns.get(str("id"))?.abort();
      return null;

    // Engines and connections
    case "engine_descriptors":
      return engines.map((engine) => engine.descriptor);
    case "parse_connection_string":
      return parseConnectionString(str("engine"), str("text"));
    case "test_connection":
      return open(payload.profile as ConnectionProfile).info;
    case "connect": {
      const opened = open(savedProfile(str("id")));
      connections.set(opened.profile.id, opened);
      return opened.info;
    }
    case "disconnect":
      connections.delete(str("id"));
      for (const [id, item] of sessions) if (item.connectionId === payload.id) sessions.delete(id);
      return null;

    // Contract: objects
    case "tree_children":
      return treeChildren(str("connectionId"), payload.request as TreeRequest);
    case "describe_object": {
      const { engine } = connection(str("connectionId"));
      return need(engine, "describe", "object descriptions")(payload.object as ObjectRef);
    }
    case "browse_object": {
      const { engine } = connection(str("connectionId"));
      const request = payload.request as BrowseRequest;
      checkFilter(request);
      return need(engine, "browse", "browsing rows")(request);
    }
    case "apply_edits":
      return applyEdits(str("connectionId"), payload.changes as RowChangeSet, (payload.caller as Caller | null) ?? null, confirmation);
    case "schema_graph": {
      const { engine } = connection(str("connectionId"));
      return need(engine, "schemaGraph", "a schema diagram")(str("database"));
    }
    case "completion_schema": {
      const { engine } = connection(str("connectionId"));
      return engine.completion?.(str("database")) ?? { database: str("database"), objects: [] };
    }
    case "create_database": {
      const connectionId = str("connectionId");
      const { engine } = connection(connectionId);
      gate(state.config, connectionId, "createDatabase", [statementInfo("ddl", "CREATE DATABASE", [], `CREATE DATABASE ${str("name")}`)], confirmation);
      need(engine, "createDatabase", "creating databases")(str("name"), (payload.firstCollection as string | null) ?? null);
      return null;
    }
    case "invoke_capability":
      return invokeCapability(str("connectionId"), str("name"), payload.args ?? null);
    case "format_sql":
      // Like the backend until a formatter is wired: the text unchanged.
      return str("text");

    // Contract: sessions
    case "open_session":
      return openSession(str("connectionId"), str("sessionId"), (payload.options as SessionOptions | null) ?? null);
    case "close_session":
      sessions.get(str("sessionId"))?.running?.abort();
      sessions.delete(str("sessionId"));
      return null;
    case "run":
      return run(str("sessionId"), payload.request as RunRequest, confirmation, (payload.onEvent as Channel<StreamEvent> | undefined) ?? null);
    case "cancel_run":
    case "cancel_sql":
      sessions.get(str("sessionId"))?.running?.abort();
      return null;
    case "transaction":
    case "sql_transaction":
      return transaction(str("sessionId"), payload.op as TransactionOp);

    // Old commands (until step 16)
    case "list_databases":
      return legacy.listDatabases(connection(str("connectionId")).engine);
    case "list_tables":
      return legacy.listTables(connection(str("connectionId")).engine, str("database"));
    case "describe_table":
      return legacy.describeTable(connection(str("connectionId")).engine, str("database"), str("table"));
    case "browse_table": {
      const request = payload.request as OldBrowseRequest;
      return legacy.browseTable(connection(request.connectionId).engine, request);
    }
    case "apply_changes": {
      const connectionId = str("connectionId");
      const changes = payload.changes as OldRowChange[];
      const table = str("table");
      gate(
        state.config,
        connectionId,
        payload.caller === "import" ? "import" : "gridSave",
        [statementInfo("write", "CHANGES", [table], `${changes.length} change(s) to ${table}, each matched by its key`)],
        confirmation,
      );
      return legacy.applyChanges(connection(connectionId).engine, str("database"), table, changes);
    }
    case "table_relations":
      return legacy.tableRelations(connection(str("connectionId")).engine, str("database"), str("table"));
    case "run_sql":
      return runSqlLegacy(str("sessionId"), str("sql"), (payload.database as string | null) ?? null, confirmation);
    case "vector_info":
    case "fetch_vector":
    case "vector_search":
    case "vector_health":
    case "index_recall": {
      const request = payload.request as { connectionId?: string } | undefined;
      return legacyVector(str("connectionId") || request?.connectionId || "", command, payload);
    }
    case "test_embedding":
      return VECTOR_DIMS;

    // Mock-only
    case "mock_touch_row":
      return touchRow(str("connectionId"), str("table"), payload.key, Boolean(payload.remove), (payload.column as string | undefined) ?? undefined);
    default:
      throw mockError("ERROR", `devMock: no handler for ${command}`);
  }
}

/** Errors the user did not just answer (confirmations, passwords) go to `diagnostics().recentErrors`, like the app log. */
function record(command: string, error: unknown): void {
  const code = errorCode(error);
  if (code === "CONFIRMATION_REQUIRED" || code === "PASSWORD_REQUIRED") return;
  recentErrors.push({ time: new Date().toISOString(), level: "WARN", target: `command::${command}`, message: errorText(error), code });
  if (recentErrors.length > 50) recentErrors.shift();
}

declare global {
  interface Window {
    /** Dev mock helpers for the browser console (see the comment at the top of `src/dev/mock/index.ts`). */
    __fostaMock?: {
      touchRow(connectionId: string, table: string, key?: unknown, column?: string): string;
      deleteRow(connectionId: string, table: string, key?: unknown): string;
      connections(): string[];
      setTreePageSize(size: number | null): void;
    };
  }
}

export function installDevMock(): void {
  mockWindows("main");
  mockIPC((command, payload) => {
    try {
      const result = handle(command, payload as Record<string, unknown>);
      if (result instanceof Promise) return result.catch((error: unknown) => (record(command, error), Promise.reject(error)));
      return result;
    } catch (error) {
      record(command, error);
      throw error;
    }
  });
  window.__fostaMock = {
    touchRow: (connectionId, table, key, column) => touchRow(connectionId, table, key, false, column),
    deleteRow: (connectionId, table, key) => touchRow(connectionId, table, key, true),
    connections: () => [...connections.keys()],
    setTreePageSize: (size) => {
      treePageSize = size && size > 0 ? size : null;
    },
  };
}
