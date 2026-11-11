/**
 * The dev mock's `mongo.schema` analyser, like src-tauri/src/engines/mongo/schema.rs: per dotted path (into
 * sub-documents and documents in arrays, depth and path caps), presence, `$type` histogram, array lengths and element
 * types, sample values, low-cardinality top values, min / max of numbers and dates, and inferred references (ObjectId
 * values found as `_id`s of another collection of the database).
 */
type Json = unknown;
type Doc = Record<string, Json>;

export const MAX_DEPTH = 8;
export const MAX_PATHS = 1000;
const MAX_ELEMENTS = 100;
const DISTINCT_CAP = 100;
const TOP_VALUES = 10;
const SAMPLE_VALUES = 3;
const MAX_VALUE_CHARS = 80;
const MAX_TRACKED_CHARS = 200;
const MAX_REF_PATHS = 8;
const MAX_REF_PROBES = 4;
const REF_IDS = 20;

const isDoc = (value: Json): value is Doc => typeof value === "object" && value !== null && !Array.isArray(value);
const WRAPPERS: Record<string, string> = {
  $oid: "objectId",
  $date: "date",
  $numberDecimal: "decimal",
  $numberLong: "long",
  $numberInt: "int",
  $numberDouble: "double",
  $binary: "binData",
  $uuid: "binData",
  $regularExpression: "regex",
  $timestamp: "timestamp",
  $minKey: "minKey",
  $maxKey: "maxKey",
};

/** The `$type` name of a relaxed Extended JSON value. */
export function typeName(value: Json): string {
  if (value === null || value === undefined) return "null";
  if (typeof value === "string") return "string";
  if (typeof value === "boolean") return "bool";
  if (typeof value === "number") return Number.isInteger(value) && Math.abs(value) < 2 ** 31 ? "int" : "double";
  if (Array.isArray(value)) return "array";
  if (isDoc(value)) {
    const keys = Object.keys(value);
    if (keys.length >= 1 && keys[0] in WRAPPERS && keys.every((key) => key.startsWith("$"))) return WRAPPERS[keys[0]];
    return "object";
  }
  return "other";
}

function numberOf(value: Json): number | null {
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (!isDoc(value)) return null;
  for (const key of ["$numberDecimal", "$numberLong", "$numberInt", "$numberDouble"]) {
    if (key in value) {
      const n = Number(value[key]);
      return Number.isFinite(n) ? n : null;
    }
  }
  return null;
}

const cut = (text: string) => (text.length <= MAX_VALUE_CHARS ? text : `${text.slice(0, MAX_VALUE_CHARS - 1)}…`);
const shown = (value: Json) => (typeof value === "string" ? cut(value) : value);

interface PathStats {
  path: string;
  parent: number | null;
  depth: number;
  present: number;
  lastDocument: number;
  types: Map<string, number>;
  array: { count: number; min: number; max: number; total: number; elementTypes: Map<string, number> } | null;
  samples: Json[];
  sampleKeys: string[];
  values: Map<string, { value: Json; count: number }> | null;
  numbers: { min: [number, Json]; max: [number, Json] } | null;
  dates: { min: number; max: number } | null;
  objectIds: string[];
}

const sortedTypes = (types: Map<string, number>) =>
  [...types.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([type, count]) => ({ type, count }));

const TRACKED = new Set(["string", "bool", "int", "long", "double", "decimal", "objectId", "date"]);

export class SchemaAnalyser {
  private readonly paths: PathStats[] = [];
  private readonly index = new Map<string, number>();
  documents = 0;
  truncated = false;
  depthCapped = false;

  add(document: Doc): void {
    this.documents++;
    this.walk(document, "", null, 0);
  }

  private walk(document: Doc, prefix: string, parent: number | null, depth: number): void {
    if (depth >= MAX_DEPTH) {
      this.depthCapped = true;
      return;
    }
    for (const [key, value] of Object.entries(document)) {
      const slot = this.slot(prefix ? `${prefix}.${key}` : key, parent, depth);
      if (slot !== null) this.record(slot, value, depth);
    }
  }

