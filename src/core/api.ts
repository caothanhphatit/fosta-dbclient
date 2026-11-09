import type { CompletionSchema, FormatOptions } from "../lang/types";
import type {
  ApplyResult,
  BrowseRequest as ObjectBrowseRequest,
  ConnectInfo,
  ConnectionSpec,
  EngineDescriptor,
  ObjectDescription,
  ObjectRef,
  Page,
  RowChangeSet,
  RunRequest,
  RunSummary,
  SecretSlot,
  SessionInfo,
  SessionOptions,
  SqlDialectId,
  StreamEvent,
  TransactionOp,
  TransactionState,
  TreeChildren,
  TreeRequest,
} from "./contract";
import { errorCode as codeOf, errorText as textOf, NOT_CONNECTED } from "./errors";
export { NOT_CONNECTED, SESSION_LOST } from "./errors";
import { downloadInBrowser, invoke as transportInvoke, invokeStreaming, isTauri, listen, type InvokeOptions, type UnlistenFn } from "./transport";
import type {
  AiConnection,
  AiSettings,
  AppConfig,
  Capabilities,
  Caller,
  ChatEvent,
  ChatRequest,
  Confirmation,
  ConnectionProfile,
  Diagnostics,
  EmbeddingSettings,
  HealthCheck,
  HistoryEntry,
  HistoryQuery,
  InstallKind,
  KeyCell,
  RecallReport,
  SchemaGraph,
  StatementCategory,
  TaskDef,
  TaskLogEvent,
  TaskStatus,
  UpdateInfo,
  VectorInfo,
  VectorSearchRequest,
  VectorSearchResult,
  WorkspaceMeta,
} from "./types";

export const PASSWORD_REQUIRED = "PASSWORD_REQUIRED";

const lostListeners = new Set<(connectionId: string) => void>();

/** Calls `listener` when a call on a connection fails with `NOT_CONNECTED`, so the tree stops showing it as connected. */
export function onConnectionLost(listener: (connectionId: string) => void): () => void {
  lostListeners.add(listener);
  return () => lostListeners.delete(listener);
}

/** Reports a `NOT_CONNECTED` failure of a call on `connectionId` (the query tab reports its own, from a session call). */
export function reportConnectionLost(connectionId: string): void {
  for (const listener of lostListeners) listener(connectionId);
}

/** Every command goes through here: a `NOT_CONNECTED` answer for a connection is reported to `onConnectionLost`. */
const invoke = async <T>(command: string, args: Record<string, unknown> = {}, options: InvokeOptions = {}): Promise<T> => {
  try {
    return await transportInvoke<T>(command, args, options);
  } catch (error) {
    if (codeOf(error) === NOT_CONNECTED && typeof args.connectionId === "string") reportConnectionLost(args.connectionId);
    throw error;
  }
};

/** What `load_open_tabs` returns: each workspace's saved tabs and the revision it was saved at. */
export interface StoredOpenTabs {
  workspaces: Record<string, unknown>;
  revisions: Record<string, number>;
}

/** What `save_open_tabs` answers; `saved` false: another window saved first (nothing was written), `current` is theirs. */
export interface OpenTabsSaveOutcome {
  saved: boolean;
  revision: number;
  current: unknown;
}
/** `parse_connection_string`: the profile fields a connection string sets, and its password (never stored in the profile). */
export interface ParsedConnectionString {
  spec: ConnectionSpec;
  password: string | null;
}

/** `completion_schema`: what the editor completes for one database (the language library's shape). */
export type { CompletionSchema, FormatOptions } from "../lang/types";

