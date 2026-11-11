// Completion for the MongoDB query bar's fields (Filter, Project, Sort, Collation). Each field holds one document in
// mongosh syntax, braces optional. Keys reuse the console's analysis (`mongoCompletions`) by placing the field where it
// sits in a shell command (`db.getCollection("orders").find(<filter>, <project>).sort(<sort>)`), so fields and query
// operators are offered where they fit. Values add the BSON constructors and, in the filter, the field's own values
// from `mongo.distinct` (asked lazily through `values`, debounced and capped by the caller).
import type { Completion, CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import type { CompletionSchema } from "../types";
import { mongoCompletions } from "./completion";

export type QueryBarField = "filter" | "project" | "sort" | "collation";

/** Where a value of the filter is being typed. */
export interface ValueContext {
  /** The dotted field path the value is compared with (`customer.name`, `items.sku` through `$elemMatch`). */
  field: string;
  /** The query operator the value belongs to (`$in`), null for an equality (`{ status: "paid" }`). */
  operator: string | null;
  from: number;
  to: number;
  prefix: string;
  /** The open quote when the value is a string being typed. */
  quote: string | null;
}

/** Operators whose values are values of the field (so the field's distinct values fit). */
const VALUE_OPERATORS = new Set(["$eq", "$ne", "$gt", "$gte", "$lt", "$lte", "$in", "$nin", "$all"]);

export const BSON_CONSTRUCTORS: { label: string; apply: string; info: string }[] = [
  { label: 'ObjectId("…")', apply: 'ObjectId("")', info: "An ObjectId from its 24 hex digits" },
  { label: 'ISODate("…")', apply: 'ISODate("")', info: 'A date, e.g. ISODate("2026-01-31T00:00:00Z")' },
  { label: 'new Date("…")', apply: 'new Date("")', info: "A date from ISO text with a zone, or milliseconds" },
  { label: 'NumberLong("…")', apply: 'NumberLong("")', info: "A 64-bit integer" },
  { label: "NumberInt(…)", apply: "NumberInt()", info: "A 32-bit integer" },
  { label: 'NumberDecimal("…")', apply: 'NumberDecimal("")', info: "A 128-bit decimal" },
  { label: 'UUID("…")', apply: 'UUID("")', info: "A UUID (binary subtype 4)" },
  { label: "/regex/i", apply: "//i", info: "A regular expression (case-insensitive with i)" },
  { label: "true", apply: "true", info: "Boolean" },
  { label: "false", apply: "false", info: "Boolean" },
  { label: "null", apply: "null", info: "Null, or a missing field" },
];

const COLLATION_KEYS = [
  ["locale", 'The language rules, e.g. "en", "fr", "simple"'],
  ["strength", "1 base letters, 2 accents, 3 case (default), 4, 5"],
  ["caseLevel", "Compare case at strength 1 or 2"],
  ["caseFirst", '"upper", "lower" or "off"'],
  ["numericOrdering", "Compare digit runs as numbers"],
  ["alternate", '"non-ignorable" or "shifted"'],
  ["maxVariable", '"punct" or "space" (with alternate "shifted")'],
  ["backwards", "Accents compared from the end of the string"],
  ["normalization", "Check that text is normalised"],
] as const;

/** The shell command text a field sits in, before and after it. */
function shellFrame(field: QueryBarField, collection: string): { before: string; after: string } {
  const coll = `db.getCollection(${JSON.stringify(collection)})`;
  switch (field) {
    case "filter":
      return { before: `${coll}.find(`, after: ")" };
    case "project":
      return { before: `${coll}.find({}, `, after: ")" };
    case "sort":
      return { before: `${coll}.find({}).sort(`, after: ")" };
    default:
      return { before: `${coll}.find({}).collation(`, after: ")" };
  }
}

/** The text as parsed: braces added when the field was typed without them. */
function bracedText(text: string): { text: string; shift: number } {
  const trimmed = text.trim();
  if (trimmed === "" || trimmed.startsWith("{")) return { text, shift: 0 };
  return { text: `{${text}}`, shift: 1 };
}

const IDENT = /[A-Za-z0-9_$]/;

interface Frame {
  open: "{" | "[" | "(";
  expectKey: boolean;
  /** For "{": the key whose value is being read. */
  currentKey: string | null;
}

/** Finds where a value of the filter is typed at `pos` (braces optional); null for keys and anything else. */
export function filterValueContext(text: string, pos: number): ValueContext | null {
  const { text: source, shift } = bracedText(text);
  const end = pos + shift;
  const stack: Frame[] = [];
  let lastAtom: string | null = null;
  let i = 0;
  let inString: { quote: string; start: number } | null = null;
  while (i < end) {
    const c = source[i];
    if (c === '"' || c === "'") {
      let j = i + 1;
      while (j < end && source[j] !== c) j += source[j] === "\\" ? 2 : 1;
      if (j >= end) {
        inString = { quote: c, start: i };
        break;
      }
      lastAtom = source.slice(i + 1, j);
      i = j + 1;
      continue;
    }
    if (IDENT.test(c)) {
      let j = i;
      while (j < end && IDENT.test(source[j])) j++;
      lastAtom = source.slice(i, j);
      i = j;
      continue;
    }
    const top = stack[stack.length - 1];
    if (c === "{" || c === "[" || c === "(") {
      stack.push({ open: c, expectKey: c === "{", currentKey: null });
      lastAtom = null;
    } else if (c === "}" || c === "]" || c === ")") {
      stack.pop();
      lastAtom = null;
    } else if (c === ":" && top?.open === "{") {
      top.currentKey = lastAtom;
      top.expectKey = false;
      lastAtom = null;
    } else if (c === ",") {
      if (top?.open === "{") {
        top.expectKey = true;
        top.currentKey = null;
      }
      lastAtom = null;
    } else if (!/\s/.test(c)) {
      lastAtom = null;
    }
    i++;
  }
  const top = stack[stack.length - 1];
  if (!top || top.open === "(") return null;
  if (top.open === "{" && (top.expectKey || top.currentKey === null)) return null;
  // The path: the non-operator keys of the enclosing objects; an array is only a value list of $in / $nin / $all.
  const path: string[] = [];
  let operator: string | null = null;
  for (let k = 0; k < stack.length; k++) {
    const frame = stack[k];
    if (frame.open === "(") return null;
    if (frame.open === "[") {
      const owner = stack[k - 1];
      const key = owner?.open === "{" ? owner.currentKey : null;
      // Elements of $and / $or / $nor are query documents: their keys start a path of their own.
      if (key && ["$and", "$or", "$nor"].includes(key)) continue;
      if (k === stack.length - 1 && key && ["$in", "$nin", "$all"].includes(key)) continue;
      return null;
    }
    const key = frame.currentKey;
    if (!key) return null;
    if (key === "$expr" || key === "$where" || key === "$jsonSchema" || key === "$text") return null;
    if (key.startsWith("$")) {
      if (k === stack.length - 1 || (k === stack.length - 2 && stack[k + 1].open === "[")) operator = key;
      continue;
    }
    path.push(key);
  }
  if (path.length === 0) return null;
  if (operator && !VALUE_OPERATORS.has(operator)) return null;
  if (inString) {
    const from = inString.start + 1 - shift;
    return { field: path.join("."), operator, from, to: pos, prefix: source.slice(inString.start + 1, end), quote: inString.quote };
  }
  let k = end;
  while (k > 0 && /[\w$.\-+]/.test(source[k - 1])) k--;
  // Only right after ":", ",", "[" (or the start of the value): not inside a constructor call or after another token.
  let b = k;
  while (b > 0 && /\s/.test(source[b - 1])) b--;
  if (b > 0 && !":,[".includes(source[b - 1])) return null;
  return { field: path.join("."), operator, from: k - shift, to: pos, prefix: source.slice(k, end), quote: null };
}

/** How a sampled value is written in a filter (`ObjectId("…")`, `ISODate("…")`, a JSON literal); null when unsuitable. */
export function valueLiteral(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") return JSON.stringify(value);
  if (typeof value !== "object" || Array.isArray(value)) return null;
  const object = value as Record<string, unknown>;
  const keys = Object.keys(object);
  if (keys.length !== 1) return null;
  const inner = object[keys[0]];
  switch (keys[0]) {
    case "$oid":
      return typeof inner === "string" ? `ObjectId(${JSON.stringify(inner)})` : null;
    case "$date": {
      if (typeof inner === "string") return `ISODate(${JSON.stringify(inner)})`;
      const ms = typeof inner === "object" && inner !== null ? Number((inner as Record<string, unknown>).$numberLong) : Number(inner);
      if (!Number.isFinite(ms)) return null;
      const date = new Date(ms);
      return Number.isNaN(date.getTime()) ? null : `ISODate(${JSON.stringify(date.toISOString())})`;
    }
    case "$numberLong":
      return `NumberLong(${JSON.stringify(String(inner))})`;
    case "$numberDecimal":
      return `NumberDecimal(${JSON.stringify(String(inner))})`;
    case "$numberInt":
      return String(inner);
    case "$numberDouble":
      return Number.isFinite(Number(inner)) ? String(inner) : null;
    default:
      return null;
  }
}

/** The completion options of sampled values; inside a string only strings, written without their quotes. */
export function valueOptions(values: unknown[], ctx: Pick<ValueContext, "quote">, nextChar: string): Completion[] {
  const options: Completion[] = [];
  for (const value of values) {
    if (ctx.quote) {
      if (typeof value !== "string") continue;
      const escaped = JSON.stringify(value).slice(1, -1);
      const inner = ctx.quote === "'" ? escaped.replace(/\\"/g, '"').replace(/'/g, "\\'") : escaped;
      options.push({ label: value, apply: inner + (nextChar === ctx.quote ? "" : ctx.quote), type: "text", detail: "value", boost: 2 });
      continue;
    }
    const literal = valueLiteral(value);
    if (literal !== null) options.push({ label: literal, type: typeof value === "string" ? "text" : "constant", detail: "value", boost: 2 });
  }
  return options;
}

/** A schema whose `collection` also has `extraFields` (paths sampled from the documents shown). */
export function withFields(schema: CompletionSchema, collection: string, extraFields: readonly string[]): CompletionSchema {
  if (extraFields.length === 0) return schema;
  const table = schema.tables.find((item) => item.name === collection);
  const known = new Set(table?.columns.map((column) => column.name) ?? []);
  const added = extraFields.filter((field) => !known.has(field)).map((name) => ({ name, type: "", pk: name === "_id" }));
  const merged = { name: collection, kind: "collection", columns: [...(table?.columns ?? []), ...added] };
  return { ...schema, tables: [merged, ...schema.tables.filter((item) => item !== table)] };
}

/** Key completions (fields, operators, collation options) of a field; synchronous, for tests and the source. */
export function queryKeyCompletions(
  field: QueryBarField,
  collection: string,
  schema: CompletionSchema,
  text: string,
  pos: number,
  explicit: boolean,
): CompletionResult | null {
  const { text: source, shift } = bracedText(text);
  if (field === "collation") return collationCompletions(source, pos + shift, shift, explicit);
  const frame = shellFrame(field, collection);
  const doc = frame.before + source + frame.after;
  const offset = frame.before.length + shift;
  const result = mongoCompletions(schema, doc, pos + offset, explicit);
  if (!result) return null;
  return { ...result, from: result.from - offset, to: result.to === undefined ? undefined : result.to - offset };
}

function collationCompletions(source: string, end: number, shift: number, explicit: boolean): CompletionResult | null {
  let k = end;
  while (k > 0 && IDENT.test(source[k - 1])) k--;
  let b = k;
  while (b > 0 && /\s/.test(source[b - 1])) b--;
  if (b === 0 || !"{,".includes(source[b - 1])) return null;
  if (!explicit && k === end) return null;
  const options = COLLATION_KEYS.map(([label, info]) => ({ label, info, type: "property" }));
  return { from: k - shift, to: end - shift, options, validFor: /^[\w$]*$/ };
}

export interface QueryBarCompletionOptions {
  field: QueryBarField;
  collection: string;
  /** Collections and their sampled fields (`completion_schema`), with the fields of the documents shown. */
  schema: () => CompletionSchema;
  /** The distinct values of a field (filter only); resolves [] when they cannot be read. */
  values?: (field: string) => Promise<unknown[]>;
}

/** The CodeMirror completion source of one query bar field. */
export function queryBarCompletionSource(options: QueryBarCompletionOptions): CompletionSource {
  return async (context: CompletionContext): Promise<CompletionResult | null> => {
    const text = context.state.doc.toString();
    const keys = queryKeyCompletions(options.field, options.collection, options.schema(), text, context.pos, context.explicit);
    if (keys) return keys;
    if (options.field !== "filter") return null;
    const value = filterValueContext(text, context.pos);
    if (!value) return null;
    if (!context.explicit && value.prefix === "" && !value.quote) {
      // Right after ":" only an explicit request (Ctrl+Space) or a typed character opens the list.
      return null;
    }
    const nextChar = text[context.pos] ?? "";
    const constructors: Completion[] = value.quote
      ? []
      : BSON_CONSTRUCTORS.map((item) => ({ label: item.label, apply: item.apply, info: item.info, type: "function", detail: "BSON" }));
    let sampled: Completion[] = [];
    if (options.values) {
      const values = await options.values(value.field);
      if (context.aborted) return null;
      sampled = valueOptions(values, value, nextChar);
    }
    const all = [...sampled, ...constructors];
    if (all.length === 0) return null;
    return { from: value.from, to: value.to, options: all, validFor: value.quote ? /^[^"'\\]*$/ : /^[\w$.\-+]*$/ };
  };
}
