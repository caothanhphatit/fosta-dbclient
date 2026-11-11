/**
 * Typed calls of the `mongo.*` operations (src-tauri/src/engines/mongo/ops.rs) over the injected `ConnectionApi`. Every
 * call goes through the gateway, which classifies it; `find` and `count` go through `withConfirmation`, so a workspace
 * that asks before reads still asks (null: the user declined). `distinct` serves completion and never asks: it
 * resolves [] instead of failing. `resolveRef` asks only when the user opened the reference (probing never asks).
 */
import type { ConnectionApi } from "../../core/connectionApi";
import type { Confirmation } from "../../core/types";
import { withConfirmation } from "../../features/safety/confirmationDialog";
import type { CountArgs, FindArgs, FindPage, Target } from "./queryModel";

export interface FindResult extends FindPage {
  documents: Record<string, unknown>[];
  sort: Record<string, unknown> | null;
  elapsedMs: number;
  statement: string;
}

export interface CountResult {
  count: number;
  estimated: boolean;
  elapsedMs: number;
}

export interface DistinctResult {
  values: unknown[];
  truncated: boolean;
  elapsedMs: number;
}

/** Values asked for value completion, and how long they are kept per field. */
const DISTINCT_LIMIT = 50;
const DISTINCT_TTL_MS = 60_000;
/** The bound of a completion request: a slow `distinct` must not hold the editor. */
const DISTINCT_MAX_TIME_MS = 3000;
/** The bound of reading one referenced document. */
const RESOLVE_MAX_TIME_MS = 5000;

interface ResolveResult {
  document: Record<string, unknown> | null;
}

export class MongoApi {
  private readonly distinctCache = new Map<string, { at: number; values: Promise<unknown[]> }>();

  constructor(
    private readonly connection: ConnectionApi,
    readonly target: Target,
  ) {}

  find(args: FindArgs): Promise<FindResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<FindResult>("mongo.find", args, confirmation));
  }

  count(args: CountArgs): Promise<CountResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<CountResult>("mongo.count", args, confirmation));
  }

  /**
   * The document with `_id` `id` in `collection` of the same database; null when there is none. `ask`: a workspace that
   * asks before reads asks (the user opened it); without it a read that needs asking fails (probing references).
   */
  async resolveRef(collection: string, id: unknown, ask: boolean): Promise<Record<string, unknown> | null> {
    const args = { database: this.target.database, collection, id, maxTimeMS: RESOLVE_MAX_TIME_MS };
    const call = (confirmation: Confirmation | null) => this.connection.invokeCapability<ResolveResult>("mongo.resolveRef", args, confirmation);
    const result = ask ? await withConfirmation(call) : await call(null);
    return result?.document ?? null;
  }

  /** Whether `distinct(field)` answers from the cache (no request, so no need to wait for typing to settle). */
  hasDistinct(field: string): boolean {
    const cached = this.distinctCache.get(field);
    return !!cached && Date.now() - cached.at < DISTINCT_TTL_MS;
  }

  /** Up to 50 distinct values of `field` (cached for a minute per field); [] when they cannot be read. */
  distinct(field: string): Promise<unknown[]> {
    const cached = this.distinctCache.get(field);
    if (cached && Date.now() - cached.at < DISTINCT_TTL_MS) return cached.values;
    const values = this.connection
      .invokeCapability<DistinctResult>("mongo.distinct", { ...this.target, field, limit: DISTINCT_LIMIT, maxTimeMS: DISTINCT_MAX_TIME_MS })
      .then((result) => (Array.isArray(result?.values) ? result.values : []))
      .catch(() => {
        // Not kept: the next request tries again.
        this.distinctCache.delete(field);
        return [] as unknown[];
      });
    this.distinctCache.set(field, { at: Date.now(), values });
    return values;
  }
}
