/**
 * The per-connection slice of `api` that views receive through their constructor (ADR 0001: the api is injected), so a
 * view can be tested with a fixture object instead of the backend. `bindConnection` fills in the connection id.
 */
import { api, type CompletionSchema } from "./api";
import type {
  ApplyResult,
  BrowseRequest,
  ObjectDescription,
  ObjectRef,
  Page,
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
} from "./contract";
import type { Caller, Confirmation, SchemaGraph } from "./types";

export interface ConnectionApi {
  readonly connectionId: string;
  treeChildren(request: TreeRequest): Promise<TreeChildren>;
  describe(object: ObjectRef): Promise<ObjectDescription>;
  browse(request: BrowseRequest): Promise<Page>;
  applyEdits(changes: RowChangeSet, caller?: Caller, confirmation?: Confirmation | null, importId?: string | null): Promise<ApplyResult>;
  openSession(sessionId: string, options?: SessionOptions | null): Promise<SessionInfo>;
  closeSession(sessionId: string): Promise<void>;
  run(sessionId: string, request: RunRequest, onEvent: (event: StreamEvent) => void, confirmation?: Confirmation | null): Promise<RunSummary>;
  cancelRun(sessionId: string): Promise<void>;
  transaction(sessionId: string, op: TransactionOp): Promise<TransactionState>;
  invokeCapability<T = unknown>(name: string, args?: unknown, confirmation?: Confirmation | null): Promise<T>;
  schemaGraph(database: string): Promise<SchemaGraph>;
  completionSchema(database: string): Promise<CompletionSchema>;
  createDatabase(name: string, firstCollection: string | null, confirmation?: Confirmation | null): Promise<void>;
}

export function bindConnection(connectionId: string): ConnectionApi {
  return {
    connectionId,
    treeChildren: (request) => api.treeChildren(connectionId, request),
    describe: (object) => api.describeObject(connectionId, object),
    browse: (request) => api.browseObject(connectionId, request),
    applyEdits: (changes, caller = "gridSave", confirmation = null, importId = null) => api.applyEdits(connectionId, changes, caller, confirmation, importId),
    openSession: (sessionId, options = null) => api.openSession(connectionId, sessionId, options),
    closeSession: (sessionId) => api.closeSession(sessionId),
    run: (sessionId, request, onEvent, confirmation = null) => api.run(sessionId, request, onEvent, confirmation),
    cancelRun: (sessionId) => api.cancelRun(sessionId),
    transaction: (sessionId, op) => api.transaction(sessionId, op),
    invokeCapability: <T = unknown>(name: string, args: unknown = null, confirmation: Confirmation | null = null) =>
      api.invokeCapability<T>(connectionId, name, args, confirmation),
    schemaGraph: (database) => api.schemaGraph(connectionId, database),
    completionSchema: (database) => api.completionSchema(connectionId, database),
    createDatabase: (name, firstCollection, confirmation = null) => api.createDatabase(connectionId, name, firstCollection, confirmation),
  };
}
