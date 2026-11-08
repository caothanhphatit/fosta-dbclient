/**
 * TypeScript mirrors of the engine contract (`src-tauri/src/contract`). The Rust test `contract::fixtures` writes sample
 * JSON of every type to `__fixtures__/contract/`; `contract.test.ts` checks those samples against these types, so a
 * change on one side that the other does not follow fails a test.
 */

// --- Values -------------------------------------------------------------------------------------------------------------

/** A typed cell: `{ t, v }`. Ints outside ±2^53 travel as strings; temporals, decimals and bytes (`0x…`) as text. */
export type Value =
  | { t: "null"; v: null }
  | { t: "bool"; v: boolean }
  | { t: "int"; v: number | string }
  | { t: "float"; v: number | string }
  | { t: "decimal" | "text" | "bytes" | "date" | "time" | "timestamp" | "timestamptz" | "interval" | "uuid" | "json" | "objectId"; v: string }
  | { t: "array"; v: Value[] }
  | { t: "doc"; v: [string, Value][] }
  | { t: "other"; type: string; v: string };

export type ValueTag = Value["t"];

export type ValueKind =
  | {
      kind:
        | "bool"
        | "int"
        | "float"
        | "decimal"
        | "text"
        | "bytes"
        | "date"
        | "time"
        | "timestamp"
        | "timestampTz"
        | "interval"
        | "uuid"
        | "json"
        | "objectId"
        | "document"
        | "other";
    }
  | { kind: "array"; of: ValueKind };

// --- Objects and the tree -----------------------------------------------------------------------------------------------

/** Common kinds: database, schema, folder, table, view, materializedView, collection, key, searchIndex, … Engine-specific
 * kinds are namespaced ("redis.stream"). */
export type NodeKind = string;

export interface ObjectRef {
  database: string | null;
  schema: string | null;
  name: string;
  kind: NodeKind;
  /** Extra qualifiers (a package member, an index field). */
  path?: string[];
  /** `name` is base64 (non-UTF-8 Redis keys). */
  binary?: boolean;
}

export interface NodeKey {
  /** Engine-opaque and stable; only stored and sent back. */
  id: string;
  kind: NodeKind;
  object: ObjectRef | null;
}

export type Tone = "neutral" | "ok" | "warn" | "danger";
export interface Badge {
  text: string;
  tone: Tone;
}

export type BuiltinAction = "refresh" | "copyName" | "newQuery" | "diagram" | "createDatabase" | "import" | "export" | "design";

export type ActionTarget =
  | { kind: "view"; role: string }
  | { kind: "query"; template: string }
  | { kind: "capability"; name: string; args: unknown }
  | { kind: "builtin"; name: BuiltinAction };

export interface NodeAction {
  id: string;
  label: string;
  icon: string | null;
  target: ActionTarget;
  danger: boolean;
  separatorBefore: boolean;
}

export interface ObjectNode {
  key: NodeKey;
  label: string;
  kind: NodeKind;
  /** An IconName from core/dom.ts, or "engine:<id>/<name>". */
  icon: string;
  object: ObjectRef | null;
  detail: string | null;
  badges: Badge[];
  expandable: boolean;
  defaultView: string | null;
  actions: NodeAction[];
}

export interface TreeRequest {
  parent: NodeKey | null;
  cursor: string | null;
  limit: number | null;
  filter: string | null;
}

export interface TreeChildren {
  nodes: ObjectNode[];
  nextCursor: string | null;
  totalHint: number | null;
}

// --- Describe -----------------------------------------------------------------------------------------------------------

export interface ColumnHints {
  primary?: boolean;
  binary?: boolean;
  vectorDims?: number;
  readOnly?: boolean;
}

export interface ColumnInfo {
  name: string;
  typeName: string;
  kind: ValueKind;
  nullable: boolean;
  default: string | null;
  comment: string | null;
  identity: string | null;
  generated: string | null;
  hints: ColumnHints;
}

export interface KeyInfo {
  name: string | null;
  columns: string[];
}

