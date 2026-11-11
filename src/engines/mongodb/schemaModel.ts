/**
 * The Schema section's model, without DOM: the `mongo.schema` result types, the saved state (bounded: the result is
 * kept compact so the tab's saved view stays small), the filters a path offers ("has a value", "is missing", one of
 * its top values, one of its types), the visible rows of the path tree, and the summary the assistant reads (paths,
 * types and presence; no values).
 */
import { shellText } from "./bson";
import type { Json } from "./documents";
import { clampSampleSize } from "./pipelineModel";

export type SchemaSampleMode = "first" | "random" | "filtered";

export interface TypeCount {
  type: string;
  count: number;
}

export interface SchemaReference {
  collection: string;
  matched: number;
  probed: number;
  inferred: boolean;
}

export interface SchemaPath {
  path: string;
  name: string;
  depth: number;
  count: number;
  percent: number;
  types: TypeCount[];
  mixed: boolean;
  array: { count: number; minLength: number; avgLength: number; maxLength: number; elementTypes: TypeCount[] } | null;
  samples: Json[];
  distinct: number | null;
  topValues: { value: Json; count: number }[] | null;
  min: Json;
  max: Json;
  reference: SchemaReference | null;
}

export interface SchemaResult {
  sample: { mode: SchemaSampleMode; size: number; count: number };
  paths: SchemaPath[];
  truncated: boolean;
  depthCapped: boolean;
  references: { probed: boolean; collections: number; note: string | null };
  elapsedMs: number;
}

export interface SchemaArgs {
  database: string;
  collection: string;
  sampleMode: SchemaSampleMode;
  sampleSize: number;
  filter?: string;
  references?: boolean;
  maxTimeMS?: number;
}

export const SAMPLE_MODES: readonly SchemaSampleMode[] = ["first", "random", "filtered"];

/** Shared store key: the last analysed schema of this tab (completion and the assistant read it). */
export const STORE_SCHEMA = "mongo.schema";

/** `$type` names as shown. */
export const TYPE_LABELS: Record<string, string> = {
  double: "Double",
  int: "Int32",
  long: "Int64",
  decimal: "Decimal128",
  string: "String",
  object: "Object",
  array: "Array",
  binData: "Binary",
  objectId: "ObjectId",
  bool: "Boolean",
  date: "Date",
  null: "Null",
  regex: "Regex",
  timestamp: "Timestamp",
  minKey: "MinKey",
  maxKey: "MaxKey",
  javascript: "JavaScript",
  javascriptWithScope: "JavaScript",
  symbol: "Symbol",
  undefined: "Undefined",
  dbPointer: "DBPointer",
};

export const typeLabel = (type: string) => TYPE_LABELS[type] ?? type;

// --- Saved state ----------------------------------------------------------------------------------------------------

export interface SchemaSavedState {
  sampleMode: SchemaSampleMode;
  sampleSize: number;
  filter: string;
  /** The last result, compacted (`compactResult`); null before the first analysis. */
  result: SchemaResult | null;
  /** When the result was read (ms since the epoch). */
  at: number | null;
  /** Paths collapsed in the tree. */
  collapsed: string[];
}

/** Characters the saved result may take (the whole tab's saved view is about 32 KB, shared with other sections). */
export const MAX_SAVED_RESULT_CHARS = 14_000;
const MAX_SAVED_FILTER_CHARS = 2000;
const MAX_SAVED_COLLAPSED = 200;

export const DEFAULT_SCHEMA_STATE: SchemaSavedState = { sampleMode: "random", sampleSize: 1000, filter: "", result: null, at: null, collapsed: [] };

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);
const short = (value: Json) => (typeof value === "string" && value.length > 40 ? `${value.slice(0, 39)}…` : value);

