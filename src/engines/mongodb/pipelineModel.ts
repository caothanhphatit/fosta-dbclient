/**
 * The aggregation pipeline of the Aggregations section, without DOM: stages as typed (mongosh syntax, one stage
 * document each) with their switch and fold, the edits the stage cards make (add, insert, duplicate, delete, move,
 * toggle, change the operator), each stage parsed and checked to be one stage operator, and the plan a preview runs:
 * the enabled stages in order, stopped before a stage that does not parse or writes (`$out` / `$merge` run only with
 * Run). Also the catalog of stage operators with a description and a template each, the stages a Documents query turns
 * into, the saved state and the field paths and types sampled at a stage.
 */
import { parseMongoShell } from "../../lang/mongo/shell";
import { BSON_TYPE_LABELS, bsonType } from "./bson";
import { isWrapper, type Doc, type Json } from "./documents";
import { braced, DEFAULT_LIMIT, type QueryState } from "./queryModel";

export interface PipelineStage {
  /** Unique within the pipeline (cards and drag-and-drop find stages by it); not saved. */
  id: string;
  /** The operator picked (`$match`); the text's own operator wins once it parses. */
  operator: string;
  /** The stage document as typed, mongosh syntax (`{ $match: { status: 'paid' } }`). */
  text: string;
  enabled: boolean;
  collapsed: boolean;
}

export interface StageSpec {
  operator: string;
  description: string;
  template: string;
  /** Writes the result into a collection: never previewed, only run (the gateway asks). */
  writes?: boolean;
}

/** The stage operators the picker offers, the common ones first. */
export const STAGE_SPECS: readonly StageSpec[] = [
  { operator: "$match", description: "Keep the documents that match a query", template: "{\n  $match: {\n    \n  }\n}" },
  { operator: "$project", description: "Include, exclude or compute fields", template: "{\n  $project: {\n    _id: 1\n  }\n}" },
  { operator: "$addFields", description: "Add computed fields", template: '{\n  $addFields: {\n    newField: "$field"\n  }\n}' },
  { operator: "$set", description: "Add or replace fields (alias of $addFields)", template: '{\n  $set: {\n    field: "$other"\n  }\n}' },
  { operator: "$unset", description: "Remove fields", template: '{\n  $unset: ["field"]\n}' },
  {
    operator: "$group",
    description: "Group by a key and accumulate ($sum, $avg, $push…)",
    template: '{\n  $group: {\n    _id: "$field",\n    count: { $sum: 1 }\n  }\n}',
  },
  { operator: "$sort", description: "Sort by fields (1 ascending, -1 descending)", template: "{\n  $sort: {\n    field: -1\n  }\n}" },
  { operator: "$limit", description: "Keep the first n documents", template: "{\n  $limit: 10\n}" },
  { operator: "$skip", description: "Skip the first n documents", template: "{\n  $skip: 10\n}" },
  {
    operator: "$unwind",
    description: "One document per element of an array",
    template: '{\n  $unwind: {\n    path: "$items",\n    preserveNullAndEmptyArrays: false\n  }\n}',
  },
  {
    operator: "$lookup",
    description: "Join documents of another collection into an array",
    template: '{\n  $lookup: {\n    from: "collection",\n    localField: "field",\n    foreignField: "_id",\n    as: "joined"\n  }\n}',
  },
  { operator: "$count", description: "Count the documents into one field", template: '{\n  $count: "count"\n}' },
  {
    operator: "$facet",
    description: "Several pipelines over the same input",
    template: '{\n  $facet: {\n    first: [{ $limit: 5 }],\n    total: [{ $count: "count" }]\n  }\n}',
  },
  {
    operator: "$bucket",
    description: "Group into ranges of a value",
    template: '{\n  $bucket: {\n    groupBy: "$field",\n    boundaries: [0, 100, 200],\n    default: "other",\n    output: { count: { $sum: 1 } }\n  }\n}',
  },
  {
    operator: "$bucketAuto",
    description: "Group into evenly filled ranges",
    template: '{\n  $bucketAuto: {\n    groupBy: "$field",\n    buckets: 5\n  }\n}',
  },
  { operator: "$sortByCount", description: "Group by a value, count, sort by the count", template: '{\n  $sortByCount: "$field"\n}' },
  { operator: "$replaceRoot", description: "Replace the document with an embedded one", template: '{\n  $replaceRoot: {\n    newRoot: "$field"\n  }\n}' },
  { operator: "$replaceWith", description: "Replace the document (alias of $replaceRoot)", template: '{\n  $replaceWith: "$field"\n}' },
  { operator: "$sample", description: "A random sample of n documents", template: "{\n  $sample: {\n    size: 10\n  }\n}" },
  {
    operator: "$unionWith",
    description: "Append the documents of another collection",
    template: '{\n  $unionWith: {\n    coll: "collection",\n    pipeline: []\n  }\n}',
  },
  {
    operator: "$graphLookup",
    description: "Recursive lookup (trees, graphs)",
    template:
      '{\n  $graphLookup: {\n    from: "collection",\n    startWith: "$field",\n    connectFromField: "field",\n    connectToField: "_id",\n    as: "path"\n  }\n}',
  },
  {
    operator: "$setWindowFields",
    description: "Window functions (running totals, ranks)",
    template:
      '{\n  $setWindowFields: {\n    sortBy: { field: 1 },\n    output: {\n      running: { $sum: "$field", window: { documents: ["unbounded", "current"] } }\n    }\n  }\n}',
  },
  { operator: "$redact", description: "Keep or prune content by an expression", template: '{\n  $redact: "$$KEEP"\n}' },
  { operator: "$out", description: "Write the result into a collection (replaces it)", template: '{\n  $out: "collection"\n}', writes: true },
  {
    operator: "$merge",
    description: "Merge the result into a collection",
    template: '{\n  $merge: {\n    into: "collection",\n    whenMatched: "merge",\n    whenNotMatched: "insert"\n  }\n}',
    writes: true,
  },
];

