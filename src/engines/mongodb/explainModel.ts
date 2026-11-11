/**
 * The Explain section's model, without DOM: the `mongo.explain` arguments of the Documents query or the pipeline, the
 * result types (the raw answer and the normalised plan tree of src-tauri/src/engines/mongo/explain.rs), the warnings a
 * plan earns (collection scan, in-memory sort, many documents examined per document returned, filtered fields no index
 * serves), the index a query shape suggests by the ESR rule (equality fields, then sort fields, then range fields)
 * unless an index already covers it, the saved state and the summary the assistant reads.
 */
import type { Doc, Json } from "./documents";
import type { IndexInfo, IndexKey } from "./indexModel";
import { parseStage, WRITE_OPERATORS } from "./pipelineModel";
import { buildQuery, oneLine, type QueryState, type Target } from "./queryModel";

export type Verbosity = "queryPlanner" | "executionStats" | "allPlansExecution";
export type ExplainSource = "query" | "pipeline";

export const VERBOSITIES: readonly { id: Verbosity; label: string; title: string }[] = [
  { id: "queryPlanner", label: "Query planner", title: "The chosen plan only; the query does not run" },
  { id: "executionStats", label: "Execution stats", title: "Runs the query (within the workspace's timeout) and counts what each stage did" },
  { id: "allPlansExecution", label: "All plans", title: "Execution stats plus the trial run of every candidate plan" },
];

export interface PlanNode {
  stage: string;
  indexName?: string;
  /** A key document; MongoDB 8 express plans give it as text (`"{ _id: 1 }"`). */
  keyPattern?: Record<string, Json> | string;
  direction?: string;
  bounds?: Json;
  isMultiKey?: boolean;
  filter?: Json;
  /** A pipeline stage's specification, or a shard's host. */
  details?: Json;
  shard?: string;
  nReturned?: number;
  executionTimeMillisEstimate?: number;
  totalKeysExamined?: number;
  totalDocsExamined?: number;
  children: PlanNode[];
}

export interface PlanSummary {
  nReturned: number | null;
  keysExamined: number | null;
  docsExamined: number | null;
  executionTimeMillis: number | null;
  isCollScan: boolean;
  usedIndexes: string[];
  sortInMemory: boolean;
  fetchRatio: number | null;
}

export interface NormalisedPlan {
  shape: "find" | "aggregate" | "sharded";
  explainVersion: string | null;
  winningPlan: PlanNode;
  rejectedPlans: { shard: string | null; plan: PlanNode }[];
  summary: PlanSummary;
}

export interface ExplainResult {
  explain: Json;
  /** Null when the answer has a shape the backend does not know (the raw answer still shows it). */
  plan: NormalisedPlan | null;
  verbosity: Verbosity;
  statement: string;
  elapsedMs: number;
}

export interface ExplainArgs extends Target {
  kind: "find" | "aggregate";
  verbosity: Verbosity;
  filter?: string;
  projection?: string;
  sort?: string;
  skip?: number;
  limit?: number;
  collation?: string;
  pipeline?: Doc[];
  maxTimeMS?: number;
}

/** The store key of the Aggregations section's enabled stage texts: a function, so they are read as they are now. */
export const STORE_PIPELINE = "mongo.pipeline";

/** What another section hands the Explain section: the query (as typed) or the pipeline's stage texts to explain. */
export interface ExplainRequest {
  explain: { source: ExplainSource; query?: QueryState; stages?: string[] };
}

const isRecord = (value: unknown): value is Record<string, Json> => typeof value === "object" && value !== null && !Array.isArray(value);

export function readExplainRequest(request: unknown): ExplainRequest["explain"] | null {
  if (!isRecord(request) || !isRecord(request.explain)) return null;
  const { source, query, stages } = request.explain as { source?: unknown; query?: unknown; stages?: unknown };
  if (source !== "query" && source !== "pipeline") return null;
  return {
    source,
    query: isRecord(query) ? (query as unknown as QueryState) : undefined,
    stages: Array.isArray(stages) ? stages.filter((stage): stage is string => typeof stage === "string") : undefined,
  };
}

// --- Arguments -----------------------------------------------------------------------------------------------------------

export type BuiltExplain = { ok: true; args: ExplainArgs; text: string } | { ok: false; error: string };

