/**
 * Typed calls of the `mongo.*` operations (src-tauri/src/engines/mongo/ops.rs) over the injected `ConnectionApi`. Every
 * call goes through the gateway, which classifies it; `find` and `count` go through `withConfirmation`, so a workspace
 * that asks before reads still asks (null: the user declined). `distinct` serves completion and never asks: it
 * resolves [] instead of failing. `resolveRef` asks only when the user opened the reference (probing never asks). The
 * bulk operations and `applyEdits` (saving pending edits) always go through `withConfirmation`. `aggregatePreview` asks
 * only when the user ran it (a preview after an edit never asks); `aggregate` (the full run, `$out` / `$merge` included)
 * asks per the rules. `schema` asks only when the user pressed Analyze. `indexes` asks only when the user pressed Refresh;
 * creating, hiding and dropping an index always go through `withConfirmation` (DDL / destructive per the rules).
 * `validation` asks only when the user pressed Refresh; `validationCheck` and `setValidation` (DDL) always go through
 * `withConfirmation`.
 */
import type { ConnectionApi } from "../../core/connectionApi";
import type { ApplyResult, RowChangeSet } from "../../core/contract";
import type { Confirmation } from "../../core/types";
import { withConfirmation } from "../../features/safety/confirmationDialog";
import type { deleteArgs, PreviewResult, previewArgs, updateArgs } from "./bulkModel";
import type { CreateIndexArgs, IndexesResult } from "./indexModel";
import type { CountArgs, FindArgs, FindPage, Target } from "./queryModel";
import type { SchemaArgs, SchemaResult } from "./schemaModel";
import type { SetValidationArgs, SetValidationResult, ValidationCheckArgs, ValidationCheckResult } from "./validationModel";

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

export interface UpdateManyResult {
  matched: number;
  modified: number;
  elapsedMs: number;
}

export interface DeleteManyResult {
  deleted: number;
  elapsedMs: number;
}

export interface CreateIndexResult {
  name: string;
  created: boolean;
  statement: string;
  elapsedMs: number;
}

export interface HideIndexResult {
  name: string;
  hidden: boolean;
  changed: boolean;
  statement: string;
}

export interface DistinctResult {
  values: unknown[];
  truncated: boolean;
  elapsedMs: number;
}

/** One stage of `mongo.aggregatePreview`: its output on the sample (count, first documents) or the error that stopped it. */
export interface PreviewStage {
  index: number;
  operator: string;
  count: number | null;
  elapsedMs: number;
  documents: Record<string, unknown>[];
  error: string | null;
}

export interface AggregatePreviewArgs extends Target {
  pipeline: unknown[];
  upTo?: number;
  sampleSize?: number;
  sampleMode?: "first" | "random";
  limit?: number;
  maxTimeMS?: number;
}

export interface AggregatePreviewResult {
  stages: PreviewStage[];
  sample: { mode: "first" | "random"; size: number; count: number };
  elapsedMs: number;
}

export interface AggregateArgs extends Target {
  pipeline: unknown[];
  limit?: number;
  allowDiskUse?: boolean;
  maxTimeMS?: number;
}

export interface AggregateResult {
  documents: Record<string, unknown>[];
  hasMore: boolean;
  elapsedMs: number;
  statement: string;
  /** The `$out` / `$merge` stage and the collection it wrote; null for a pipeline that returns documents. */
  wrote: { stage: string; target: string | null } | null;
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
   * Each stage's output on a bounded sample (reads only). `ask`: the user ran it, so a workspace that asks before reads
   * asks (null: declined); without it a read that needs asking fails (a preview after an edit).
   */
  aggregatePreview(args: AggregatePreviewArgs, ask: boolean): Promise<AggregatePreviewResult | null> {
    const call = (confirmation: Confirmation | null) => this.connection.invokeCapability<AggregatePreviewResult>("mongo.aggregatePreview", args, confirmation);
    return ask ? withConfirmation(call) : call(null);
  }