export interface ForeignKeyInfo {
  name: string | null;
  columns: string[];
  refDatabase: string | null;
  refSchema: string | null;
  refTable: string;
  refColumns: string[];
  onUpdate: string | null;
  onDelete: string | null;
}

export interface ReferencingKey {
  name: string | null;
  database: string | null;
  schema: string | null;
  table: string;
  columns: string[];
  refColumns: string[];
}

export interface IndexInfo {
  name: string;
  columns: string[];
  unique: boolean;
  primary: boolean;
  method: string | null;
  definition: string | null;
}

export interface ObjectDescription {
  object: ObjectRef;
  comment: string | null;
  columns: ColumnInfo[];
  primaryKey: KeyInfo | null;
  uniqueKeys: KeyInfo[];
  foreignKeys: ForeignKeyInfo[];
  referencedBy: ReferencingKey[];
  indexes: IndexInfo[];
  rowEstimate: number | null;
  ddl: string | null;
  extra: [string, string][];
}

// --- Browse and edit ----------------------------------------------------------------------------------------------------

export type FilterOp =
  | "eq"
  | "ne"
  | "lt"
  | "le"
  | "gt"
  | "ge"
  | "contains"
  | "notContains"
  | "startsWith"
  | "endsWith"
  | "like"
  | "notLike"
  | "in"
  | "notIn"
  | "between"
  | "isNull"
  | "isNotNull";

export interface FilterCond {
  column: string;
  op: FilterOp;
  values: string[];
}

export type RawFilterLang = "sql" | "mongoJson" | "esQueryDsl";

export type Filter =
  { kind: "none" } | { kind: "conditions"; conditions: FilterCond[]; matchAny: boolean } | { kind: "raw"; language: RawFilterLang; text: string };

export interface SortKey {
  column: string;
  descending: boolean;
}

export interface BrowseRequest {
  object: ObjectRef;
  filter: Filter;
  sort: SortKey[];
  offset: number;
  limit: number;
  after: Value[] | null;
  estimateTotal: boolean;
  exactTotal: boolean;
  columns: string[] | null;
}

export interface Total {
  value: number;
  exact: boolean;
}

export interface Page {
  columns: ColumnInfo[];
  rows: Value[][];
  hasMore: boolean;
  keyColumns: string[];
  nextKeyset: Value[] | null;
  total: Total | null;
  statement: string | null;
  elapsedMs: number;
  /** One row version per row (PostgreSQL xmin, SQL Server rowversion); sent back as `RowChange.version`. */
  rowVersions?: Value[];
}

export interface Field {
  column: string;
  value: Value;
}

/**
 * One row edit. Updates and deletes carry what the row looked like when it was read (`original`, `version`) so the
 * engine applies them only if the row is unchanged; `force` is the conflict dialog's "Overwrite".
 */
export type RowChange =
  | { op: "insert"; values: Field[] }
  | { op: "update"; key: Field[]; values: Field[]; original: Field[]; version?: Value; force?: boolean }
  | { op: "delete"; key: Field[]; original: Field[]; version?: Value; force?: boolean };

export interface RowChangeSet {
  object: ObjectRef;
  changes: RowChange[];
}

export interface ApplyResult {
  inserted: number;
  updated: number;
  deleted: number;
}

export interface RowConflict {
  index: number;
  op: "update" | "delete" | "insert";
  key: Field[];
  /** When the row was read. */
  original: Field[];
  /** The row now; null when it no longer exists. */
  current: Field[] | null;
  /** What the user tried to write. */
  attempted: Field[];
}

/** The structured `CONFLICT` error: nothing was applied. */
export interface ConflictError {
  code: "CONFLICT";
  message: string;
  object: ObjectRef;
  conflicts: RowConflict[];
  /**
   * Engines without transactions (MongoDB standalone) undo the writes they already made when a later change conflicts:
   * true when that worked, false when some writes could not be undone. Absent when nothing had been written.
   */
  compensated?: boolean;
}