/** The explain of the Documents query (fields as typed, the page's limit and skip). */
export function findExplainArgs(state: QueryState, target: Target, verbosity: Verbosity): BuiltExplain {
  const built = buildQuery(state, target);
  if (!built.ok) return { ok: false, error: `The query does not parse (${built.error.field}): ${built.error.message}` };
  const find = built.query.find;
  const args: ExplainArgs = { ...target, kind: "find", verbosity };
  for (const key of ["filter", "projection", "sort", "collation"] as const) if (find[key]) args[key] = find[key];
  if (find.skip) args.skip = find.skip;
  if (find.limit) args.limit = find.limit;
  const parts = [`find(${oneLine(state.filter.trim()) || "{}"})`];
  if (state.sort.trim()) parts.push(`sort(${oneLine(state.sort.trim())})`);
  if (args.skip) parts.push(`skip(${args.skip})`);
  if (args.limit) parts.push(`limit(${args.limit})`);
  return { ok: true, args, text: parts.join(".") };
}

/** The explain of the pipeline's enabled stage texts; `$out` / `$merge` are refused (an explain only reads). */
export function pipelineExplainArgs(texts: readonly string[], target: Target, verbosity: Verbosity): BuiltExplain {
  if (texts.length === 0) return { ok: false, error: "The pipeline has no enabled stage: add one in Aggregations." };
  const pipeline: Doc[] = [];
  for (const [index, text] of texts.entries()) {
    const parsed = parseStage(text);
    if (!parsed.ok) return { ok: false, error: `Stage ${index + 1} does not parse: ${parsed.error}` };
    if (WRITE_OPERATORS.has(parsed.operator))
      return { ok: false, error: `Stage ${index + 1} (${parsed.operator}) writes into a collection; an explain only reads, so turn it off first.` };
    pipeline.push(parsed.value);
  }
  const operators = pipeline.map((stage) => Object.keys(stage)[0]);
  return { ok: true, args: { ...target, kind: "aggregate", verbosity, pipeline }, text: `aggregate([${operators.join(", ")}])` };
}

// --- The query's shape (ESR) ---------------------------------------------------------------------------------------------

export interface SortField {
  field: string;
  direction: 1 | -1;
}

/** The fields a query filters and sorts on, by how an index serves them. */
export interface QueryShape {
  equality: string[];
  sort: SortField[];
  range: string[];
  /** `$or` / `$nor` at the top: each branch would need its own index, so no single index is suggested. */
  hasOr: boolean;
}

/** Extended JSON type wrappers: a value, compared for equality. */
const EJSON_VALUES = new Set([
  "$oid",
  "$date",
  "$numberLong",
  "$numberInt",
  "$numberDouble",
  "$numberDecimal",
  "$binary",
  "$uuid",
  "$timestamp",
  "$symbol",
  "$minKey",
  "$maxKey",
]);

type Kind = "equality" | "range" | null;

function conditionKind(value: Json, sorted: boolean): Kind {
  if (!isRecord(value)) return "equality";
  const keys = Object.keys(value);
  if (keys.length === 0 || !keys.some((key) => key.startsWith("$"))) return "equality";
  if (keys.length === 1 && EJSON_VALUES.has(keys[0])) return "equality";
  if (keys.every((key) => key === "$eq")) return "equality";
  // `$in` is a set of equalities; with a sort the index returns several runs to merge, so it ranks with the ranges.
  if (keys.every((key) => key === "$in")) return sorted ? "range" : "equality";
  if (keys.includes("$text") || keys.includes("$near") || keys.includes("$nearSphere") || keys.includes("$geoWithin")) return null;
  return "range";
}

export function queryShape(filter: Json, sort: Json): QueryShape {
  const sortFields: SortField[] = [];
  if (isRecord(sort)) {
    for (const [field, direction] of Object.entries(sort)) {
      const n = typeof direction === "number" ? direction : Number(isRecord(direction) ? (direction.$numberInt ?? direction.$numberLong ?? NaN) : NaN);
      if (n === 1 || n === -1) sortFields.push({ field, direction: n });
    }
  }
  const equality = new Set<string>();
  const range = new Set<string>();
  let hasOr = false;
  const visit = (document: Json) => {
    if (!isRecord(document)) return;
    for (const [key, value] of Object.entries(document)) {
      if (key === "$and" && Array.isArray(value)) value.forEach(visit);
      else if (key === "$or" || key === "$nor") hasOr = true;
      else if (key.startsWith("$")) continue;
      else {
        const kind = conditionKind(value, sortFields.length > 0);
        if (kind === "equality") equality.add(key);
        else if (kind === "range") range.add(key);
      }
    }
  };
  visit(filter);
  for (const field of equality) range.delete(field);
  return { equality: [...equality], sort: sortFields, range: [...range], hasOr };
}

