/**
 * Documents as `mongo.find` returns them (relaxed Extended JSON), without DOM: how a value reads in the list
 * (`ObjectId("…")`, `ISODate("…")`, a quoted string), what a nested value folds to (`{ 3 fields }`, `[ 5 items ]`) and
 * the field paths the documents have (for completion). The readable-BSON views of step M4 build on these.
 */

export type Json = unknown;
export type Doc = Record<string, Json>;

export type ValueKind = "string" | "number" | "boolean" | "null" | "objectId" | "date" | "binary" | "regex" | "special" | "object" | "array";

const WRAPPERS = new Set([
  "$oid",
  "$date",
  "$numberLong",
  "$numberDecimal",
  "$numberInt",
  "$numberDouble",
  "$binary",
  "$uuid",
  "$regularExpression",
  "$timestamp",
  "$minKey",
  "$maxKey",
  "$symbol",
  "$code",
  "$dbPointer",
  "$undefined",
]);

/** An Extended JSON wrapper (`{ "$oid": … }`) is one value, not a nested document. */
export function isWrapper(value: Json): value is Doc {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const keys = Object.keys(value);
  return keys.length >= 1 && keys.length <= 2 && WRAPPERS.has(keys[0]);
}

export function isNested(value: Json): value is Doc | Json[] {
  return Array.isArray(value) || (typeof value === "object" && value !== null && !isWrapper(value));
}

/** The text and kind of a value that is not nested. */
export function scalarText(value: Json): { text: string; kind: ValueKind } {
  if (value === null || value === undefined) return { text: "null", kind: "null" };
  if (typeof value === "string") return { text: JSON.stringify(value), kind: "string" };
  if (typeof value === "number") return { text: String(value), kind: "number" };
  if (typeof value === "boolean") return { text: String(value), kind: "boolean" };
  if (!isWrapper(value)) return { text: JSON.stringify(value), kind: Array.isArray(value) ? "array" : "object" };
  const [key] = Object.keys(value);
  const inner = value[key];
  switch (key) {
    case "$oid":
      return { text: `ObjectId(${JSON.stringify(inner)})`, kind: "objectId" };
    case "$date": {
      if (typeof inner === "string") return { text: `ISODate(${JSON.stringify(inner)})`, kind: "date" };
      const ms = Number(typeof inner === "object" && inner !== null ? (inner as Doc).$numberLong : inner);
      const date = new Date(ms);
      return { text: Number.isNaN(date.getTime()) ? `Date(${ms})` : `ISODate(${JSON.stringify(date.toISOString())})`, kind: "date" };
    }
    case "$numberLong":
      return { text: `NumberLong(${String(inner)})`, kind: "number" };
    case "$numberDecimal":
      return { text: `NumberDecimal(${JSON.stringify(String(inner))})`, kind: "number" };
    case "$numberInt":
    case "$numberDouble":
      return { text: String(inner), kind: "number" };
    case "$uuid":
      return { text: `UUID(${JSON.stringify(inner)})`, kind: "binary" };
    case "$binary": {
      const binary = (typeof inner === "object" && inner !== null ? inner : {}) as Doc;
      const subType = String(binary.subType ?? "00");
      return { text: `Binary.createFromBase64(${JSON.stringify(String(binary.base64 ?? ""))}, ${parseInt(subType, 16) || 0})`, kind: "binary" };
    }
    case "$regularExpression": {
      const regex = (typeof inner === "object" && inner !== null ? inner : {}) as Doc;
      return { text: `/${String(regex.pattern ?? "")}/${String(regex.options ?? "")}`, kind: "regex" };
    }
    case "$timestamp": {
      const ts = (typeof inner === "object" && inner !== null ? inner : {}) as Doc;
      return { text: `Timestamp({ t: ${String(ts.t)}, i: ${String(ts.i)} })`, kind: "special" };
    }
    case "$minKey":
      return { text: "MinKey()", kind: "special" };
    case "$maxKey":
      return { text: "MaxKey()", kind: "special" };
    default:
      return { text: JSON.stringify(value), kind: "special" };
  }
}

/** What a folded nested value shows: `{ 3 fields }`, `[ 5 items ]`, `{}`, `[]`. */
export function foldedText(value: Doc | Json[]): string {
  if (Array.isArray(value)) return value.length === 0 ? "[]" : `[ ${value.length} ${value.length === 1 ? "item" : "items"} ]`;
  const n = Object.keys(value).length;
  return n === 0 ? "{}" : `{ ${n} ${n === 1 ? "field" : "fields"} }`;
}

const MAX_PATHS = 400;
const MAX_DEPTH = 5;

/** The dotted field paths of `documents` (through arrays of documents), in first-seen order, bounded. */
export function documentPaths(documents: readonly Json[]): string[] {
  const paths = new Set<string>();
  const walk = (value: Json, prefix: string, depth: number) => {
    if (paths.size >= MAX_PATHS || depth > MAX_DEPTH) return;
    if (Array.isArray(value)) {
      for (const item of value.slice(0, 20)) walk(item, prefix, depth);
      return;
    }
    if (typeof value !== "object" || value === null || isWrapper(value)) return;
    for (const [key, inner] of Object.entries(value)) {
      const path = prefix ? `${prefix}.${key}` : key;
      paths.add(path);
      if (paths.size >= MAX_PATHS) return;
      walk(inner, path, depth + 1);
    }
  };
  for (const document of documents.slice(0, 200)) walk(document, "", 1);
  return [...paths];
}

/** One document as indented JSON text (relaxed Extended JSON, as read). */
export function documentJson(document: Json): string {
  return JSON.stringify(document, null, 2);
}
