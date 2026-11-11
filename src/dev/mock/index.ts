/**
 * Browser-only stand-in for the Tauri backend, used by `npm run dev` when the page is opened in a plain browser (no
 * `__TAURI_INTERNALS__`). Never bundled into the desktop build.
 *
 * - `index.ts` (this file): the command router, connections and sessions, diagnostics, tasks, the old commands.
 * - `state.ts`: config, workspaces, history. `gateway.ts`: the safety rules. `ai.ts`: the assistant.
 * - `connections.ts`: saving and deleting profiles, saved passwords and the test-connection secret binding, connection
 *   string parsing. Test connection with the password "wrong" fails with AUTH_FAILED; SQL Server without "Accept
 *   invalid certificates" fails like a self-signed server; SSH settings are accepted as if the tunnel were up.
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
 *
 * MCP server (`mcp.ts`): Settings → MCP server works; `__fostaMock.mcpApproval()` (or `"typed"`, or `?mcpApproval` in the
 * address) shows an incoming `run_query` approval from Claude Code.
 *
 * Updates (`update.ts`): `check_update` always finds 0.1.1; `__fostaMock.setInstallKind("portable")` or `"web"` changes the
 * update dialog (or `?installKind=web` in the address), `?whatsNew` shows the "What's new" note.
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
  SecretSlot,
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
  EmbeddingSettings,
  HistoryEntry,
  HistoryQuery,
  InstallKind,
  KeyTarget,
  StatementCategory,
  TaskDef,
  Workspace,
} from "../../core/types";
import { PRODUCTION_ICON, presetSettings, relaxations, safetyOf, unmarksLooserThanReadOnly, withoutConfirmation } from "../../features/safety/safety";
import {
  chatRuns,
  deleteAiConnection,
  draftKey,
  fetchModels,
  probeChat,
  saveAiConnection,
  saveEmbeddingSettings,
  savedApiKeys,
  setAiActive,
  streamChat,
} from "./ai";
import { checkTestPassword, deleteConnection, parseConnectionString, profiles, saveConnection } from "./connections";
import { mockError, NOT_CONNECTED, SESSION_LOST } from "./errors";
import { gate, refuseOperation, statementInfo } from "./gateway";
import { mockCheckUpdate, mockInstallKind, mockRestart, mockUpdaterCheck, mockUpdaterDownload, mockWhatsNew, setInstallKind } from "./update";
import { mockMcpAnswer, mockMcpStatus, mockMcpToken, mockRotateMcpToken, mockSetMcpSettings, simulateMcpApproval } from "./mcp";
import { VECTOR_DIMS } from "./shop";
import { state } from "./state";
import {
  acquire,
  connectionSaved,
  deleteTask,
  release,
  restartTask,
  saveTask,
  startAutoCommands,
  startTask,
  stopTask,
  taskLogs,
  taskStatuses,
  withProbe,
} from "./commands";
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
/** Connections the "backend" dropped (`__fostaMock.restartBackend`): calls fail with `NOT_CONNECTED` until `connect`. */
const dropped = new Set<string>();
const recentErrors: Diagnostics["recentErrors"] = [];

// --- Connections ----------------------------------------------------------------------------------------------------------

function savedProfile(id: string): ConnectionProfile {
  const profile = profiles().find((item) => item.id === id);
  if (!profile) throw mockError("ERROR", `Connection '${id}' no longer exists`);
  return profile;
}

/** Private 10.x addresses stand for servers the mock cannot reach. */
function unreachable(profile: ConnectionProfile): boolean {
  return /^10\./.test(profile.host.trim());
}

function knownEngine(id: string): EngineMock {
  const engine = engineById.get(id);
  if (!engine) throw mockError("UNKNOWN_ENGINE", `This build of Fosta Studio does not know the engine '${id}'.`, { engine: id });
  return engine;
}

function open(profile: ConnectionProfile): Connection {
  const engine = knownEngine(profile.driver);
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
      hint: "Check that the server is running and reachable from this machine: VPN, firewall, or a port-forward command.",
      cause: [`connect to ${profile.host}:${profile.port}`, "operation timed out after 10 s"],
    });
  return { profile, engine, info: engine.connectInfo(profile) };
}