function parsedJson(text: string | undefined): Json {
  if (!text) return null;
  try {
    return JSON.parse(text) as Json;
  } catch {
    return null;
  }
}

/** The shape of an explained find, or of a pipeline's leading `$match` stages and the `$sort` right after them. */
export function shapeOfArgs(args: ExplainArgs): QueryShape {
  if (args.kind === "find") return queryShape(parsedJson(args.filter), parsedJson(args.sort));
  const matches: Json[] = [];
  let sort: Json = null;
  for (const stage of args.pipeline ?? []) {
    if (isRecord(stage.$match) && sort === null) matches.push(stage.$match);
    else if (isRecord(stage.$sort) && sort === null) sort = stage.$sort;
    else break;
  }
  return queryShape(matches.length === 1 ? matches[0] : { $and: matches }, sort);
}

export function shapeFields(shape: QueryShape): string[] {
  return [...shape.equality, ...shape.range];
}

// --- Index suggestion ----------------------------------------------------------------------------------------------------

export interface IndexSuggestion {
  keys: { field: string; kind: 1 | -1 }[];
  /** Why the keys are in this order. */
  reason: string;
  /** An existing index that already serves the shape (no suggestion then), or null. */
  coveredBy: string | null;
  /** A hidden index with these keys: unhiding it is cheaper than building another. */
  hidden: string | null;
}

const sameSet = (a: readonly string[], b: readonly string[]) => a.length === b.length && a.every((item) => b.includes(item));

/** Whether an index's keys start with the shape's equality fields (any order), its sort fields (in order, all in the same or all in the opposite direction) and its range fields (any order). */
export function indexCovers(keys: readonly IndexKey[], shape: QueryShape): boolean {
  if (keys.some((key) => typeof key.kind !== "number")) return false;
  const fields = keys.map((key) => key.field);
  let at = 0;
  const take = (n: number) => {
    const part = keys.slice(at, at + n);
    at += n;
    return part;
  };
  if (fields.length < shape.equality.length + shape.sort.length + shape.range.length) return false;
  if (
    !sameSet(
      take(shape.equality.length).map((key) => key.field),
      shape.equality,
    )
  )
    return false;
  const sorted = take(shape.sort.length);
  if (sorted.some((key, i) => key.field !== shape.sort[i].field)) return false;
  if (sorted.length > 0) {
    const same = sorted.every((key, i) => key.kind === shape.sort[i].direction);
    const opposite = sorted.every((key, i) => key.kind === -shape.sort[i].direction);
    if (!same && !opposite) return false;
  }
  return sameSet(
    take(shape.range.length).map((key) => key.field),
    shape.range,
  );
}

/**
 * The index the shape asks for by the ESR rule, with the existing index that already covers it (then the caller shows
 * no suggestion) or the hidden one that would. Null when the query filters and sorts on nothing an index could serve
 * (or on `$or` branches).
 */