// --- Schema graph (the ER diagram) --------------------------------------------------------------------------------------

export interface SchemaColumn {
  name: string;
  columnType: string;
  nullable: boolean;
  primary: boolean;
}

export interface SchemaTable {
  name: string;
  /** "table" and "view" today; engines may report other object kinds. */
  kind: "table" | "view" | (string & {});
  rows: number | null;
  comment: string;
  columns: SchemaColumn[];
}

/** One foreign key, possibly over several columns. */
export interface Relation {
  constraint: string;
  database: string;
  table: string;
  columns: string[];
  referencedDatabase: string;
  referencedTable: string;
  referencedColumns: string[];
}

/** Everything the ER diagram draws for one database (`schema_graph`, gated on the `schemaGraph` capability). */
export interface SchemaGraph {
  tables: SchemaTable[];
  /** Foreign keys whose both ends are in `tables`. */
  relations: Relation[];
  /** Total tables in the database; more than `tables.length` when capped. */
  totalTables: number;
}

// --- Sessions and streaming ---------------------------------------------------------------------------------------------

export interface TransactionState {
  supported: boolean;
  open: boolean;
  autoCommit: boolean;
  failed: boolean;
}

export type TransactionOp = "begin" | "commit" | "rollback" | "autoCommitOn" | "autoCommitOff" | "status";

export interface SessionOptions {
  language: string | null;
  database: string | null;
}

export interface SessionInfo {
  sessionId: string;
  language: string;
  transaction: TransactionState;
  cancellable: boolean;
}

export interface RunRequest {
  text: string;
  database: string | null;
  offset: number;
}

export interface RunSummary {
  statements: number;
  elapsedMs: number;
  stopped: boolean;
  transaction: TransactionState;
}

export type StreamEvent =
  | { event: "statement"; index: number; sql: string }
  | { event: "columns"; index: number; columns: ColumnInfo[] }
  | { event: "rows"; index: number; rows: Value[][] }
  | { event: "done"; index: number; affected: number | null; rows: number; truncated: boolean; elapsedMs: number }
  | { event: "notice"; index: number; text: string };

// --- Engines ------------------------------------------------------------------------------------------------------------

export type CapabilityName =
  | "tree"
  | "describe"
  | "multipleDatabases"
  | "schemas"
  | "browseRows"
  | "editRows"
  | "rawFilter"
  | "keysetPaging"
  | "estimatedCount"
  | "exactCount"
  | "query"
  | "cancel"
  | "transactions"
  | "readOnlySession"
  | "statementTimeout"
  | "streamingResults"
  | "schemaGraph"
  | "createDatabase"
  | "explain"
  | "documents"
  | "keyValue"
  | "search"
  | "console"
  | "serverOverview"
  | "vector";

export type EngineFamily = "sql" | "document" | "keyValue" | "search";
export type EngineStatus = "ready" | "preview" | "planned";
export type SqlDialectId = "mariaDb" | "mySql" | "msSql" | "postgres" | "sqlite" | "clickHouse" | "oracle" | "generic";
export type AuthMethodId = "password" | "none" | "apiKey" | "bearer" | "aclUser";
export type TlsModeId = "disable" | "prefer" | "require" | "verifyCa" | "verifyFull";
export type SecretSlot = "password" | "sshPassword" | "sshKeyPassphrase" | "tlsKeyPassphrase" | "apiKey" | "token";
export type FormSection = "general" | "auth" | "advanced";

export type FieldKind =
  | { type: "text" }
  | { type: "number" }
  | { type: "password"; slot: SecretSlot }
  | { type: "checkbox" }
  | { type: "select"; options: [string, string][] }
  | { type: "file"; filters: string[]; save: boolean }
  | { type: "textarea" };

export interface FieldSpec {
  /** "database" | "file" | "url" | "options.<name>" */
  key: string;
  kind: FieldKind;
  label: string;
  placeholder: string | null;
  hint: string | null;
  section: FormSection;
  default: string | null;
  required: boolean;
}

export interface UrlSpec {
  schemes: string[];
  example: string;
}

