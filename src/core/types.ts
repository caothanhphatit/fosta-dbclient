import type { AuthMethodId, Relation, SshConfig, TlsConfig } from "./contract";

export type Cell = string | null;

export type DriverId = "mariadb" | "mysql" | "mssql" | "mongodb";

export interface ConnectionProfile {
  id: string;
  name: string;
  driver: DriverId;
  host: string;
  port: number;
  user: string;
  database: string | null;
  /** SQLite database file. */
  file?: string;
  /** Connection string without its password (MongoDB SRV / replica sets); the backend moves `options.uri` here. */
  url?: string;
  savePassword: boolean;
  /** Driver-specific settings, see drivers.ts for the keys each driver reads. */
  options: Record<string, string>;
  auth?: AuthMethodId;
  /** TLS settings; the backend moves the SQL Server `options.encrypt` / `options.trustServerCertificate` here. */
  tls?: TlsConfig;
  ssh?: SshConfig;
  /** Safety rules for this connection only (replacing the workspace's); not edited by the dialog yet. */
  safety?: SafetySettings | null;
}

/** Safety presets of a workspace (see the statement gateway in ARCHITECTURE.md). */
export type SafetyPreset = "open" | "guarded" | "readOnly" | "custom";

/** What happens to a statement of one category, from least to most strict. */
export type SafetyAction = "allow" | "confirm" | "typed" | "block";

/** Statement categories the gateway reports. "unknown" is SQL it could not parse (treated like a write). */
export type StatementCategory = "read" | "transaction" | "write" | "unknown" | "ddl" | "destructive" | "admin";

export interface CategoryRules {
  read: SafetyAction;
  write: SafetyAction;
  ddl: SafetyAction;
  destructive: SafetyAction;
  admin: SafetyAction;
}

/** The `safety` object of a workspace in fosta-studio.json. */
export interface SafetySettings {
  preset: SafetyPreset;
  /** The Custom matrix (kept while another preset is selected). */
  rules: CategoryRules;
  /** Custom only: several statements per query-tab run. */
  allowMultiStatement: boolean;
  /** Custom only: statement timeout in seconds, 0 = none. */
  statementTimeoutSecs: number;
  /** Custom only: rows kept per result set in query tabs. */
  maxRows: number;
  /** What the workspace started from; anything looser counts as relaxed. */
  baseline: SafetyPreset | null;
}

export type Caller = "queryTab" | "gridFilter" | "gridSave" | "import" | "createDatabase" | "assistant" | "transaction" | "settings";

/** One statement as the gateway classified it. */
export interface StatementInfo {
  category: StatementCategory;
  kind: string;
  tables: string[];
  excerpt: string;
  reason: string | null;
}

/** Payload of a `CONFIRMATION_REQUIRED` or `BLOCKED` error from the gateway. */
export interface GatewayProblem {
  code: "CONFIRMATION_REQUIRED" | "BLOCKED";
  message: string;
  caller: Caller;
  workspace: string;
  connection: string;
  preset: SafetyPreset;
  level: SafetyAction;
  token: string | null;
  /** What to type for a typed confirmation (a table or workspace name). */
  expected: string | null;
  categories: StatementCategory[];
  tables: string[];
  statements: StatementInfo[];
}

/** The user's answer to a confirmation request; sent back with the same call. */
export interface Confirmation {
  token: string;
  typed: string | null;
}

/** Transaction state of a query tab, as reported by the engine. */
export interface TransactionState {
  supported: boolean;
  open: boolean;
  autoCommit: boolean;
}

export type TransactionOp = "begin" | "commit" | "rollback" | "autoCommitOn" | "autoCommitOff" | "status";

export interface TaskDef {
  id: string;
  name: string;
  program: string;
  args: string[];
  autoStart: boolean;
  autoRestart: boolean;
  readyPattern: string | null;
}

export type EmbeddingAuth = "bearer" | "api-key" | "none";

export interface EmbeddingSettings {
  url: string;
  model: string;
  auth: EmbeddingAuth;
  dimensions: number | null;
}