export function suggestIndex(shape: QueryShape, indexes: readonly IndexInfo[] | null): IndexSuggestion | null {
  if (shape.hasOr) return null;
  const sortFields = shape.sort.filter((sort) => !shape.equality.includes(sort.field));
  const effective: QueryShape = {
    equality: shape.equality,
    sort: sortFields,
    range: shape.range.filter((field) => !sortFields.some((sort) => sort.field === field)),
    hasOr: false,
  };
  const keys = [
    ...effective.equality.map((field) => ({ field, kind: 1 as const })),
    ...effective.sort.map((sort) => ({ field: sort.field, kind: sort.direction })),
    ...effective.range.map((field) => ({ field, kind: 1 as const })),
  ];
  if (keys.length === 0) return null;
  if (keys.length === 1 && keys[0].field === "_id") return { keys, reason: "The _id index serves it.", coveredBy: "_id_", hidden: null };
  const parts: string[] = [];
  if (effective.equality.length) parts.push(`equality on ${effective.equality.join(", ")} first`);
  if (effective.sort.length) parts.push(`then the sort ${effective.sort.map((sort) => `${sort.field} ${sort.direction === 1 ? "↑" : "↓"}`).join(", ")}`);
  if (effective.range.length) parts.push(`then the range on ${effective.range.join(", ")}`);
  const reason = `${parts.join(", ")} (Equality, Sort, Range).`;
  const usable = (indexes ?? []).filter((index) => !isRecord(index.options.partialFilterExpression));
  const covering = usable.find((index) => index.options.hidden !== true && indexCovers(index.keys, effective));
  const hidden = covering ? null : (usable.find((index) => index.options.hidden === true && indexCovers(index.keys, effective))?.name ?? null);
  return { keys, reason: reason.charAt(0).toUpperCase() + reason.slice(1), coveredBy: covering?.name ?? null, hidden };
}

/** The request the Indexes section opens prefilled (`receive`). */
export function createIndexRequest(suggestion: IndexSuggestion): { createIndex: { keys: Record<string, 1 | -1>; options: Record<string, Json> } } {
  return { createIndex: { keys: Object.fromEntries(suggestion.keys.map((key) => [key.field, key.kind])), options: {} } };
}

export function keysText(keys: readonly { field: string; kind: 1 | -1 }[]): string {
  return `{ ${keys.map((key) => `${/^[A-Za-z_$][\w$]*$/.test(key.field) ? key.field : JSON.stringify(key.field)}: ${key.kind}`).join(", ")} }`;
}

// --- Walking the plan ---------------------------------------------------------------------------------------------------

export function walkPlan(node: PlanNode, visit: (node: PlanNode, depth: number) => void, depth = 0): void {
  visit(node, depth);
  for (const child of node.children ?? []) walkPlan(child, visit, depth + 1);
}

