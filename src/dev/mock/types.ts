/**
 * The shape every engine mock in `./engines/*.ts` exports (collected with `import.meta.glob`). Planned engines export
 * only `descriptor` and `order`; the ready ones implement the rest on an in-memory catalog (`catalog.ts`).
 */
import type {
  ApplyResult,
  BrowseRequest,
  ConnectInfo,
  EngineDescriptor,
  ObjectDescription,
  ObjectRef,
  Page,
  RowChangeSet,
  RunRequest,
  StreamEvent,
  TransactionState,
  TreeChildren,
  TreeRequest,
} from "../../core/contract";
import type { CompletionSchema } from "../../core/api";
import type { ConnectionProfile, SchemaGraph, StatementInfo } from "../../core/types";

/** A query session of the mock (`open_session`). */
export interface MockSession {
  sessionId: string;
  connectionId: string;
  language: string;
  database: string | null;
  transaction: TransactionState;
  /** Aborted by `cancel_run` / `cancel_sql`. */
  running: AbortController | null;
}

export interface RunContext {
  session: MockSession;
  signal: AbortSignal;
}

export interface EngineMock {
  descriptor: EngineDescriptor;
  /** Position in `engine_descriptors` (the backend's registry order). */
  order: number;
  connectInfo?(profile: ConnectionProfile): ConnectInfo;
  tree?(parent: TreeRequest["parent"]): TreeChildren["nodes"];
  describe?(object: ObjectRef): ObjectDescription;
  browse?(request: BrowseRequest): Page;
  apply?(changes: RowChangeSet): ApplyResult;
  /** Streams the run's events; resolves with the number of statements run. */
  run?(request: RunRequest, emit: (event: StreamEvent) => void, context: RunContext): Promise<number>;
  /** How the gateway sees a run's text (statement categories). */
  classify?(text: string): StatementInfo[];
  /** Engine operations (`invoke_capability`); undefined for an unknown name. */
  invoke?(name: string, args: unknown): unknown;
  schemaGraph?(database: string): SchemaGraph;
  completion?(database: string | null): CompletionSchema;
  createDatabase?(name: string, firstCollection: string | null): void;
  /** Engine-specific backend commands (Oracle's `oracle_client_*`); `emit` streams events to a channel argument. */
  commands?: Record<string, (payload: Record<string, unknown>, emit: (event: unknown) => void) => unknown>;
  /** Mock-only: "someone else" edits (or deletes) a row, to provoke a `CONFLICT` on the next save. */
  touchRow?(table: string, key: unknown, remove: boolean, column?: string): string;
}