/** The open connection; the mock connects lazily (like a reconnect) when the UI skipped `connect`. */
function connection(connectionId: string): Connection {
  const existing = connections.get(connectionId);
  if (existing) return existing;
  const profile = profiles().find((item) => item.id === connectionId);
  if (!profile || dropped.has(connectionId)) throw mockError("NOT_CONNECTED", NOT_CONNECTED);
  const opened = open(profile);
  connections.set(connectionId, opened);
  return opened;
}

function need<K extends keyof EngineMock>(engine: EngineMock, member: K, capability: string): NonNullable<EngineMock[K]> {
  const value = engine[member];
  if (!value) throw mockError("UNSUPPORTED_CAPABILITY", `${engine.descriptor.label} does not offer ${capability}.`, { engine: engine.descriptor.id });
  return value as NonNullable<EngineMock[K]>;
}

// --- Open tabs (kept in localStorage, so a reload of the preview restores them like a restart of the app) ------------------

const MOCK_TABS_KEY = "fosta-mock.openTabs";

function readMockTabs(): { workspaces: Record<string, unknown>; revisions: Record<string, number> } {
  try {
    const parsed = JSON.parse(localStorage.getItem(MOCK_TABS_KEY) ?? "{}") as { workspaces?: unknown; revisions?: unknown } | null;
    const record = (value: unknown) => (typeof value === "object" && value !== null ? value : {});
    return { workspaces: record(parsed?.workspaces) as Record<string, unknown>, revisions: record(parsed?.revisions) as Record<string, number> };
  } catch {
    return { workspaces: {}, revisions: {} };
  }
}

function writeMockTabs(stored: { workspaces: Record<string, unknown>; revisions: Record<string, number> }): void {
  try {
    localStorage.setItem(MOCK_TABS_KEY, JSON.stringify(stored));
  } catch {
    // Storage full or unavailable: the preview then starts without tabs.
  }
}

// --- Sessions and streaming -----------------------------------------------------------------------------------------------

