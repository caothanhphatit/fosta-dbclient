import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AiSettings,
  AppConfig,
  BrowseRequest,
  CellValue,
  ChatEvent,
  ChatRequest,
  ConnectionProfile,
  EmbeddingSettings,
  HealthCheck,
  QueryOutcome,
  RecallReport,
  RowChange,
  TableDesign,
  TableInfo,
  TablePage,
  TableRelations,
  TaskDef,
  TaskLogEvent,
  TaskStatus,
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
  testConnection: (profile: ConnectionProfile, password: string | null) =>
    invoke<string>("test_connection", { profile, password }),
  connect: (id: string, password: string | null) => invoke<string>("connect", { id, password }),
  disconnect: (id: string) => invoke<void>("disconnect", { id }),
  listDatabases: (connectionId: string) => invoke<string[]>("list_databases", { connectionId }),
  listTables: (connectionId: string, database: string) =>
    invoke<TableInfo[]>("list_tables", { connectionId, database }),
  describeTable: (connectionId: string, database: string, table: string) =>
    invoke<TableDesign>("describe_table", { connectionId, database, table }),
  browseTable: (request: BrowseRequest) => invoke<TablePage>("browse_table", { request }),
  applyChanges: (connectionId: string, database: string, table: string, changes: RowChange[]) =>
    invoke<number>("apply_changes", { connectionId, database, table, changes }),
  openSession: (connectionId: string, sessionId: string) =>
    invoke<void>("open_session", { connectionId, sessionId }),
  closeSession: (sessionId: string) => invoke<void>("close_session", { sessionId }),
  runSql: (sessionId: string, database: string | null, sql: string) =>
    invoke<QueryOutcome>("run_sql", { sessionId, database, sql }),
  cancelSql: (sessionId: string) => invoke<void>("cancel_sql", { sessionId }),
  taskStatuses: () => invoke<TaskStatus[]>("task_statuses"),
  taskLogs: (id: string) => invoke<string[]>("task_logs", { id }),
  startTask: (id: string) => invoke<void>("start_task", { id }),
  stopTask: (id: string) => invoke<void>("stop_task", { id }),
  restartTask: (id: string) => invoke<void>("restart_task", { id }),
  saveTask: (task: TaskDef, workspaceId: string | null = null) => invoke<AppConfig>("save_task", { task, workspaceId }),
  deleteTask: (id: string) => invoke<AppConfig>("delete_task", { id }),
  saveWorkspace: (workspace: WorkspaceMeta) => invoke<AppConfig>("save_workspace", { workspace }),
  setActiveWorkspace: (id: string) => invoke<AppConfig>("set_active_workspace", { id }),
  deleteWorkspace: (id: string) => invoke<AppConfig>("delete_workspace", { id }),
  tableRelations: (connectionId: string, database: string, table: string) =>
    invoke<TableRelations>("table_relations", { connectionId, database, table }),
  vectorInfo: (connectionId: string, database: string, table: string) =>
    invoke<VectorInfo>("vector_info", { connectionId, database, table }),
  fetchVector: (connectionId: string, database: string, table: string, column: string, key: CellValue[]) =>
    invoke<number[]>("fetch_vector", { connectionId, database, table, column, key }),
  vectorSearch: (request: VectorSearchRequest) => invoke<VectorSearchResult>("vector_search", { request }),
  vectorHealth: (connectionId: string, database: string, table: string, column: string, sample: number) =>
    invoke<HealthCheck[]>("vector_health", { connectionId, database, table, column, sample }),
  indexRecall: (connectionId: string, database: string, table: string, column: string, k: number, queries: number) =>
    invoke<RecallReport>("index_recall", { connectionId, database, table, column, k, queries }),
  saveEmbeddingSettings: (settings: EmbeddingSettings | null, apiKey: string | null) =>
    invoke<AppConfig>("save_embedding_settings", { settings, apiKey }),
  testEmbedding: () => invoke<number>("test_embedding"),
  saveAiSettings: (settings: AiSettings | null, apiKey: string | null) =>
    invoke<AppConfig>("save_ai_settings", { settings, apiKey }),
  testAi: () => invoke<string>("test_ai"),
  /** Streams the reply through `onEvent`; resolves with the full text. */
  aiChat: (request: ChatRequest, onEvent: (event: ChatEvent) => void) => {
    const channel = new Channel<ChatEvent>();
    channel.onmessage = onEvent;
    return invoke<string>("ai_chat", { request, onEvent: channel });
  },
  aiCancel: (id: string) => invoke<void>("ai_cancel", { id }),
};

export function onTaskStatus(handler: (status: TaskStatus) => void): Promise<UnlistenFn> {
  return listen<TaskStatus>("task-status", (event) => handler(event.payload));
}

export function onTaskLog(handler: (event: TaskLogEvent) => void): Promise<UnlistenFn> {
  return listen<TaskLogEvent>("task-log", (event) => handler(event.payload));
}

export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
}