/** The fields of a key pattern, given as a document or as the text of one. */
export function keyPatternFields(pattern: PlanNode["keyPattern"]): string[] {
  if (isRecord(pattern)) return Object.keys(pattern);
  if (typeof pattern !== "string") return [];
  return [...pattern.matchAll(/["']?([\w.$]+)["']?\s*:/g)].map((match) => match[1]);
}

/** The fields of the indexes the winning plan reads. */
export function indexedFields(plan: NormalisedPlan): Set<string> {
  const fields = new Set<string>();
  walkPlan(plan.winningPlan, (node) => keyPatternFields(node.keyPattern).forEach((field) => fields.add(field)));
  return fields;
}

/** What a stage examined: keys or documents, whichever is more; null without counters. */
export function examinedOf(node: PlanNode): number | null {
  const counts = [node.totalKeysExamined, node.totalDocsExamined].filter((n): n is number => typeof n === "number");
  return counts.length ? Math.max(...counts) : null;
}

/** The share of the examined documents or keys a stage returned (0..1), null when it cannot be told. */
export function returnedShare(node: PlanNode): number | null {
  const examined = examinedOf(node);
  if (examined === null || typeof node.nReturned !== "number") return null;
  if (examined === 0) return 1;
  return Math.max(0, Math.min(1, node.nReturned / examined));
}

// --- Warnings -----------------------------------------------------------------------------------------------------------

export interface PlanWarning {
  id: "collscan" | "sort" | "ratio" | "unindexed" | "unknown";
  tone: "warn" | "info";
  title: string;
  detail: string;
}

/** Documents examined per document returned from which the plan is flagged, and the fewest examined that counts. */
export const RATIO_WARN = 10;
const RATIO_MIN_EXAMINED = 100;

export function planWarnings(plan: NormalisedPlan | null, shape: QueryShape | null, format: (n: number) => string = String): PlanWarning[] {
  if (!plan)
    return [{ id: "unknown", tone: "info", title: "Plan not read", detail: "This server's answer has a shape the plan view does not know; see Raw JSON." }];
  const summary = plan.summary;
  const warnings: PlanWarning[] = [];
  const filtered = shape ? shapeFields(shape).length > 0 || shape.sort.length > 0 : true;
  if (summary.isCollScan)
    warnings.push(
      filtered
        ? { id: "collscan", tone: "warn", title: "Collection scan", detail: "Every document is read (COLLSCAN): no index serves this query." }
        : { id: "collscan", tone: "info", title: "Collection scan", detail: "No filter or sort: reading the documents in natural order is expected." },
    );
  if (summary.sortInMemory)
    warnings.push({
      id: "sort",
      tone: "warn",
      title: "In-memory sort",
      detail:
        "The documents are sorted after they are read (a blocking SORT stage, limited to 100 MB without disk use); an index on the sort fields returns them in order.",
    });
  if (summary.fetchRatio !== null && summary.fetchRatio >= RATIO_WARN && (summary.docsExamined ?? 0) >= RATIO_MIN_EXAMINED)
    warnings.push({
      id: "ratio",
      tone: "warn",
      title: `${format(Math.round(summary.fetchRatio))}× documents examined`,
      detail: `${format(summary.docsExamined ?? 0)} documents examined for ${format(summary.nReturned ?? 0)} returned: the index (if any) narrows the search too little.`,
    });
  if (shape) {
    const indexed = indexedFields(plan);
    const missing = shapeFields(shape).filter((field) => !indexed.has(field));
    if (missing.length > 0)
      warnings.push({
        id: "unindexed",
        tone: "warn",
        title: missing.length === 1 ? "Filtered field without an index" : "Filtered fields without an index",
        detail: `No index the plan uses holds ${missing.join(", ")}: the server compares ${missing.length === 1 ? "it" : "them"} document by document.`,
      });
  }
  return warnings;
}

// --- Saved state, assistant ----------------------------------------------------------------------------------------------

export interface ExplainSavedState {
  source: ExplainSource;
  verbosity: Verbosity;
}

export const DEFAULT_EXPLAIN_STATE: ExplainSavedState = { source: "query", verbosity: "executionStats" };

export function readExplainState(value: unknown): ExplainSavedState {
  if (!isRecord(value)) return { ...DEFAULT_EXPLAIN_STATE };
  return {
    source: value.source === "pipeline" ? "pipeline" : "query",
    verbosity: VERBOSITIES.some((item) => item.id === value.verbosity) ? (value.verbosity as Verbosity) : DEFAULT_EXPLAIN_STATE.verbosity,
  };
}

/** The plan in a few lines for the assistant: the stages, the counters, the warnings and the suggestion (no documents). */
export function explainSummary(result: ExplainResult, what: string, warnings: readonly PlanWarning[], suggestion: IndexSuggestion | null): string {
  const lines = [`Explain (${result.verbosity}) of ${what}:`];
  const plan = result.plan;
  if (!plan) lines.push("The plan's shape is not known; only the raw answer is shown.");
  else {
    const stages: string[] = [];
    walkPlan(plan.winningPlan, (node, depth) => {
      if (stages.length < 30)
        stages.push(`${"  ".repeat(depth)}${node.stage}${node.indexName ? ` ${node.indexName}` : ""}${node.shard ? ` (${node.shard})` : ""}`);
    });
    lines.push(`Winning plan (root first):\n${stages.join("\n")}`);
    const s = plan.summary;
    const counters = [
      s.nReturned !== null ? `returned ${s.nReturned}` : null,
      s.keysExamined !== null ? `keys examined ${s.keysExamined}` : null,
      s.docsExamined !== null ? `documents examined ${s.docsExamined}` : null,
      s.executionTimeMillis !== null ? `${s.executionTimeMillis} ms` : null,
    ].filter(Boolean);
    if (counters.length) lines.push(`Counters: ${counters.join(", ")}.`);
    lines.push(`Indexes used: ${s.usedIndexes.length ? s.usedIndexes.join(", ") : "none"}; rejected plans: ${plan.rejectedPlans.length}.`);
  }
  for (const warning of warnings) lines.push(`Warning: ${warning.title}. ${warning.detail}`);
  if (suggestion && !suggestion.coveredBy) lines.push(`Suggested index: ${keysText(suggestion.keys)} (${suggestion.reason})`);
  else if (suggestion?.coveredBy) lines.push(`Index ${suggestion.coveredBy} already covers the filter and sort.`);
  return lines.join("\n");
}