export const WRITE_OPERATORS: ReadonlySet<string> = new Set(STAGE_SPECS.filter((spec) => spec.writes).map((spec) => spec.operator));

export function specOf(operator: string): StageSpec | undefined {
  return STAGE_SPECS.find((spec) => spec.operator === operator);
}

/** The template of `operator`; an unknown operator gets an empty document under its name. */
export function templateOf(operator: string): string {
  return specOf(operator)?.template ?? `{\n  ${operator}: {}\n}`;
}

export function newStage(id: string, operator: string, text = templateOf(operator)): PipelineStage {
  return { id, operator, text, enabled: true, collapsed: false };
}

/** The operator a stage's text starts with (`{ $match: …` → `$match`), parsed or not; null when there is none yet. */
export function operatorOfText(text: string): string | null {
  const match = /^\s*\{\s*(?:\/\/[^\n]*\n\s*|\/\*[\s\S]*?\*\/\s*)*(["']?)(\$[A-Za-z]+)\1\s*:/.exec(text);
  return match ? match[2] : null;
}

// --- Edits (each returns a new list) -----------------------------------------------------------------------------------

/** Inserts `stage` at `at` (the end by default). */
export function insertStage(stages: readonly PipelineStage[], stage: PipelineStage, at = stages.length): PipelineStage[] {
  const index = Math.max(0, Math.min(stages.length, at));
  return [...stages.slice(0, index), stage, ...stages.slice(index)];
}

/** A copy of the stage `id` right after it, with `newId`. */
export function duplicateStage(stages: readonly PipelineStage[], id: string, newId: string): PipelineStage[] {
  const index = stages.findIndex((stage) => stage.id === id);
  if (index < 0) return [...stages];
  return insertStage(stages, { ...stages[index], id: newId, collapsed: false }, index + 1);
}

export function removeStage(stages: readonly PipelineStage[], id: string): PipelineStage[] {
  return stages.filter((stage) => stage.id !== id);
}

/** Moves the stage `id` to position `to` (clamped; positions of the list after the stage left it). */
export function moveStage(stages: readonly PipelineStage[], id: string, to: number): PipelineStage[] {
  const index = stages.findIndex((stage) => stage.id === id);
  if (index < 0) return [...stages];
  const rest = stages.filter((stage) => stage.id !== id);
  return insertStage(rest, stages[index], to);
}

/** Moves the stage `id` by `delta` places (-1 up, 1 down). */
export function moveStageBy(stages: readonly PipelineStage[], id: string, delta: number): PipelineStage[] {
  const index = stages.findIndex((stage) => stage.id === id);
  if (index < 0) return [...stages];
  return moveStage(stages, id, index + delta);
}

export function updateStage(stages: readonly PipelineStage[], id: string, change: Partial<Omit<PipelineStage, "id">>): PipelineStage[] {
  return stages.map((stage) => (stage.id === id ? { ...stage, ...change } : stage));
}

export function toggleStage(stages: readonly PipelineStage[], id: string): PipelineStage[] {
  return stages.map((stage) => (stage.id === id ? { ...stage, enabled: !stage.enabled } : stage));
}

/** What a stage's value starts with after the operator: `{` a document, `[` an array, `"` a string, `0` a number. */
function bodyKind(text: string): string | null {
  const match = /^\s*\{\s*(?:\/\/[^\n]*\n\s*|\/\*[\s\S]*?\*\/\s*)*["']?\$[A-Za-z]+["']?\s*:\s*(\S)/.exec(text);
  if (!match) return null;
  const c = match[1];
  return c === "'" || c === '"' ? '"' : /[\d-]/.test(c) ? "0" : c;
}

/**
 * The text of a stage after picking `operator`: an empty or untouched template becomes the new template; typed text
 * keeps its body under the new operator name when the new operator takes the same kind of value (a document for a
 * document: `$set` to `$addFields`); otherwise, and for text without an operator, the new template.
 */
export function textForOperator(stage: PipelineStage, operator: string): string {
  const text = stage.text;
  if (text.trim() === "" || text === templateOf(stage.operator)) return templateOf(operator);
  const current = operatorOfText(text);
  if (!current || bodyKind(text) !== bodyKind(templateOf(operator))) return templateOf(operator);
  return text.replace(/^(\s*\{\s*(?:\/\/[^\n]*\n\s*|\/\*[\s\S]*?\*\/\s*)*["']?)\$[A-Za-z]+/, `$1${operator}`);
}

// --- Parsing ---------------------------------------------------------------------------------------------------------

export type ParsedStage = { ok: true; operator: string; json: string; value: Doc } | { ok: false; error: string; position: number };

/** One stage as Extended JSON: a document with exactly one key, a stage operator. */
export function parseStage(text: string): ParsedStage {
  if (text.trim() === "") return { ok: false, error: "The stage is empty: write a stage document, e.g. { $match: {} }", position: 0 };
  const parsed = parseMongoShell(text, { expect: "document" });
  if (!parsed.ok) return parsed;
  const value = JSON.parse(parsed.json) as Doc;
  const keys = Object.keys(value);
  const start = Math.max(0, text.indexOf("{"));
  if (keys.length === 0) return { ok: false, error: "The stage is empty: name a stage operator, e.g. { $match: {} }", position: start };
  if (keys.length > 1) {
    return { ok: false, error: `A stage has one operator; this one has ${keys.length} (${keys.join(", ")}). Split it into stages`, position: start };
  }
  const operator = keys[0];
  if (!operator.startsWith("$")) {
    const at = text.indexOf(operator);
    return { ok: false, error: `"${operator}" is not a stage operator: stage operators start with $, e.g. $match`, position: at >= 0 ? at : start };
  }
  return { ok: true, operator, json: parsed.json, value };
}

/** Why a stage does not run in the preview (its card says it), or the stage that runs there. */
export type PlanEntry =
  | { kind: "run"; previewIndex: number }
  | { kind: "disabled" }
  | { kind: "invalid"; error: string; position: number }
  | { kind: "writes"; operator: string }
  /** After a stage that stops the preview (one that does not parse, or writes). */
  | { kind: "blocked"; by: number };

export interface PipelinePlan {
  /** The stages the preview runs, in order (enabled, up to the first that does not parse or writes). */
  pipeline: Doc[];
  /** Per stage of the list, what the preview does with it. */
  entries: PlanEntry[];
  /** The first stage that does not parse (an index into the list), or -1. */
  firstInvalid: number;
}

/** What a preview runs, and why each other stage does not run. */
export function previewPlan(stages: readonly PipelineStage[]): PipelinePlan {
  const pipeline: Doc[] = [];
  const entries: PlanEntry[] = [];
  let stop = -1;
  let firstInvalid = -1;
  stages.forEach((stage, index) => {
    if (!stage.enabled) {
      entries.push({ kind: "disabled" });
      return;
    }
    const parsed = parseStage(stage.text);
    if (!parsed.ok) {
      if (firstInvalid < 0) firstInvalid = index;
      entries.push(stop >= 0 ? { kind: "blocked", by: stop } : { kind: "invalid", error: parsed.error, position: parsed.position });
      if (stop < 0) stop = index;
      return;
    }
    if (stop >= 0) {
      entries.push({ kind: "blocked", by: stop });
      return;
    }
    if (WRITE_OPERATORS.has(parsed.operator)) {
      entries.push({ kind: "writes", operator: parsed.operator });
      stop = index;
      return;
    }
    entries.push({ kind: "run", previewIndex: pipeline.length });
    pipeline.push(parsed.value);
  });
  return { pipeline, entries, firstInvalid };
}

export type RunnablePipeline = { ok: true; pipeline: Doc[]; texts: string[] } | { ok: false; index: number; error: string; position: number };

/** The enabled stages for a full run (writes included), or the first enabled stage that does not parse. */
export function enabledPipeline(stages: readonly PipelineStage[]): RunnablePipeline {
  const pipeline: Doc[] = [];
  const texts: string[] = [];
  for (const [index, stage] of stages.entries()) {
    if (!stage.enabled) continue;
    const parsed = parseStage(stage.text);
    if (!parsed.ok) return { ok: false, index, error: parsed.error, position: parsed.position };
    pipeline.push(parsed.value);
    texts.push(stage.text);
  }
  return { ok: true, pipeline, texts };
}

// --- From the Documents query ----------------------------------------------------------------------------------------

/**
 * The stages a Documents query turns into: `$match` (filter), `$sort`, `$skip`, `$limit` and `$project`, in the order a
 * find applies them. The limit is a stage only when it is not the default page size (the page size is not a limit of
 * the query). Fields as typed (braces optional) keep their text; empty ones give no stage.
 */
export function stagesFromQuery(state: QueryState): string[] {
  const body = (text: string) => braced(text.trim()).text;
  const stages: string[] = [];
  if (state.filter.trim() && state.filter.trim() !== "{}") stages.push(`{ $match: ${body(state.filter)} }`);
  if (state.sort.trim() && state.sort.trim() !== "{}") stages.push(`{ $sort: ${body(state.sort)} }`);
  if (state.skip > 0) stages.push(`{ $skip: ${Math.floor(state.skip)} }`);
  if (state.limit !== DEFAULT_LIMIT) stages.push(`{ $limit: ${Math.floor(state.limit)} }`);
  if (state.project.trim() && state.project.trim() !== "{}") stages.push(`{ $project: ${body(state.project)} }`);
  return stages;
}

// --- Saved state -----------------------------------------------------------------------------------------------------

export type SampleMode = "first" | "random";

export const DEFAULT_SAMPLE_SIZE = 1000;
export const MAX_SAMPLE_SIZE = 10_000;
/** Documents each preview strip gets per stage. */
export const PREVIEW_LIMIT = 10;
/** Stage texts kept with the open tabs at most (UTF-16 characters, all stages); a longer pipeline is saved by name instead. */
export const MAX_SAVED_PIPELINE_CHARS = 24_000;

export interface SavedStage {
  text: string;
  enabled: boolean;
  collapsed: boolean;
}

export interface AggregationsSavedState {
  stages: SavedStage[];
  sampleSize: number;
  sampleMode: SampleMode;
  /** Preview after each edit (debounced). */
  autoPreview: boolean;
  /** The index of the stage last focused. */
  active: number;
}

export function clampSampleSize(value: unknown): number {
  const n = Math.floor(Number(value));
  return Number.isFinite(n) && n >= 1 ? Math.min(n, MAX_SAMPLE_SIZE) : DEFAULT_SAMPLE_SIZE;
}

/** The saved state of `stages` and the options; stages are left out when their text is over the bound. */
export function savedStateOf(stages: readonly PipelineStage[], options: Omit<AggregationsSavedState, "stages">): AggregationsSavedState {
  const total = stages.reduce((sum, stage) => sum + stage.text.length, 0);
  const kept = total <= MAX_SAVED_PIPELINE_CHARS ? stages.map(({ text, enabled, collapsed }) => ({ text, enabled, collapsed })) : [];
  return { stages: kept, ...options };
}

/** Reads a saved state (any version, any shape); missing parts take their defaults. */
export function readAggregationsState(value: unknown): AggregationsSavedState {
  const record = typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
  const stages = Array.isArray(record.stages)
    ? record.stages
        .filter((item): item is Record<string, unknown> => typeof item === "object" && item !== null && typeof (item as SavedStage).text === "string")
        .slice(0, 200)
        .map((item) => ({ text: item.text as string, enabled: item.enabled !== false, collapsed: item.collapsed === true }))
    : [];
  return {
    stages,
    sampleSize: clampSampleSize(record.sampleSize ?? DEFAULT_SAMPLE_SIZE),
    sampleMode: record.sampleMode === "random" ? "random" : "first",
    autoPreview: record.autoPreview !== false,
    active: Math.max(0, Math.floor(Number(record.active) || 0)),
  };
}

/** Stages of saved texts (a saved query's pipeline, the saved state), each with an id from `nextId`. */
export function stagesOfTexts(texts: readonly (string | SavedStage)[], nextId: () => string): PipelineStage[] {
  return texts.map((item) => {
    const saved = typeof item === "string" ? { text: item, enabled: true, collapsed: false } : item;
    return { id: nextId(), operator: operatorOfText(saved.text) ?? "$match", text: saved.text, enabled: saved.enabled, collapsed: saved.collapsed };
  });
}

// --- Fields sampled at a stage ---------------------------------------------------------------------------------------

export interface SampledField {
  path: string;
  /** BSON type labels seen, most frequent first. */
  types: string[];
}

const MAX_FIELD_DEPTH = 4;

/** The dotted paths of `documents` with the BSON types seen at each (through arrays of documents), bounded. */
export function sampledFields(documents: readonly Json[], max = 80): SampledField[] {
  const seen = new Map<string, Map<string, number>>();
  const note = (path: string, type: string) => {
    let types = seen.get(path);
    if (!types) {
      if (seen.size >= max) return;
      types = new Map();
      seen.set(path, types);
    }
    types.set(type, (types.get(type) ?? 0) + 1);
  };
  const walk = (value: Json, prefix: string, depth: number) => {
    if (depth > MAX_FIELD_DEPTH || typeof value !== "object" || value === null || Array.isArray(value) || isWrapper(value)) return;
    for (const [key, inner] of Object.entries(value)) {
      const path = prefix ? `${prefix}.${key}` : key;
      note(path, BSON_TYPE_LABELS[bsonType(inner)]);
      if (Array.isArray(inner)) for (const item of inner.slice(0, 10)) walk(item, path, depth + 1);
      else walk(inner, path, depth + 1);
    }
  };
  for (const document of documents.slice(0, 100)) walk(document, "", 1);
  return [...seen].map(([path, types]) => ({ path, types: [...types].sort((a, b) => b[1] - a[1]).map(([type]) => type) }));
}

/** The `as` field a `$lookup` / `$graphLookup` stage writes its joined array into; null for other stages. */
export function lookupField(stage: Doc): string | null {
  const operator = Object.keys(stage)[0];
  if (operator !== "$lookup" && operator !== "$graphLookup") return null;
  const spec = stage[operator];
  if (typeof spec !== "object" || spec === null) return null;
  const as = (spec as Doc).as;
  return typeof as === "string" && as !== "" ? as : null;
}

/** A one-line summary of a pipeline: `$match → $group → $sort`. */
export function pipelineSummary(texts: readonly string[]): string {
  if (texts.length === 0) return "empty pipeline";
  const operators = texts.map((text) => operatorOfText(text) ?? "?");
  const shown = operators.length > 6 ? [...operators.slice(0, 5), "…"] : operators;
  return `${shown.join(" → ")} (${texts.length} ${texts.length === 1 ? "stage" : "stages"})`;
}