export interface TlsSupport {
  modes: TlsModeId[];
  ca: boolean;
  clientCert: boolean;
  serverName: boolean;
  pin: boolean;
}

export interface ConnectionForm {
  usesHost: boolean;
  usesFile: boolean;
  defaultPort: number | null;
  defaultUser: string;
  databaseLabel: string;
  databaseHint: string;
  url: UrlSpec | null;
  auth: AuthMethodId[];
  fields: FieldSpec[];
  ssh: boolean;
  tls: TlsSupport | null;
}

export interface LanguageSpec {
  id: string;
  label: string;
  editor: string;
  dialect: SqlDialectId | null;
  default: boolean;
  hint: string;
  /** Code-fence tags in assistant replies: the assistant writes the first; a block with any of them runs in this language. */
  fences: string[];
}

/** A quick action the assistant panel offers for an engine (prefilled in the composer). */
export interface AssistantPrompt {
  label: string;
  /** An `IconName` from `core/dom.ts`. */
  icon: string;
  prompt: string;
}

/** What the assistant panel says and offers for an engine, so it never compares engine ids. */
export interface AssistantHints {
  /** How test data is written: "INSERT statements", "an insertMany command". */
  testData: string;
  prompts: AssistantPrompt[];
  /** Query tabs of this engine offer "Translate to…". */
  translateFrom: boolean;
  /** "Translate this to …"; null leaves the engine out of the menu. */
  translateTo: string | null;
  /** The menu label; the engine label when null. */
  translateLabel: string | null;
}

export interface FilterSupport {
  structured: boolean;
  raw: RawFilterLang | null;
  placeholder: string;
}

export interface EngineDescriptor {
  id: string;
  label: string;
  family: EngineFamily;
  icon: string;
  status: EngineStatus;
  sqlDialect: SqlDialectId | null;
  capabilities: CapabilityName[];
  objectsLabel: string;
  connection: ConnectionForm;
  languages: LanguageSpec[];
  filter: FilterSupport;
  assistant: AssistantHints;
}

export interface ServerInfo {
  product: string;
  version: string;
  label: string;
  flavor: string | null;
}

export interface ConnectInfo {
  engine: string;
  server: ServerInfo;
  capabilities: CapabilityName[];
  languages: LanguageSpec[];
  connectionActions: NodeAction[];
}

export type TlsMode = TlsModeId;

export interface TlsConfig {
  mode: TlsMode;
  caFile: string | null;
  clientCertFile: string | null;
  clientKeyFile: string | null;
  serverName: string | null;
  acceptInvalidCerts: boolean;
  pinnedSha256: string | null;
}

export type SshAuth = { method: "password" } | { method: "privateKey"; keyFile: string } | { method: "agent" };

export interface SshConfig {
  enabled: boolean;
  host: string;
  port: number;
  user: string;
  auth: SshAuth;
  jumpHosts: { host: string; port: number; user: string }[];
  hostKey: "acceptNew" | "strict" | "off";
  keepaliveSecs: number | null;
}

export interface ConnectionSpec {
  engine: string;
  host: string;
  port: number;
  user: string;
  database: string | null;
  file: string | null;
  url: string | null;
  options: Record<string, string>;
  tls: TlsConfig | null;
  ssh: SshConfig | null;
  auth: AuthMethodId | null;
}

// --- Errors -------------------------------------------------------------------------------------------------------------

/** Where in the statement an error is: 1-based line and column, 0-based character offset. */
export interface ErrorPosition {
  line?: number;
  column?: number;
  offset?: number;
}

/** Every backend error: `code` and `message` always, the rest when known. Gateway and conflict errors add their fields. */
export interface AppError {
  code: string;
  message: string;
  engine?: string;
  detail?: string;
  hint?: string;
  sqlstate?: string;
  vendorCode?: string | number;
  position?: ErrorPosition;
  statement?: { index: number; text: string };
  cause?: string[];
  retryable?: boolean;
  [extra: string]: unknown;
}