/** The result without samples and with at most `top` top values per path, then without the paths that do not fit `maxChars`. */
export function compactResult(result: SchemaResult, maxChars = MAX_SAVED_RESULT_CHARS): SchemaResult {
  for (const top of [5, 3, 0]) {
    const paths = result.paths.map((entry) => ({
      ...entry,
      samples: [],
      topValues: top > 0 && entry.topValues ? entry.topValues.slice(0, top).map((item) => ({ value: short(item.value), count: item.count })) : null,
      min: short(entry.min),
      max: short(entry.max),
    }));
    const compact = { ...result, paths };
    if (JSON.stringify(compact).length <= maxChars) return compact;
    if (top === 0) {
      // Drop the deepest paths last-first until it fits.
      const kept = [...paths];
      while (kept.length > 0 && JSON.stringify({ ...result, paths: kept }).length > maxChars) kept.pop();
      return { ...result, paths: kept, truncated: true };
    }
  }
  return result;
}

function readPath(value: unknown): SchemaPath | null {
  if (!isRecord(value) || typeof value.path !== "string" || !value.path) return null;
  const types = Array.isArray(value.types)
    ? value.types.filter((item): item is TypeCount => isRecord(item) && typeof item.type === "string" && typeof item.count === "number")
    : [];
  const array = isRecord(value.array) ? (value.array as SchemaPath["array"]) : null;
  const topValues = Array.isArray(value.topValues)
    ? value.topValues.filter((item): item is { value: Json; count: number } => isRecord(item) && typeof item.count === "number")
    : null;
  const reference = isRecord(value.reference) && typeof value.reference.collection === "string" ? (value.reference as unknown as SchemaReference) : null;
  const number = (key: string) => (typeof value[key] === "number" ? (value[key] as number) : 0);
  return {
    path: value.path,
    name: typeof value.name === "string" ? value.name : value.path.slice(value.path.lastIndexOf(".") + 1),
    depth: number("depth"),
    count: number("count"),
    percent: number("percent"),
    types,
    mixed: value.mixed === true,
    array,
    samples: Array.isArray(value.samples) ? value.samples : [],
    distinct: typeof value.distinct === "number" ? value.distinct : null,
    topValues,
    min: value.min ?? null,
    max: value.max ?? null,
    reference,
  };
}

/** A `mongo.schema` result (or a saved one) read defensively; null when it is not one. */
export function readResult(value: unknown): SchemaResult | null {
  if (!isRecord(value) || !Array.isArray(value.paths) || !isRecord(value.sample)) return null;
  const sample = value.sample;
  const mode = SAMPLE_MODES.includes(sample.mode as SchemaSampleMode) ? (sample.mode as SchemaSampleMode) : "first";
  const references = isRecord(value.references) ? value.references : {};
  return {
    sample: { mode, size: Number(sample.size) || 0, count: Number(sample.count) || 0 },
    paths: value.paths.map(readPath).filter((entry): entry is SchemaPath => entry !== null),
    truncated: value.truncated === true,
    depthCapped: value.depthCapped === true,
    references: {
      probed: references.probed === true,
      collections: Number(references.collections) || 0,
      note: typeof references.note === "string" ? references.note : null,
    },
    elapsedMs: Number(value.elapsedMs) || 0,
  };
}

export function readSchemaState(value: unknown): SchemaSavedState {
  if (!isRecord(value)) return { ...DEFAULT_SCHEMA_STATE };
  const mode = SAMPLE_MODES.includes(value.sampleMode as SchemaSampleMode) ? (value.sampleMode as SchemaSampleMode) : DEFAULT_SCHEMA_STATE.sampleMode;
  return {
    sampleMode: mode,
    sampleSize: value.sampleSize === undefined ? DEFAULT_SCHEMA_STATE.sampleSize : clampSampleSize(value.sampleSize),
    filter: typeof value.filter === "string" ? value.filter.slice(0, MAX_SAVED_FILTER_CHARS) : "",
    result: readResult(value.result),
    at: typeof value.at === "number" ? value.at : null,
    collapsed: Array.isArray(value.collapsed) ? value.collapsed.filter((item): item is string => typeof item === "string").slice(0, MAX_SAVED_COLLAPSED) : [],
  };
}

export function savedSchemaState(state: SchemaSavedState): SchemaSavedState {
  return {
    ...state,
    filter: state.filter.slice(0, MAX_SAVED_FILTER_CHARS),
    result: state.result ? compactResult(state.result) : null,
    collapsed: state.collapsed.slice(0, MAX_SAVED_COLLAPSED),
  };
}

