import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AiConnection,
  AiSettings,
  AppConfig,
  BrowseRequest,
  Caller,
  CellValue,
  ChatEvent,
  ChatRequest,
  Confirmation,
  ConnectionProfile,
  EmbeddingSettings,
  HealthCheck,
  HistoryEntry,
  HistoryQuery,
  QueryOutcome,
  RecallReport,
  RowChange,
  SchemaGraph,
  TableDesign,
  TableInfo,
  TablePage,
  TableRelations,
  TaskDef,
  TaskLogEvent,
  TaskStatus,
  TransactionOp,
  TransactionState,
  VectorInfo,
  VectorSearchRequest,
  VectorSearchResult,
  WorkspaceMeta,
} from "./types";

export const PASSWORD_REQUIRED = "PASSWORD_REQUIRED";

export const api = {
  getConfig: () => invoke<AppConfig>("get_config"),
  saveConnection: (profile: ConnectionProfile, password: string | null, workspaceId: string | null = null) =>
    invoke<AppConfig>("save_connection", { profile, password, workspaceId }),
  deleteConnection: (id: string) => invoke<AppConfig>("delete_connection", { id }),
  testConnection: (profile: ConnectionProfile, password: string | null) => invoke<string>("test_connection", { profile, password }),
  connect: (id: string, password: string | null) => invoke<string>("connect", { id, password }),
  disconnect: (id: string) => invoke<void>("disconnect", { id }),
  listDatabases: (connectionId: string) => invoke<string[]>("list_databases", { connectionId }),
  schemaGraph: (connectionId: string, database: string) => invoke<SchemaGraph>("schema_graph", { connectionId, database }),
  createDatabase: (connectionId: string, name: string, firstCollection: string | null, confirmation: Confirmation | null = null) =>
    invoke<void>("create_database", { connectionId, name, firstCollection, confirmation }),
  listTables: (connectionId: string, database: string) => invoke<TableInfo[]>("list_tables", { connectionId, database }),
  describeTable: (connectionId: string, database: string, table: string) => invoke<TableDesign>("describe_table", { connectionId, database, table }),
  browseTable: (request: BrowseRequest) => invoke<TablePage>("browse_table", { request }),
  /** `caller` is "gridSave" or "import"; batches of one import share `importId` and are confirmed once. */
  applyChanges: (
    connectionId: string,
    database: string,
    table: string,
    changes: RowChange[],
    caller: Caller = "gridSave",
    confirmation: Confirmation | null = null,
    importId: string | null = null,
  ) => invoke<number>("apply_changes", { connectionId, database, table, changes, caller, confirmation, importId }),
  openSession: (connectionId: string, sessionId: string) => invoke<void>("open_session", { connectionId, sessionId }),
  closeSession: (sessionId: string) => invoke<void>("close_session", { sessionId }),
  runSql: (sessionId: string, database: string | null, sql: string, confirmation: Confirmation | null = null) =>
    invoke<QueryOutcome>("run_sql", { sessionId, database, sql, confirmation }),
  /** Begin / Commit / Rollback / auto-commit for a query tab; resolves with the engine-reported state. */
  sqlTransaction: (sessionId: string, op: TransactionOp) => invoke<TransactionState>("sql_transaction", { sessionId, op }),
  cancelSql: (sessionId: string) => invoke<void>("cancel_sql", { sessionId }),
  taskStatuses: () => invoke<TaskStatus[]>("task_statuses"),
  taskLogs: (id: string) => invoke<string[]>("task_logs", { id }),
  startTask: (id: string) => invoke<void>("start_task", { id }),
  stopTask: (id: string) => invoke<void>("stop_task", { id }),
  restartTask: (id: string) => invoke<void>("restart_task", { id }),
  saveTask: (task: TaskDef, workspaceId: string | null = null) => invoke<AppConfig>("save_task", { task, workspaceId }),
  deleteTask: (id: string) => invoke<AppConfig>("delete_task", { id }),
  /** Relaxing a Production workspace needs its name in `relaxConfirmation`. */
  saveWorkspace: (workspace: WorkspaceMeta, relaxConfirmation: string | null = null) => invoke<AppConfig>("save_workspace", { workspace, relaxConfirmation }),
  setActiveWorkspace: (id: string) => invoke<AppConfig>("set_active_workspace", { id }),
  deleteWorkspace: (id: string) => invoke<AppConfig>("delete_workspace", { id }),
  tableRelations: (connectionId: string, database: string, table: string) => invoke<TableRelations>("table_relations", { connectionId, database, table }),
  vectorInfo: (connectionId: string, database: string, table: string) => invoke<VectorInfo>("vector_info", { connectionId, database, table }),
  fetchVector: (connectionId: string, database: string, table: string, column: string, key: CellValue[]) =>
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
  /** Writes into Downloads and returns the full path; `base64` for binary data. */
  saveDownload: (name: string, text: string | null, base64: string | null) => invoke<string>("save_download", { name, text, base64 }),
  revealFile: (path: string) => invoke<void>("reveal_file", { path }),
  /** Streams the reply through `onEvent`; resolves with the full text. */
  aiChat: (request: ChatRequest, onEvent: (event: ChatEvent) => void) => {
    const channel = new Channel<ChatEvent>();
    channel.onmessage = onEvent;
    return invoke<string>("ai_chat", { request, onEvent: channel });
  },
  aiCancel: (id: string) => invoke<void>("ai_cancel", { id }),
  /** Records a query run; resolves with the stored entry, or null when history is turned off. */
  historyAppend: (entry: HistoryEntry) => invoke<HistoryEntry | null>("history_append", { entry }),
  /** Matching entries, newest first. */
  historyList: (query: HistoryQuery) => invoke<HistoryEntry[]>("history_list", { query }),
  historyDelete: (ids: string[]) => invoke<number>("history_delete", { ids }),
  historyClear: () => invoke<void>("history_clear"),
  setQueryHistory: (enabled: boolean) => invoke<AppConfig>("set_query_history", { enabled }),
};

export function onTaskStatus(handler: (status: TaskStatus) => void): Promise<UnlistenFn> {
  return listen<TaskStatus>("task-status", (event) => handler(event.payload));
}

export function onTaskLog(handler: (event: TaskLogEvent) => void): Promise<UnlistenFn> {
  return listen<TaskLogEvent>("task-log", (event) => handler(event.payload));
}

/** Structured backend errors (e.g. from the statement gateway) are objects with a `code` and a `message`. */
export function errorCode(error: unknown): string | null {
  if (typeof error === "object" && error !== null && "code" in error && typeof error.code === "string") return error.code;
  return null;
}

export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (typeof error === "object" && error !== null && "message" in error && typeof error.message === "string") return error.message;
  return JSON.stringify(error);
}