function session(sessionId: string): MockSession {
  const found = sessions.get(sessionId);
  if (!found) throw mockError("SESSION_LOST", SESSION_LOST);
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

async function run(
  sessionId: string,
  request: RunRequest,
  confirmation: Confirmation | null,
  channel: Channel<StreamEvent> | null,
  maxRows: number | null = null,
): Promise<RunSummary> {
  const current = session(sessionId);
  const { engine } = connection(current.connectionId);
  const runner = need(engine, "run", "queries");
  gate(state.config, current.connectionId, "queryTab", classify(engine, request.text), confirmation);
  const channelSender = channel ? channelEmitter(channel) : null;
  const sender = channelSender && maxRows ? rowCap(channelSender, maxRows) : channelSender;
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

/** The `vector_*` commands, answered by the engine's `vector.*` operations like the backend does. */
function vectorCommand(connectionId: string, command: string, payload: Record<string, unknown>): unknown {
  const { engine } = connection(connectionId);
  switch (command) {
    case "vector_info":
      return engine.invoke?.("vector.info", payload) ?? { supported: false, columns: [], indexes: [], metrics: [] };
    case "fetch_vector":
      return engine.invoke?.("vector.fetch", payload) ?? [];
    case "vector_health":
      return engine.invoke?.("vector.health", payload) ?? [];
    case "index_recall":
      return engine.invoke?.("vector.recall", payload);
    default:
      return engine.invoke?.("vector.search", payload.request);
  }
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

/** Like the backend's `max_rows`: keeps the first `limit` rows of each result set and marks it truncated. */
function rowCap(sender: { emit(event: StreamEvent): void; end(): void }, limit: number) {
  const seen = new Map<number, number>();
  const cut = new Set<number>();
  return {
    end: () => sender.end(),
    emit(event: StreamEvent) {
      if (event.event === "rows") {
        const already = seen.get(event.index) ?? 0;
        const room = Math.max(0, limit - already);
        if (event.rows.length > room) cut.add(event.index);
        seen.set(event.index, already + Math.min(room, event.rows.length));
        if (room > 0) sender.emit({ ...event, rows: event.rows.slice(0, room) });
        return;
      }
      if (event.event === "done" && seen.has(event.index)) {
        sender.emit({ ...event, rows: seen.get(event.index) ?? 0, truncated: event.truncated || cut.has(event.index) });
        return;
      }
      sender.emit(event);
    },
  };
}

function skipConfirmations(connectionId: string, categories: StatementCategory[]) {
  const workspace = state.config.workspaces.find((item) => item.connections.some((profile) => profile.id === connectionId));
  if (!workspace) throw mockError("NOT_FOUND", `Connection '${connectionId}' no longer exists`);
  if (workspace.icon === PRODUCTION_ICON) throw mockError("BLOCKED", "A Production workspace always asks. Change its rules in the workspace settings instead.");
  const after = withoutConfirmation(safetyOf(workspace), categories);
  if (after)
    state.config = { ...state.config, workspaces: state.config.workspaces.map((item) => (item.id === workspace.id ? { ...item, safety: after } : item)) };
  return state.config;
}

function saveWorkspace(meta: Workspace, relaxConfirmation: unknown) {
  const config = state.config;
  const existing = config.workspaces.find((item) => item.id === meta.id);
  const before = existing ? safetyOf(existing) : presetSettings(meta.icon === PRODUCTION_ICON ? "readOnly" : "guarded");
  const after = meta.safety ?? before;
  const production = existing?.icon === PRODUCTION_ICON || meta.icon === PRODUCTION_ICON;
  const name = existing?.name ?? meta.name;
  const relaxing = relaxations(before, after).length > 0 || unmarksLooserThanReadOnly(existing?.icon, meta.icon, after);
  if (production && relaxing && String(relaxConfirmation ?? "").trim() !== name.trim()) {
    throw mockError("RELAX_CONFIRMATION_REQUIRED", `Relaxing the rules of the Production workspace needs its name typed: "${name}".`, { expected: name });
  }
  state.config = existing
    ? {
        ...config,
        workspaces: config.workspaces.map((item) =>
          item.id === meta.id
            ? {
                ...item,
                name: meta.name,
                color: meta.color,
                icon: meta.icon,
                safety: after,
                assistantQueries: meta.assistantQueries ?? item.assistantQueries,
                mcp: meta.mcp ?? item.mcp,
              }
            : item,
        ),
      }
    : {
        ...config,
        workspaces: [
          ...config.workspaces,
          { ...meta, safety: after, assistantQueries: meta.assistantQueries ?? "default", mcp: meta.mcp ?? "off", connections: [], tasks: [] },
        ],
      };
  return state.config;
}

function diagnostics(): Diagnostics {
  return {
    appVersion: "0.2.0-dev",
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
      startAutoCommands();
      return state.config;
    case "save_workspace":
      return saveWorkspace(payload.workspace as Workspace, payload.relaxConfirmation);
    case "skip_confirmations":
      return skipConfirmations(String(payload.connectionId), payload.categories as StatementCategory[]);
    case "delete_workspace":
      for (const workspace of state.config.workspaces.filter((item) => item.id === payload.id)) {
        for (const profile of workspace.connections) release(profile.id);
        for (const task of workspace.tasks) stopTask(task.id);
      }
      state.config = { ...state.config, workspaces: state.config.workspaces.filter((item) => item.id !== payload.id) };
      if (state.config.activeWorkspace === payload.id) state.config.activeWorkspace = state.config.workspaces[0].id;
      return state.config;
    case "save_connection":
      connectionSaved(payload.profile as ConnectionProfile);
      return saveConnection(
        payload.profile as ConnectionProfile,
        (payload.password as string | null) ?? null,
        (payload.workspaceId as string | null) ?? null,
        (payload.secrets as Partial<Record<SecretSlot, string>> | null) ?? null,
        (payload.relaxConfirmation as string | null) ?? null,
      );
    case "delete_connection":
      connections.delete(str("id"));
      release(str("id"));
      return deleteConnection(str("id"));
    case "save_task":
      return saveTask(payload.task as TaskDef, (payload.workspaceId as string | null) ?? null);
    case "delete_task":
      return deleteTask(str("id"));
    case "save_embedding_settings":
      return saveEmbeddingSettings(
        (payload.settings as EmbeddingSettings | null) ?? null,
        (payload.apiKey as string | null) ?? null,
        (payload.keyRef as string | null) ?? null,
      );
    case "saved_api_keys":
      return savedApiKeys(payload.target as KeyTarget);
    case "capabilities":
      return { edition: "desktop", tasks: true, revealFile: true, browserDownloads: false, secretStore: "Windows Credential Manager", inContainer: false };
    case "task_statuses":
      return taskStatuses();
    case "task_logs":
      return taskLogs(str("id"));
    case "start_task":
      return startTask(str("id"));
    case "stop_task":
      return stopTask(str("id"));
    case "restart_task":
      return restartTask(str("id"));
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
    case "load_open_tabs":
      return readMockTabs();
    case "save_open_tabs": {
      // Like the backend: a save based on another revision writes nothing and gets the stored tabs back.
      const stored = readMockTabs();
      const id = str("workspaceId");
      const current = stored.revisions[id] ?? 0;
      if (typeof payload.revision === "number" && payload.revision !== current) {
        return { saved: false, revision: current, current: stored.workspaces[id] ?? null };
      }
      const known = new Set(state.config.workspaces.map((workspace) => workspace.id));
      for (const key of Object.keys(stored.workspaces)) if (!known.has(key)) delete stored.workspaces[key];
      for (const key of Object.keys(stored.revisions)) if (!known.has(key)) delete stored.revisions[key];
      let revision = 0;
      if (payload.tabs === null || payload.tabs === undefined || !known.has(id)) {
        delete stored.workspaces[id];
        delete stored.revisions[id];
      } else {
        revision = current + 1;
        stored.workspaces[id] = payload.tabs;
        stored.revisions[id] = revision;
      }
      writeMockTabs(stored);
      return { saved: true, revision, current: null };
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
    case "set_assistant_read_queries":
      state.config = { ...state.config, assistantReadQueries: Boolean(payload.enabled) };
      return state.config;
    case "diagnostics":
      return diagnostics();

    // MCP server (mcp.ts)
    case "mcp_status":
      return mockMcpStatus();
    case "set_mcp_settings":
      return mockSetMcpSettings(Boolean(payload.enabled), (payload.port as number | null | undefined) ?? null);
    case "mcp_token":
      return mockMcpToken();
    case "rotate_mcp_token":
      return mockRotateMcpToken();
    case "mcp_answer":
      return mockMcpAnswer(String(payload.id), Boolean(payload.approve), (payload.typed as string | null | undefined) ?? null);
    // Updates (update.ts)
    case "check_update":
      return mockCheckUpdate();
    case "install_kind":
      return mockInstallKind();
    case "whats_new":
      return mockWhatsNew();
    case "set_update_check":
      state.config = {
        ...state.config,
        updateCheck: { lastChecked: null, latestVersion: null, lastRunVersion: null, ...state.config.updateCheck, enabled: Boolean(payload.enabled) },
      };
      return state.config;
    case "open_release_page":
      window.open(str("url"), "_blank", "noopener");
      return null;
    case "plugin:updater|check":
      return mockUpdaterCheck();
    case "plugin:updater|download": {
      const emitter = channelEmitter(payload.onEvent as Channel<unknown>);
      return mockUpdaterDownload((event) => emitter.emit(event)).finally(() => emitter.end());
    }
    case "plugin:updater|install":
      return mockRestart(payload.restartAfterInstall !== false);
    case "plugin:process|restart":
      return mockRestart();
    case "plugin:resources|close":
      return null;

    // Assistant
    case "save_ai_connection":
      return saveAiConnection(payload.connection as AiConnection, (payload.apiKey as string | null) ?? null, (payload.keyRef as string | null) ?? null);
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
      return parseConnectionString(knownEngine(str("engine")).descriptor, str("text"));
    case "test_connection": {
      const profile = payload.profile as ConnectionProfile;
      knownEngine(profile.driver);
      checkTestPassword(profile, (payload.password as string | null) ?? null);
      return withProbe(profile, () => open(profile).info);
    }
    case "connect": {
      const profile = savedProfile(str("id"));
      dropped.delete(profile.id);
      const connectNow = () => {
        try {
          const opened = open(profile);
          connections.set(opened.profile.id, opened);
          return opened.info;
        } catch (error) {
          if (!connections.has(profile.id)) release(profile.id);
          throw error;
        }
      };
      return profile.commandId ? acquire(profile.id, profile).then(connectNow) : (release(profile.id), connectNow());
    }
    case "disconnect":
      connections.delete(str("id"));
      release(str("id"));
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
      // The mock engines read from the start only: Last falls back to an exact count and an offset.
      if (request.reverse) throw mockError("UNSUPPORTED_CAPABILITY", "The mock engines do not read from the end.", { engine: engine.descriptor.id });
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
      const database = (payload.database as string | null) ?? null;
      return engine.completion?.(database) ?? { databases: [], schemas: [], tables: [] };
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
      // The backend formatter is Rust (src-tauri/src/lang); the browser mock leaves the text unchanged.
      return str("text");

    // Contract: sessions
    case "open_session":
      return openSession(str("connectionId"), str("sessionId"), (payload.options as SessionOptions | null) ?? null);
    case "close_session":
      sessions.get(str("sessionId"))?.running?.abort();
      sessions.delete(str("sessionId"));
      return null;
    case "run":
      return run(
        str("sessionId"),
        payload.request as RunRequest,
        confirmation,
        (payload.onEvent as Channel<StreamEvent> | undefined) ?? null,
        (payload.maxRows as number | null | undefined) ?? null,
      );
    case "classify_run": {
      const current = session(str("sessionId"));
      return classify(connection(current.connectionId).engine, str("text")).map((statement) => statement.category);
    }
    case "export_prepare":
    case "export_rows":
      // The mock has no backend to stream from: the dialog builds the file in the page instead.
      throw mockError("UNSUPPORTED_CAPABILITY", "The mock engines do not stream exports.", {});
    case "cancel_request":
      return null;
    case "cancel_run":
      sessions.get(str("sessionId"))?.running?.abort();
      return null;
    case "transaction":
      return transaction(str("sessionId"), payload.op as TransactionOp);

    // Vector tooling
    case "vector_info":
    case "fetch_vector":
    case "vector_search":
    case "vector_health":
    case "index_recall": {
      const request = payload.request as { connectionId?: string } | undefined;
      return vectorCommand(str("connectionId") || request?.connectionId || "", command, payload);
    }
    case "test_embedding":
      return VECTOR_DIMS;

    // Mock-only
    case "mock_touch_row":
      return touchRow(str("connectionId"), str("table"), payload.key, Boolean(payload.remove), (payload.column as string | undefined) ?? undefined);
    default: {
      const handler = engines.find((engine) => engine.commands?.[command])?.commands?.[command];
      if (!handler) throw mockError("ERROR", `devMock: no handler for ${command}`);
      const channel = payload.onEvent as Channel<unknown> | undefined;
      const emitter = channel ? channelEmitter(channel) : null;
      const result = handler(payload, (event) => emitter?.emit(event));
      if (!(result instanceof Promise)) return result;
      return result.finally(() => emitter?.end());
    }
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
      setInstallKind(kind: InstallKind): void;
      mcpApproval(level?: "confirm" | "typed"): string;
      /** Drops every session and open connection, like a restart of the backend (lost sessions, NOT_CONNECTED). */
      restartBackend(): void;
    };
  }
}

export function installDevMock(): void {
  mockWindows("main");
  // Events (`listen` / `emit`) are delivered by the mock itself, so mock commands can report their status.
  mockIPC(
    (command, payload) => {
      try {
        const result = handle(command, payload as Record<string, unknown>);
        if (result instanceof Promise) return result.catch((error: unknown) => (record(command, error), Promise.reject(error)));
        return result;
      } catch (error) {
        record(command, error);
        throw error;
      }
    },
    { shouldMockEvents: true },
  );
  window.__fostaMock = {
    touchRow: (connectionId, table, key, column) => touchRow(connectionId, table, key, false, column),
    deleteRow: (connectionId, table, key) => touchRow(connectionId, table, key, true),
    connections: () => [...connections.keys()],
    setTreePageSize: (size) => {
      treePageSize = size && size > 0 ? size : null;
    },
    setInstallKind,
    mcpApproval: simulateMcpApproval,
    restartBackend: () => {
      // Like a restart of the app's backend or the web server: every session and open connection is gone.
      for (const id of connections.keys()) dropped.add(id);
      connections.clear();
      sessions.clear();
    },
  };
}