/** Identity of a workspace; what the workspace dialog edits. */
export interface WorkspaceMeta {
  id: string;
  name: string;
  color: string;
  /** Environment kind shown on the rail: laptop, cloud, shield, … (see workspaces.ts). */
  icon: string;
  /** Statement rules; always present in a loaded config, omitted to keep the current ones. */
  safety?: SafetySettings;
}

/** A workspace groups the connections and startup tasks of one environment. */
export interface Workspace extends WorkspaceMeta {
  connections: ConnectionProfile[];
  tasks: TaskDef[];
}

export type AiProvider = "fosta" | "openai" | "gemini" | "anthropic" | "custom";

export interface AiSettings {
  provider: AiProvider;
  /** Empty means the provider's public endpoint. */
  baseUrl: string;
  model: string;
}

/** A saved LLM provider account; its key is in Credential Manager. */
export interface AiConnection {
  id: string;
  name: string;
  provider: AiProvider;
  /** Empty means the provider's public endpoint. */
  baseUrl: string;
  defaultModel: string;
}

/** The connection and model the chat uses. */
export interface AiActive {
  connectionId: string;
  model: string;
}

/** `diagnostics()`: what a bug report needs, without secrets or row data. */
export interface Diagnostics {
  appVersion: string;
  edition: string;
  os: string;
  arch: string;
  osVersion: string | null;
  logFolder: string | null;
  debugLogging: boolean;
  connections: { engine: string; version: string; crashed: boolean }[];
  recentErrors: { time: string; level: string; target: string; message: string; code: string | null }[];
}

/** What the running edition supports (Rust `Capabilities`); the UI hides the rest. */
export interface Capabilities {
  edition: "desktop" | "web";
  /** Startup tasks (child processes) can run. */
  tasks: boolean;
  /** "Show in folder" after an export. */
  revealFile: boolean;
  /** Exports are downloaded by the browser instead of written to ~/Downloads. */
  browserDownloads: boolean;
  /** Where saved passwords and API keys go, as shown in dialogs. */
  secretStore: string;
  /** The backend runs in a container, where `localhost` is not the user's machine. */
  inContainer: boolean;
}

export interface AppConfig {
  workspaces: Workspace[];
  activeWorkspace: string;
  embedding: EmbeddingSettings | null;
  aiConnections: AiConnection[];
  /** Set whenever there is at least one connection. */
  aiActive: AiActive | null;
  /** Record query-tab runs in the local history (default on). */
  queryHistory: boolean;
  /** Debug-level entries in the application log (Settings → Debug logging). */
  debugLogging?: boolean;
  /** Settings file version; 2 since the engine contract. */
  configVersion?: number;
  /** Set once after startup when the settings file could not be read and was set aside. */
  startupNotice?: string;
}

export interface ChatMessage {
  role: "user" | "assistant";
  content: string;
}

/** Where the user is when they ask; the backend adds the table list itself. */
export interface ChatContext {
  connectionId?: string;
  connectionName?: string;
  driverLabel?: string;
  serverVersion?: string;
  database?: string;
  table?: string;
  editorText?: string;
  lastError?: string;
  extra?: string;
}

export interface ChatRequest {
  id: string;
  messages: ChatMessage[];
  context: ChatContext;
  /** Overrides the active LLM connection / model for this request. */
  connectionId?: string;
  model?: string;
}

/** Views the assistant can open; none of them changes data. */
export type UiAction =
  | { kind: "openDiagram"; database: string; focusTable: string | null }
  /** The filter is only pre-filled as a suggestion; nothing runs until the user applies it. */
  | { kind: "openTable"; database: string; table: string; suggestedFilter: string | null }
  | { kind: "openQuery"; database: string | null; sql: string };

export type ChatEvent = { type: "text"; text: string } | { type: "tool"; label: string } | { type: "action"; action: UiAction } | { type: "done" };

export type TaskState = "stopped" | "starting" | "running" | "restarting" | "failed";

