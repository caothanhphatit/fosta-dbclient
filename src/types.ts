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
  savePassword: boolean;
  /** Driver-specific settings, see drivers.ts for the keys each driver reads. */
  options: Record<string, string>;
}

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
}

/** A workspace groups the connections and startup tasks of one environment. */
export interface Workspace extends WorkspaceMeta {
  connections: ConnectionProfile[];
  tasks: TaskDef[];
}

export interface AppConfig {
  workspaces: Workspace[];
  activeWorkspace: string;
  embedding: EmbeddingSettings | null;
}

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

export interface Relation {
  constraint: string;
  database: string;
  table: string;
  columns: string[];
  referencedDatabase: string;
  referencedTable: string;
  referencedColumns: string[];
}

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

export type QueryVector =
  | { kind: "row"; key: CellValue[] }
  | { kind: "literal"; text: string }
  | { kind: "text"; text: string };

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
  | { kind: "insert"; values: CellValue[] }
  | { kind: "update"; key: CellValue[]; values: CellValue[] }
  | { kind: "delete"; key: CellValue[] };