  /** The full run; with `$out` / `$merge` the gateway asks per the rules (null: declined). */
  aggregate(args: AggregateArgs): Promise<AggregateResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<AggregateResult>("mongo.aggregate", args, confirmation));
  }

  /**
   * The shape of a sample of the collection (`mongo.schema`, reads only). `ask`: the user pressed Analyze, so a workspace
   * that asks before reads asks (null: declined); without it a read that needs asking fails (the first automatic run).
   */
  schema(args: SchemaArgs, ask: boolean): Promise<SchemaResult | null> {
    const call = (confirmation: Confirmation | null) => this.connection.invokeCapability<SchemaResult>("mongo.schema", args, confirmation);
    return ask ? withConfirmation(call) : call(null);
  }

  /**
   * The collection's indexes with options, sizes and usage (`mongo.indexes`, reads only). `ask`: the user pressed Refresh,
   * so a workspace that asks before reads asks (null: declined); without it a read that needs asking fails.
   */
  indexes(ask: boolean): Promise<IndexesResult | null> {
    const call = (confirmation: Confirmation | null) => this.connection.invokeCapability<IndexesResult>("mongo.indexes", { ...this.target }, confirmation);
    return ask ? withConfirmation(call) : call(null);
  }

  /** Builds an index (DDL: the gateway asks per the rules; null: declined). */
  createIndex(args: CreateIndexArgs): Promise<CreateIndexResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<CreateIndexResult>("mongo.createIndex", args, confirmation));
  }

  /** Hides an index from the planner or shows it again (DDL, `collMod`). */
  setIndexHidden(name: string, hidden: boolean): Promise<HideIndexResult | null> {
    const operation = hidden ? "mongo.hideIndex" : "mongo.unhideIndex";
    return withConfirmation((confirmation) => this.connection.invokeCapability<HideIndexResult>(operation, { ...this.target, name }, confirmation));
  }

  /** Drops an index (destructive: the gateway asks, typed in Guarded workspaces). */
  dropIndex(name: string): Promise<{ name: string; statement: string } | null> {
    return withConfirmation((confirmation) =>
      this.connection.invokeCapability<{ name: string; statement: string }>("mongo.dropIndex", { ...this.target, name }, confirmation),
    );
  }

  /**
   * The collection's validator, level and action (`mongo.validation`, reads only; raw: `readRules` checks it). `ask`: the
   * user pressed Refresh, so a workspace that asks before reads asks (null: declined).
   */
  validation(ask: boolean): Promise<unknown> {
    const call = (confirmation: Confirmation | null) => this.connection.invokeCapability<unknown>("mongo.validation", { ...this.target }, confirmation);
    return ask ? withConfirmation(call) : call(null);
  }

  /** How many existing documents a validator refuses, with a few of them (reads only; the user pressed Check). */
  validationCheck(args: ValidationCheckArgs): Promise<ValidationCheckResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<ValidationCheckResult>("mongo.validationCheck", args, confirmation));
  }

  /** Applies a validator, level or action (DDL, `collMod`: the gateway asks per the rules; null: declined). */
  setValidation(args: SetValidationArgs): Promise<SetValidationResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<SetValidationResult>("mongo.setValidation", args, confirmation));
  }

  /** The match count and a before/after sample of an update (reads only). */
  updatePreview(args: ReturnType<typeof previewArgs>): Promise<PreviewResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<PreviewResult>("mongo.updatePreview", args, confirmation));
  }

  /** Updates every matching document; the gateway asks per the rules (null: declined). */
  updateMany(args: ReturnType<typeof updateArgs>): Promise<UpdateManyResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<UpdateManyResult>("mongo.updateMany", args, confirmation));
  }

  /** Deletes every matching document (destructive: the gateway asks, typed in Guarded workspaces). */
  deleteMany(args: ReturnType<typeof deleteArgs>): Promise<DeleteManyResult | null> {
    return withConfirmation((confirmation) => this.connection.invokeCapability<DeleteManyResult>("mongo.deleteMany", args, confirmation));
  }

  /** Saves pending document edits through `apply_edits` (grid save rules: Read-only blocks, confirmations per the rules). */
  applyEdits(set: RowChangeSet): Promise<ApplyResult | null> {
    return withConfirmation((confirmation) => this.connection.applyEdits(set, "gridSave", confirmation));
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