export const api = {
  getConfig: () => invoke<AppConfig>("get_config"),
  /**
   * `secrets` holds other slots (SSH password, API key…); `password` stays for the password slot. `workspaceId` moves the
   * connection (null keeps it where it is). `relaxConfirmation` is the Production workspace name, when the save makes the
   * connection's rules less strict there (`RELAX_CONFIRMATION_REQUIRED`).
   */
  saveConnection: (
    profile: ConnectionProfile,
    password: string | null,
    workspaceId: string | null = null,
    secrets: Partial<Record<SecretSlot, string>> | null = null,
    relaxConfirmation: string | null = null,
  ) => invoke<AppConfig>("save_connection", { profile, password, workspaceId, secrets, relaxConfirmation }),
  deleteConnection: (id: string) => invoke<AppConfig>("delete_connection", { id }),
  /** `secrets`: other slots typed in the dialog (SSH password or key passphrase, TLS key passphrase). */
  testConnection: (profile: ConnectionProfile, password: string | null, secrets: Partial<Record<SecretSlot, string>> | null = null) =>
    invoke<ConnectInfo>("test_connection", { profile, password, secrets }),
  connect: (id: string, password: string | null) => invoke<ConnectInfo>("connect", { id, password }),
  disconnect: (id: string) => invoke<void>("disconnect", { id }),

  // --- Engine contract (views get the per-connection ones through `connectionApi.ts`) ---
  /** Every engine this build knows, planned ones included (`core/engines.ts` caches them). */
  engineDescriptors: () => invoke<EngineDescriptor[]>("engine_descriptors"),
  parseConnectionString: (engine: string, text: string) => invoke<ParsedConnectionString>("parse_connection_string", { engine, text }),
  treeChildren: (connectionId: string, request: TreeRequest) => invoke<TreeChildren>("tree_children", { connectionId, request }),
  describeObject: (connectionId: string, object: ObjectRef) => invoke<ObjectDescription>("describe_object", { connectionId, object }),
  browseObject: (connectionId: string, request: ObjectBrowseRequest) => invoke<Page>("browse_object", { connectionId, request }),
  /**
   * Row edits from a grid save or an import. Updates and deletes carry `original` (and `version`); a changed row fails the
   * whole set with a `CONFLICT` error. Batches of one import share `importId` and are confirmed once.
   */
  applyEdits: (
    connectionId: string,
    changes: RowChangeSet,
    caller: Caller = "gridSave",
    confirmation: Confirmation | null = null,
    importId: string | null = null,
  ) => invoke<ApplyResult>("apply_edits", { connectionId, changes, caller, confirmation, importId }),
  /** Opens a query session; `options` picks the language and database (defaults: the engine's default language). */
  openSession: (connectionId: string, sessionId: string, options: SessionOptions | null = null) =>
    invoke<SessionInfo>("open_session", { connectionId, sessionId, options }),
  closeSession: (sessionId: string) => invoke<void>("close_session", { sessionId }),
  /**
   * Streams `StreamEvent`s (statement, columns, row batches, done, notices) through `onEvent`; resolves with the summary.
   * `maxRows` keeps fewer rows per result set than the workspace rules allow (never more).
   */
  run: (
    sessionId: string,
    request: RunRequest,
    onEvent: (event: StreamEvent) => void,
    confirmation: Confirmation | null = null,
    maxRows: number | null = null,
  ) => invokeStreaming<RunSummary, StreamEvent>("run", { sessionId, request, confirmation, maxRows }, "onEvent", onEvent),
  cancelRun: (sessionId: string) => invoke<void>("cancel_run", { sessionId }),
  /** How the engine classifies `text` in the session's language, one category per statement; nothing runs. */
  classifyRun: (sessionId: string, text: string) => invoke<StatementCategory[]>("classify_run", { sessionId, text }),
  /** Begin / commit / rollback / auto-commit of a session; resolves with the engine-reported state. */
  transaction: (sessionId: string, op: TransactionOp) => invoke<TransactionState>("transaction", { sessionId, op }),
  /** An engine-specific operation (`vector.search`, `redis.scan`, …); the gateway refuses names the engine does not know. */
  invokeCapability: <T = unknown>(connectionId: string, name: string, args: unknown = null, confirmation: Confirmation | null = null) =>
    invoke<T>("invoke_capability", { connectionId, name, args, confirmation }),
  completionSchema: (connectionId: string, database: string | null) => invoke<CompletionSchema>("completion_schema", { connectionId, database }),
  formatSql: (dialect: SqlDialectId | null, text: string, options: FormatOptions | null = null) => invoke<string>("format_sql", { dialect, text, options }),

  schemaGraph: (connectionId: string, database: string) => invoke<SchemaGraph>("schema_graph", { connectionId, database }),
  createDatabase: (connectionId: string, name: string, firstCollection: string | null, confirmation: Confirmation | null = null) =>
    invoke<void>("create_database", { connectionId, name, firstCollection, confirmation }),
  taskStatuses: () => invoke<TaskStatus[]>("task_statuses"),
  taskLogs: (id: string) => invoke<string[]>("task_logs", { id }),
  startTask: (id: string) => invoke<void>("start_task", { id }),
  stopTask: (id: string) => invoke<void>("stop_task", { id }),
  restartTask: (id: string) => invoke<void>("restart_task", { id }),
  saveTask: (task: TaskDef, workspaceId: string | null = null) => invoke<AppConfig>("save_task", { task, workspaceId }),
  deleteTask: (id: string) => invoke<AppConfig>("delete_task", { id }),
  /** Relaxing a Production workspace needs its name in `relaxConfirmation`. */
  saveWorkspace: (workspace: WorkspaceMeta, relaxConfirmation: string | null = null) => invoke<AppConfig>("save_workspace", { workspace, relaxConfirmation }),
  /** The confirmation dialog's "Don't ask again": plain confirmations of these categories stop on this connection's rules. */
  skipConfirmations: (connectionId: string, categories: StatementCategory[]) => invoke<AppConfig>("skip_confirmations", { connectionId, categories }),
  setActiveWorkspace: (id: string) => invoke<AppConfig>("set_active_workspace", { id }),
  deleteWorkspace: (id: string) => invoke<AppConfig>("delete_workspace", { id }),
  // Vector tooling: engine `vector.*` operations behind typed commands (typed key cells in, typed result sets out).
  vectorInfo: (connectionId: string, database: string, table: string) => invoke<VectorInfo>("vector_info", { connectionId, database, table }),
  fetchVector: (connectionId: string, database: string, table: string, column: string, key: KeyCell[]) =>
    invoke<number[]>("fetch_vector", { connectionId, database, table, column, key }),
  vectorSearch: (request: VectorSearchRequest) => invoke<VectorSearchResult>("vector_search", { request }),
  vectorHealth: (connectionId: string, database: string, table: string, column: string, sample: number) =>
    invoke<HealthCheck[]>("vector_health", { connectionId, database, table, column, sample }),
  indexRecall: (connectionId: string, database: string, table: string, column: string, k: number, queries: number) =>
    invoke<RecallReport>("index_recall", { connectionId, database, table, column, k, queries }),
  saveEmbeddingSettings: (settings: EmbeddingSettings | null, apiKey: string | null) => invoke<AppConfig>("save_embedding_settings", { settings, apiKey }),
  testEmbedding: () => invoke<number>("test_embedding"),
  /** Upserts an LLM connection; an empty key keeps the saved one. */
  saveAiConnection: (connection: AiConnection, apiKey: string | null) => invoke<AppConfig>("save_ai_connection", { connection, apiKey }),
  deleteAiConnection: (id: string) => invoke<AppConfig>("delete_ai_connection", { id }),
  setAiActive: (connectionId: string, model: string) => invoke<AppConfig>("set_ai_active", { connectionId, model }),
  /** An empty key falls back to the one saved for `connectionId`. */
  listAiModels: (settings: AiSettings, apiKey: string | null, connectionId: string | null = null) =>
    invoke<string[]>("list_ai_models", { settings, apiKey, connectionId }),
  /** One short exchange with the settings as typed (unsaved); an empty key means `connectionId`'s saved one. */
  testAi: (settings: AiSettings, apiKey: string | null, connectionId: string | null = null) => invoke<string>("test_ai", { settings, apiKey, connectionId }),
  /**
   * Desktop: writes into Downloads and returns the full path. Web: the browser downloads the file; returns its name.
   * `base64` for binary data.
   */
  saveDownload: (name: string, text: string | null, base64: string | null) =>
    isTauri() ? invoke<string>("save_download", { name, text, base64 }) : Promise.resolve(downloadInBrowser(name, text, base64)),
  /** Desktop only (`capabilities().revealFile`). */
  revealFile: (path: string) => invoke<void>("reveal_file", { path }),
  /** What this edition supports; see `core/capabilities.ts`. */
  capabilities: () => invoke<Capabilities>("capabilities"),
  /** Streams the reply through `onEvent`; resolves with the full text. */
  aiChat: (request: ChatRequest, onEvent: (event: ChatEvent) => void) => invokeStreaming<string, ChatEvent>("ai_chat", { request }, "onEvent", onEvent),
  aiCancel: (id: string) => invoke<void>("ai_cancel", { id }),
  /** Records a query run; resolves with the stored entry, or null when history is turned off. */
  historyAppend: (entry: HistoryEntry) => invoke<HistoryEntry | null>("history_append", { entry }),
  /** The open tabs of every workspace from the last session, by workspace id (`app/tabState.ts` reads them). */
  loadOpenTabs: () => invoke<StoredOpenTabs>("load_open_tabs"),
  /** Saves one workspace's open tabs; null forgets them. */
  saveOpenTabs: (workspaceId: string, tabs: unknown, revision: number | null = null) =>
    invoke<OpenTabsSaveOutcome>("save_open_tabs", { workspaceId, tabs, revision }),
  /** `saveOpenTabs` while the page unloads (web): the request outlives the page (`keepalive`, bodies up to 64 KB). */
  saveOpenTabsOnUnload: (workspaceId: string, tabs: unknown, revision: number | null) =>
    invoke<OpenTabsSaveOutcome>("save_open_tabs", { workspaceId, tabs, revision }, { keepalive: true }),
  /** Matching entries, newest first. */
  historyList: (query: HistoryQuery) => invoke<HistoryEntry[]>("history_list", { query }),
  historyDelete: (ids: string[]) => invoke<number>("history_delete", { ids }),
  historyClear: () => invoke<void>("history_clear"),
  setQueryHistory: (enabled: boolean) => invoke<AppConfig>("set_query_history", { enabled }),
  /** Settings → "Allow the assistant to run read-only queries". */
  setAssistantReadQueries: (enabled: boolean) => invoke<AppConfig>("set_assistant_read_queries", { enabled }),
  /** App version, OS, open connections, recent errors and the log folder, for bug reports. */
  diagnostics: () => invoke<Diagnostics>("diagnostics"),
  setDebugLogging: (enabled: boolean) => invoke<AppConfig>("set_debug_logging", { enabled }),
  /** Desktop only: opens the log folder in the file manager. */
  openLogFolder: () => invoke<void>("open_log_folder"),
  /** The latest release compared with this build; without `force` at most one request a day. `null`: nothing to show. */
  checkUpdate: (force: boolean) => invoke<UpdateInfo | null>("check_update", { force }),
  installKind: () => invoke<InstallKind>("install_kind"),
  setUpdateCheck: (enabled: boolean) => invoke<AppConfig>("set_update_check", { enabled }),
  /** Once per update: the new version, to show "What's new". */
  whatsNew: () => invoke<string | null>("whats_new"),
  /** Desktop only: opens a GitHub release page in the default browser. */
  openReleasePage: (url: string) => invoke<void>("open_release_page", { url }),
};

export function onTaskStatus(handler: (status: TaskStatus) => void): Promise<UnlistenFn> {
  return listen<TaskStatus>("task-status", handler);
}

export function onTaskLog(handler: (event: TaskLogEvent) => void): Promise<UnlistenFn> {
  return listen<TaskLogEvent>("task-log", handler);
}

/** Structured backend errors (e.g. from the statement gateway) are objects with a `code` and a `message`; see `core/errors.ts`. */
export function errorCode(error: unknown): string | null {
  return codeOf(error);
}

export function errorText(error: unknown): string {
  return textOf(error);
}
