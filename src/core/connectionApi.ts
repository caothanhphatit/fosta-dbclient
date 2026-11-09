/**
 * The per-connection slice of `api` that views receive through their constructor (ADR 0001: the api is injected), so a
 * view can be tested with a fixture object instead of the backend. `bindConnection` fills in the connection id.
 */
import { api, type CompletionSchema } from "./api";
import type {
  ApplyResult,
  BrowseRequest,
  ObjectDescription,
  ObjectNode,
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
  /** With `requestId`, `cancelRequest(requestId)` stops the read on the server. */
  browse(request: BrowseRequest, requestId?: string | null): Promise<Page>;
  cancelRequest(requestId: string): Promise<void>;
  applyEdits(changes: RowChangeSet, caller?: Caller, confirmation?: Confirmation | null, importId?: string | null): Promise<ApplyResult>;
  openSession(sessionId: string, options?: SessionOptions | null): Promise<SessionInfo>;
  closeSession(sessionId: string): Promise<void>;
  run(sessionId: string, request: RunRequest, onEvent: (event: StreamEvent) => void, confirmation?: Confirmation | null): Promise<RunSummary>;
  cancelRun(sessionId: string): Promise<void>;
  transaction(sessionId: string, op: TransactionOp): Promise<TransactionState>;
  invokeCapability<T = unknown>(name: string, args?: unknown, confirmation?: Confirmation | null): Promise<T>;
  schemaGraph(database: string): Promise<SchemaGraph>;
  /** Tables, columns and keys of `database` for completion; without one, only the database names. */
  completionSchema(database: string | null): Promise<CompletionSchema>;
  createDatabase(name: string, firstCollection: string | null, confirmation?: Confirmation | null): Promise<void>;
}

export function bindConnection(connectionId: string): ConnectionApi {
  return {
    connectionId,
    treeChildren: (request) => api.treeChildren(connectionId, request),
    describe: (object) => api.describeObject(connectionId, object),
    browse: (request, requestId = null) => api.browseObject(connectionId, request, requestId),
    cancelRequest: (requestId) => api.cancelRequest(requestId),
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

// --- Walking the object tree ---------------------------------------------------------------------------------------------

/** Object kinds that hold rows or documents: what `listObjects` returns (and the query editor completes). */
export const RELATION_KINDS: ReadonlySet<string> = new Set(["table", "view", "materializedView", "foreignTable", "dictionary", "collection"]);

/** Node kinds `listObjects` looks inside to find relations (schemas, and engine folders such as "Tables"). */
const CONTAINER_KINDS: ReadonlySet<string> = new Set(["schema", "folder"]);

const WALK_PAGE = 500;
const WALK_MAX_PAGES = 40;

type TreeSource = Pick<ConnectionApi, "treeChildren">;

/** Every child of `parent`, following `nextCursor` (bounded, so a huge catalog cannot keep the walk going forever). */
export async function allChildren(source: TreeSource, parent: TreeRequest["parent"]): Promise<ObjectNode[]> {
  const nodes: ObjectNode[] = [];
  let cursor: string | null = null;
  for (let pageNumber = 0; pageNumber < WALK_MAX_PAGES; pageNumber += 1) {
    const page: TreeChildren = await source.treeChildren({ parent, cursor, limit: WALK_PAGE, filter: null });
    nodes.push(...page.nodes);
    cursor = page.nextCursor;
    if (!cursor) break;
  }
  return nodes;
}

/** The name a database node stands for. */
export function databaseName(node: ObjectNode): string {
  return node.object?.database ?? node.object?.name ?? node.label;
}

/** The database nodes at the top of the tree (what the old `list_databases` returned). */
export async function databaseNodes(source: TreeSource): Promise<ObjectNode[]> {
  return (await allChildren(source, null)).filter((node) => node.kind === "database");
}

/**
 * The tables, views and collections of `database` (what the old `list_tables` returned): the database node's
 * children, looking inside schema and folder nodes on the way.
 */
export async function collectObjects(source: TreeSource, database: string): Promise<ObjectNode[]> {
  const root = (await databaseNodes(source)).find((node) => databaseName(node) === database);
  if (!root) return [];
  const found: ObjectNode[] = [];
  const visit = async (parent: ObjectNode, depth: number): Promise<void> => {
    for (const node of await allChildren(source, parent.key)) {
      if (RELATION_KINDS.has(node.kind)) found.push(node);
      else if (node.expandable && depth < 3 && CONTAINER_KINDS.has(node.kind)) await visit(node, depth + 1);
    }
  };
  await visit(root, 0);
  return found;
}

/** Database names of a connection, from `tree_children`. */
export async function listDatabases(connectionId: string): Promise<string[]> {
  return (await databaseNodes(bindConnection(connectionId))).map(databaseName);
}

/** Relation-kind nodes (tables, views, collections) of one database, from `tree_children`. */
export function listObjects(connectionId: string, database: string): Promise<ObjectNode[]> {
  return collectObjects(bindConnection(connectionId), database);
}