// --- The tree ---------------------------------------------------------------------------------------------------------

const parentOf = (path: string) => (path.includes(".") ? path.slice(0, path.lastIndexOf(".")) : null);

/** Paths that have children in the result. */
export function parents(paths: readonly SchemaPath[]): Set<string> {
  const known = new Set(paths.map((entry) => entry.path));
  const found = new Set<string>();
  for (const entry of paths) {
    const parent = parentOf(entry.path);
    if (parent && known.has(parent)) found.add(parent);
  }
  return found;
}

/** The rows shown: every path whose ancestors are all expanded, optionally only those matching `search`. */
export function visiblePaths(paths: readonly SchemaPath[], collapsed: ReadonlySet<string>, search = ""): SchemaPath[] {
  const needle = search.trim().toLowerCase();
  return paths.filter((entry) => {
    if (needle) return entry.path.toLowerCase().includes(needle);
    for (let parent = parentOf(entry.path); parent; parent = parentOf(parent)) if (collapsed.has(parent)) return false;
    return true;
  });
}

// --- Filters ----------------------------------------------------------------------------------------------------------

export interface PathFilter {
  label: string;
  filter: string;
}

const key = (path: string) => shellText(path);

/** The Documents filters a path offers: exists, missing, each of its first top values, each of its types when mixed. */
export function pathFilters(entry: SchemaPath, maxValues = 5): PathFilter[] {
  const filters: PathFilter[] = [
    { label: "Has this field", filter: `{ ${key(entry.path)}: { $exists: true } }` },
    { label: "Is missing", filter: `{ ${key(entry.path)}: { $exists: false } }` },
  ];
  for (const item of (entry.topValues ?? []).slice(0, maxValues)) {
    // A cut string (shown with an ellipsis) would not match: only whole values are offered.
    if (typeof item.value === "string" && item.value.endsWith("…")) continue;
    const text = shellText(item.value);
    filters.push({ label: `Equals ${text.length > 48 ? `${text.slice(0, 47)}…` : text}`, filter: `{ ${key(entry.path)}: ${text} }` });
  }
  if (entry.mixed) {
    for (const type of entry.types) {
      if (type.type === "null") continue;
      filters.push({ label: `Is of type ${typeLabel(type.type)}`, filter: `{ ${key(entry.path)}: { $type: ${shellText(type.type)} } }` });
    }
  }
  return filters;
}

// --- For other sections and the assistant ------------------------------------------------------------------------------

/** The field paths of the result, for completion. */
export function schemaFieldPaths(result: SchemaResult): string[] {
  return result.paths.map((entry) => entry.path);
}

const typesText = (types: readonly TypeCount[]) =>
  types
    .filter((type) => type.type !== "null")
    .map((type) => typeLabel(type.type))
    .join("|") || "Null";

/** What the assistant reads: the sample, then one line per path with types and presence (no values), bounded. */
export function schemaSummary(result: SchemaResult, target: { database: string; collection: string }, maxPaths = 120): string {
  const lines = [
    `Schema of ${target.database}.${target.collection}, analysed on a ${result.sample.mode} sample of ${result.sample.count} documents${result.truncated ? " (paths truncated)" : ""}:`,
  ];
  for (const entry of result.paths.slice(0, maxPaths)) {
    let line = `${entry.path}: ${typesText(entry.types)}${entry.mixed ? " (mixed)" : ""}, ${entry.percent}%`;
    if (entry.types.some((type) => type.type === "null")) line += ", sometimes null";
    if (entry.array) line += `, array of ${typesText(entry.array.elementTypes)} (length ${entry.array.minLength}-${entry.array.maxLength})`;
    if (entry.distinct !== null && entry.distinct <= 20) line += `, ${entry.distinct} distinct values`;
    if (entry.reference) line += `, likely references ${entry.reference.collection}._id (inferred)`;
    lines.push(line);
  }
  if (result.paths.length > maxPaths) lines.push(`… ${result.paths.length - maxPaths} more paths.`);
  return lines.join("\n");
}