  private slot(path: string, parent: number | null, depth: number): number | null {
    const known = this.index.get(path);
    if (known !== undefined) return known;
    if (this.paths.length >= MAX_PATHS) {
      this.truncated = true;
      return null;
    }
    this.index.set(path, this.paths.length);
    this.paths.push({
      path,
      parent,
      depth,
      present: 0,
      lastDocument: 0,
      types: new Map(),
      array: null,
      samples: [],
      sampleKeys: [],
      values: new Map(),
      numbers: null,
      dates: null,
      objectIds: [],
    });
    return this.paths.length - 1;
  }

  private record(slot: number, value: Json, depth: number): void {
    const stats = this.paths[slot];
    if (stats.lastDocument !== this.documents) {
      stats.lastDocument = this.documents;
      stats.present++;
    }
    const type = typeName(value);
    stats.types.set(type, (stats.types.get(type) ?? 0) + 1);
    if (type === "object") this.walk(value as Doc, stats.path, slot, depth + 1);
    else if (Array.isArray(value)) {
      const array = (stats.array ??= { count: 0, min: Infinity, max: 0, total: 0, elementTypes: new Map() });
      array.count++;
      array.total += value.length;
      array.min = Math.min(array.min, value.length);
      array.max = Math.max(array.max, value.length);
      for (const item of value.slice(0, MAX_ELEMENTS)) {
        const itemType = typeName(item);
        array.elementTypes.set(itemType, (array.elementTypes.get(itemType) ?? 0) + 1);
        if (itemType === "object") this.walk(item as Doc, stats.path, slot, depth + 1);
        else if (itemType !== "array") this.scalar(stats, item, itemType);
      }
    } else this.scalar(stats, value, type);
  }

  private scalar(stats: PathStats, value: Json, type: string): void {
    if (type === "null") return;
    const key = JSON.stringify(value);
    if (stats.samples.length < SAMPLE_VALUES && !stats.sampleKeys.includes(key)) {
      stats.sampleKeys.push(key);
      stats.samples.push(shown(value));
    }
    if (TRACKED.has(type) && stats.values) {
      const entry = stats.values.get(key);
      if (typeof value === "string" && value.length > MAX_TRACKED_CHARS) stats.values = null;
      else if (entry) entry.count++;
      else if (stats.values.size >= DISTINCT_CAP) stats.values = null;
      else stats.values.set(key, { value: shown(value), count: 1 });
    }
    const n = numberOf(value);
    if (n !== null) {
      stats.numbers ??= { min: [n, value], max: [n, value] };
      if (n < stats.numbers.min[0]) stats.numbers.min = [n, value];
      if (n > stats.numbers.max[0]) stats.numbers.max = [n, value];
    }
    if (type === "date") {
      const at = Date.parse(String((value as Doc).$date));
      if (Number.isFinite(at)) stats.dates = stats.dates ? { min: Math.min(stats.dates.min, at), max: Math.max(stats.dates.max, at) } : { min: at, max: at };
    }
    if (type === "objectId" && stats.objectIds.length < REF_IDS) {
      const id = String((value as Doc).$oid);
      if (!stats.objectIds.includes(id)) stats.objectIds.push(id);
    }
  }

  /** Mostly-ObjectId paths (not `_id`), reference-like names first, each with the ids sampled. */
  referenceCandidates(): [string, string[]][] {
    return this.paths
      .filter((stats) => stats.path !== "_id" && stats.objectIds.length > 0)
      .filter((stats) => {
        const ids = (stats.types.get("objectId") ?? 0) + (stats.array?.elementTypes.get("objectId") ?? 0);
        const nonNull = [...stats.types.entries()].filter(([type]) => type !== "null").reduce((sum, [, n]) => sum + n, 0);
        return ids * 2 >= Math.max(1, nonNull);
      })
      .sort((a, b) => Number(!refBase(lastName(a.path))) - Number(!refBase(lastName(b.path))))
      .slice(0, MAX_REF_PATHS)
      .map((stats) => [stats.path, [...stats.objectIds]]);
  }