export interface TaskStatus {
  id: string;
  state: TaskState;
  pid: number | null;
  restarts: number;
  message: string | null;
}

export interface TaskLogEvent {
  id: string;
  line: string;
}

export interface TableInfo {
  name: string;
  kind: "table" | "view";
  rows: number | null;
  comment: string;
}

export interface TableTarget {
  connectionId: string;
  driver: DriverId;
  database: string;
  table: string;
  kind: "table" | "view";
}

export interface ColumnMeta {
  name: string;
  typeName: string;
  numeric: boolean;
  binary: boolean;
  primary: boolean;
  vectorDims: number | null;
}

export interface ResultSet {
  columns: ColumnMeta[];
  rows: Cell[][];
  affectedRows: number;
  truncated: boolean;
}

export interface QueryOutcome {
  sets: ResultSet[];
  elapsedMs: number;
  transaction: TransactionState;
}

export interface TablePage {
  columns: ColumnMeta[];
  rows: Cell[][];
  total: number;
  primaryKey: string[];
  elapsedMs: number;
}

export interface ColumnDesign {
  name: string;
  columnType: string;
  nullable: boolean;
  key: string;
  defaultValue: Cell;
  extra: string;
  comment: string;
}

export interface TableDesign {
  columns: ColumnDesign[];
  ddl: string;
}

export interface BrowseRequest {
  connectionId: string;
  database: string;
  table: string;
  offset: number;
  limit: number;
  orderBy: string | null;
  descending: boolean;
  filter: string | null;
}

export interface CellValue {
  column: string;
  value: Cell;
}

export type { Relation, SchemaColumn, SchemaGraph, SchemaTable } from "./contract";

export interface TableRelations {
  outgoing: Relation[];
  incoming: Relation[];
}

export interface VectorIndex {
  name: string;
  column: string;
  metric: string;
  m: number | null;
}

export interface VectorInfo {
  supported: boolean;
  columns: { name: string; dims: number }[];
  indexes: VectorIndex[];
}

export type Metric = "auto" | "cosine" | "euclidean";

export type QueryVector = { kind: "row"; key: CellValue[] } | { kind: "literal"; text: string } | { kind: "text"; text: string };

export interface VectorSearchRequest {
  connectionId: string;
  database: string;
  table: string;
  column: string;
  query: QueryVector;
  k: number;
  metric: Metric;
  filter: string | null;
}

export interface VectorSearchResult {
  result: ResultSet;
  plan: ResultSet;
  indexUsed: string | null;
  sql: string;
  runnableSql: string;
  elapsedMs: number;
  queryDims: number;
  primaryKey: string[];
}

export type CheckStatus = "ok" | "info" | "warn" | "error";

export interface HealthCheck {
  label: string;
  status: CheckStatus;
  value: string;
  detail: string;
}

export interface RecallReport {
  index: string;
  metric: string;
  k: number;
  queries: number;
  recall: number;
  worstRecall: number;
  annMs: number;
  exactMs: number;
}

export type RowChange =
  { kind: "insert"; values: CellValue[] } | { kind: "update"; key: CellValue[]; values: CellValue[] } | { kind: "delete"; key: CellValue[] };

/** One result set of a query run, as recorded in the history. */
export interface HistoryResult {
  /** True for a set with columns (rows), false for a statement that reports affected rows. */
  rowSet: boolean;
  rows: number;
  affectedRows: number;
  truncated: boolean;
}

/** A run in a query tab, stored by the backend in fosta-studio-history.jsonl on this machine. */
export interface HistoryEntry {
  /** Assigned by the backend; empty when appending. */
  id: string;
  sql: string;
  connectionId: string;
  connectionName: string;
  driver: string;
  database: string | null;
  /** Milliseconds since the Unix epoch. */
  startedAt: number;
  durationMs: number;
  results: HistoryResult[];
  error: string | null;
  cancelled: boolean;
}

export interface HistoryQuery {
  connectionId?: string;
  search?: string;
  limit: number;
  /** Only entries that started before this time (ms since the epoch), for paging. */
  before?: number;
}