  report(references: Map<string, Json>): Doc[] {
    const children = this.paths.map(() => [] as number[]);
    const roots: number[] = [];
    this.paths.forEach((stats, slot) => (stats.parent === null ? roots.push(slot) : children[stats.parent].push(slot)));
    const order: number[] = [];
    const stack = [...roots].reverse();
    while (stack.length > 0) {
      const slot = stack.pop()!;
      order.push(slot);
      stack.push(...[...children[slot]].reverse());
    }
    const documents = Math.max(1, this.documents);
    return order.map((slot) => {
      const stats = this.paths[slot];
      const values = stats.values ? [...stats.values.values()].sort((a, b) => b.count - a.count) : null;
      const min = stats.numbers ? stats.numbers.min[1] : stats.dates ? { $date: new Date(stats.dates.min).toISOString() } : null;
      const max = stats.numbers ? stats.numbers.max[1] : stats.dates ? { $date: new Date(stats.dates.max).toISOString() } : null;
      return {
        path: stats.path,
        name: lastName(stats.path),
        depth: stats.depth,
        count: stats.present,
        percent: Math.round((stats.present / documents) * 1000) / 10,
        types: sortedTypes(stats.types),
        mixed: [...stats.types.keys()].filter((type) => type !== "null").length > 1,
        array: stats.array
          ? {
              count: stats.array.count,
              minLength: stats.array.min,
              avgLength: Math.round((stats.array.total / Math.max(1, stats.array.count)) * 10) / 10,
              maxLength: stats.array.max,
              elementTypes: sortedTypes(stats.array.elementTypes),
            }
          : null,
        samples: stats.samples,
        distinct: stats.values ? stats.values.size : null,
        topValues: values && values.length > 0 ? values.slice(0, TOP_VALUES) : null,
        min,
        max,
        reference: references.get(stats.path) ?? null,
      };
    });
  }
}

const lastName = (path: string) => path.slice(path.lastIndexOf(".") + 1);
const refBase = (name: string) => /^(.+?)(?:_ids?|Ids?|IDs?)$/.exec(name)?.[1] ?? null;
const normal = (name: string) => name.toLowerCase().replace(/[_\-\s]/g, "");
const plurals = (word: string) => [word, `${word}s`, `${word}es`, ...(/[^aeiou]y$/.test(word) ? [`${word.slice(0, -1)}ies`] : [])];

/** The collections to probe for `path`: named after the field first, then the others (not `self`, never `system.*`). */
export function probeOrder(path: string, collections: readonly string[], self: string): string[] {
  const base = refBase(lastName(path));
  const wanted = new Set<string>();
  if (base) {
    for (const form of plurals(normal(base))) wanted.add(form);
    const words = base.split(/_|-|(?=[A-Z])/).filter(Boolean);
    const last = words[words.length - 1];
    if (words.length > 1 && last.length >= 3) for (const form of plurals(normal(last))) wanted.add(form);
  }
  const usable = collections.filter((name) => !name.startsWith("system."));
  const matching = usable.filter((name) => [...wanted].some((form) => normal(name) === form || normal(name).endsWith(form)));
  const others = usable.filter((name) => name !== self && !matching.includes(name)).sort();
  return [...matching, ...others].slice(0, MAX_REF_PROBES);
}

/** `mongo.schema` over `sample`: `collections` lists the database, `idsOf` gives a collection's `_id` values (as JSON text). */
export function schemaResult(
  sample: Doc[],
  options: { mode: string; size: number; self: string; references: boolean; collections: () => string[]; idsOf: (collection: string) => Set<string> },
): Doc {
  const analyser = new SchemaAnalyser();
  for (const document of sample) analyser.add(document);
  const references = new Map<string, Json>();
  let listed = 0;
  if (options.references) {
    const candidates = analyser.referenceCandidates();
    const collections = candidates.length > 0 ? options.collections() : [];
    listed = collections.length;
    for (const [path, ids] of candidates) {
      let best: { collection: string; matched: number } | null = null;
      for (const collection of probeOrder(path, collections, options.self)) {
        const known = options.idsOf(collection);
        const matched = ids.filter((id) => known.has(JSON.stringify({ $oid: id }))).length;
        if (matched > 0 && (!best || matched > best.matched)) best = { collection, matched };
        if (matched === ids.length) break;
      }
      if (best) references.set(path, { ...best, probed: ids.length, inferred: true });
    }
  }
  return {
    sample: { mode: options.mode, size: options.size, count: analyser.documents },
    paths: analyser.report(references),
    truncated: analyser.truncated,
    depthCapped: analyser.depthCapped,
    references: { probed: options.references, collections: listed, note: null },
    elapsedMs: 4,
  };
}
