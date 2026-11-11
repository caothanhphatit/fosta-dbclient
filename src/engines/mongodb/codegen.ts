/**
 * Code export, without DOM: a find or an aggregation (canonical Extended JSON, as `parseMongoShell` writes it) as a
 * mongosh command, a Node.js script (the official `mongodb` driver) or a Python script (PyMongo).
 *
 * Every BSON value keeps its exact type in each language: `$oid` is an ObjectId, `$date` a Date / timezone-aware
 * datetime, `$numberLong` a Long / Int64, `$numberDecimal` a Decimal128, `$binary` a Binary (a UUID for subtype 04),
 * `$timestamp`, `$regularExpression`, `$minKey` / `$maxKey` their driver classes, and a double that JavaScript would
 * store as an int32 (a whole number) is spelled as a Double. Key order is kept as written. Only the imports a script
 * uses are emitted. Values are pretty-printed with a stable layout: a document or array stays on one line while it
 * fits the line width, and otherwise puts one entry per line.
 *
 * The mongosh output reads back through `parseMongoShell` to the same Extended JSON (except doubles with a whole value
 * and regexes that JavaScript cannot express, which mongosh spells `Double(…)` / `BSONRegExp(…)` and the parser refuses).
 */
import { parseMongoShell } from "../../lang/mongo/shell";
import { buildQuery, type QueryState, type Target } from "./queryModel";

export type CodeLanguage = "mongosh" | "node" | "python";

export const CODE_LANGUAGES: readonly { id: CodeLanguage; label: string; extension: string }[] = [
  { id: "mongosh", label: "mongosh", extension: "js" },
  { id: "node", label: "Node.js", extension: "mjs" },
  { id: "python", label: "Python", extension: "py" },
];

/** The parts of a find, each document as Extended JSON text; empty or missing parts are left out. */
export interface CodegenFind {
  filter?: string | null;
  projection?: string | null;
  sort?: string | null;
  collation?: string | null;
  skip?: number;
  limit?: number;
}

export interface CodegenOptions {
  allowDiskUse?: boolean;
  maxTimeMS?: number;
  /** Extended JSON text; a find's own collation wins over this one. */
  collation?: string | null;
}

export interface CodegenRequest {
  database: string;
  collection: string;
  kind: "find" | "aggregate";
  find?: CodegenFind;
  /** One Extended JSON document per stage. */
  pipeline?: readonly string[];
  options?: CodegenOptions;
}

export type CodegenResult = { ok: true; code: string } | { ok: false; error: string };

/** The code of `request` in `language`, or why the request cannot be written (Extended JSON that does not read). */
export function generateCode(language: CodeLanguage, request: CodegenRequest): CodegenResult {
  try {
    const ctx = newContext(language);
    const code = language === "python" ? pythonScript(request, ctx) : language === "node" ? nodeScript(request, ctx) : mongoshCommand(request, ctx);
    return { ok: true, code };
  } catch (error) {
    if (error instanceof CodegenError) return { ok: false, error: error.message };
    throw error;
  }
}

/** One Extended JSON value written in `language` (a literal, without imports), pretty-printed from column 0. */
export function formatValue(language: CodeLanguage, ejson: string): CodegenResult {
  try {
    const ctx = newContext(language);
    return { ok: true, code: render(readEjson(ejson), ctx, 0, 0) };
  } catch (error) {
    if (error instanceof CodegenError) return { ok: false, error: error.message };
    throw error;
  }
}

/** The query bar's find (as typed, mongosh syntax) as code; the error names the field that does not parse. */
export function exportFindQuery(language: CodeLanguage, state: QueryState, target: Target, options: CodegenOptions = {}): CodegenResult {
  const built = buildQuery(state, target);
  if (!built.ok) return { ok: false, error: `${built.error.field}: ${built.error.message}` };
  const { filter, projection, sort, collation, skip, limit } = built.query.find;
  return generateCode(language, { ...target, kind: "find", find: { filter, projection, sort, collation, skip, limit }, options });
}

/** A pipeline of stages as typed (mongosh syntax, one document per stage) as code; the error names the stage. */
export function exportPipeline(language: CodeLanguage, target: Target, stages: readonly string[], options: CodegenOptions = {}): CodegenResult {
  const pipeline: string[] = [];
  for (const [index, text] of stages.entries()) {
    const parsed = parseMongoShell(text, { expect: "document" });
    if (!parsed.ok) return { ok: false, error: `Stage ${index + 1}: ${parsed.error}` };
    pipeline.push(parsed.json);
  }
  return generateCode(language, { ...target, kind: "aggregate", pipeline, options });
}

class CodegenError extends Error {}

// --- Reading Extended JSON ------------------------------------------------------------------------------------------

type Value =
  | { t: "doc"; entries: [string, Value][] }
  | { t: "arr"; items: Value[] }
  | { t: "tuple"; items: Value[] }
  | { t: "str"; v: string }
  | { t: "bool"; v: boolean }
  | { t: "null" }
  | { t: "int"; v: number; wrapped: boolean }
  | { t: "double"; v: number }
  | { t: "long"; v: string }
  | { t: "decimal"; v: string }
  | { t: "oid"; v: string }
  | { t: "date"; ms: bigint }
  | { t: "binary"; base64: string; subType: number }
  | { t: "timestamp"; time: number; inc: number }
  | { t: "regex"; pattern: string; options: string }
  | { t: "minKey" }
  | { t: "maxKey" };

type Raw =
  { k: "obj"; entries: [string, Raw][] } | { k: "arr"; items: Raw[] } | { k: "str"; v: string } | { k: "num"; text: string } | { k: "lit"; v: boolean | null };

const INT32_MIN = -2147483648;
const INT32_MAX = 2147483647;
const INT64_MIN = -(2n ** 63n);
const INT64_MAX = 2n ** 63n - 1n;
const UINT32_MAX = 4294967295;
const JS_DATE_LIMIT = 8_640_000_000_000_000n;

function readEjson(text: string): Value {
  return toValue(readJson(text));
}

/** A JSON reader that keeps key order (JSON.parse moves integer-like keys first) and the text of numbers. */
function readJson(text: string): Raw {
  let i = 0;
  const fail = (what: string): never => {
    throw new CodegenError(`Invalid Extended JSON at offset ${i}: ${what}`);
  };
  const ws = () => {
    while (i < text.length && (text[i] === " " || text[i] === "\t" || text[i] === "\n" || text[i] === "\r")) i++;
  };
  const token = (pattern: RegExp): string | undefined => {
    pattern.lastIndex = i;
    const match = pattern.exec(text);
    if (!match) return undefined;
    i += match[0].length;
    return match[0];
  };
  const string = (): string => {
    const raw = token(/"(?:[^"\\]|\\.)*"/y);
    if (raw === undefined) return fail("expected a string");
    try {
      return JSON.parse(raw) as string; // refuses raw control characters and bad escapes
    } catch {
      return fail("invalid string");
    }
  };
  const value = (): Raw => {
    ws();
    const ch = text[i];
    if (ch === "{") {
      i++;
      const entries: [string, Raw][] = [];
      ws();
      if (text[i] === "}") {
        i++;
        return { k: "obj", entries };
      }
      for (;;) {
        ws();
        const key = string();
        ws();
        if (text[i] !== ":") fail("expected ':'");
        i++;
        entries.push([key, value()]);
        ws();
        if (text[i] === ",") i++;
        else if (text[i] === "}") {
          i++;
          return { k: "obj", entries };
        } else fail("expected ',' or '}'");
      }
    }
    if (ch === "[") {
      i++;
      const items: Raw[] = [];
      ws();
      if (text[i] === "]") {
        i++;
        return { k: "arr", items };
      }
      for (;;) {
        items.push(value());
        ws();
        if (text[i] === ",") i++;
        else if (text[i] === "]") {
          i++;
          return { k: "arr", items };
        } else fail("expected ',' or ']'");
      }
    }
    if (ch === '"') return { k: "str", v: string() };
    const number = token(/-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/y);
    if (number !== undefined) return { k: "num", text: number };
    const literal = token(/true|false|null/y);
    if (literal !== undefined) return { k: "lit", v: literal === "null" ? null : literal === "true" };
    return fail(ch === undefined ? "unexpected end" : "expected a value");
  };
  const result = value();
  ws();
  if (i < text.length) fail("unexpected text after the value");
  return result;
}

function toValue(raw: Raw): Value {
  switch (raw.k) {
    case "str":
      return { t: "str", v: raw.v };
    case "lit":
      return raw.v === null ? { t: "null" } : { t: "bool", v: raw.v };
    case "num":
      return plainNumber(raw.text);
    case "arr":
      return { t: "arr", items: raw.items.map(toValue) };
    case "obj":
      return wrapper(raw.entries) ?? { t: "doc", entries: raw.entries.map(([key, value]) => [key, toValue(value)]) };
  }
}

/** A plain JSON number as the backend reads it: an integer is an int32 or an int64, anything else a double. */
function plainNumber(text: string): Value {
  if (/[.eE]/.test(text)) return { t: "double", v: Number(text) };
  const big = BigInt(text);
  if (big >= BigInt(INT32_MIN) && big <= BigInt(INT32_MAX)) return { t: "int", v: Number(big), wrapped: false };
  if (big >= INT64_MIN && big <= INT64_MAX) return { t: "long", v: big.toString() };
  return { t: "double", v: Number(text) };
}

/** The fields of a sub-document that has exactly `names` (in any order), or undefined. */
function fields(raw: Raw, names: string[]): Record<string, Raw> | undefined {
  if (raw.k !== "obj" || raw.entries.length !== names.length) return undefined;
  const found = Object.fromEntries(raw.entries) as Record<string, Raw>;
  return names.every((name) => Object.hasOwn(found, name)) ? found : undefined;
}

const str = (raw: Raw | undefined): string | undefined => (raw?.k === "str" ? raw.v : undefined);
const uint32 = (raw: Raw | undefined): number | undefined => {
  if (raw?.k !== "num" || !/^\d+$/.test(raw.text)) return undefined;
  const v = Number(raw.text);
  return v <= UINT32_MAX ? v : undefined;
};

/** The BSON value of a canonical (or relaxed) Extended JSON wrapper, undefined for a plain document. */
function wrapper(entries: [string, Raw][]): Value | undefined {
  if (entries.length === 0) return undefined;
  const [key, inner] = entries[0];
  if (!key.startsWith("$")) return undefined;
  const only = entries.length === 1;
  const bad = (): never => {
    throw new CodegenError(`Invalid ${key} value`);
  };
  switch (key) {
    case "$oid": {
      const hex = str(inner);
      return only && hex !== undefined && /^[0-9a-fA-F]{24}$/.test(hex) ? { t: "oid", v: hex.toLowerCase() } : bad();
    }
    case "$date": {
      if (!only) return bad();
      let ms: bigint | undefined;
      const long = fields(inner, ["$numberLong"]);
      if (long && str(long.$numberLong) !== undefined && /^-?\d+$/.test(str(long.$numberLong) ?? "")) ms = BigInt(str(long.$numberLong) ?? "");
      else if (inner.k === "num" && /^-?\d+$/.test(inner.text)) ms = BigInt(inner.text);
      else if (inner.k === "str" && !Number.isNaN(Date.parse(inner.v))) ms = BigInt(Date.parse(inner.v));
      return ms !== undefined && ms >= INT64_MIN && ms <= INT64_MAX ? { t: "date", ms } : bad();
    }
    case "$numberLong": {
      const text = str(inner);
      if (!only || text === undefined || !/^-?\d+$/.test(text)) return bad();
      const big = BigInt(text);
      return big >= INT64_MIN && big <= INT64_MAX ? { t: "long", v: big.toString() } : bad();
    }
    case "$numberInt": {
      const text = str(inner);
      if (!only || text === undefined || !/^-?\d+$/.test(text)) return bad();
      const v = Number(text);
      return v >= INT32_MIN && v <= INT32_MAX ? { t: "int", v: v === 0 ? 0 : v, wrapped: true } : bad();
    }
    case "$numberDouble": {
      const text = str(inner);
      if (!only || text === undefined) return bad();
      if (text === "NaN") return { t: "double", v: NaN };
      if (text === "Infinity" || text === "-Infinity") return { t: "double", v: text === "Infinity" ? Infinity : -Infinity };
      return /^-?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$/.test(text) ? { t: "double", v: Number(text) } : bad();
    }
    case "$numberDecimal": {
      const text = str(inner);
      return only && text !== undefined ? { t: "decimal", v: text } : bad();
    }
    case "$binary": {
      const parts = only ? fields(inner, ["base64", "subType"]) : undefined;
      const base64 = str(parts?.base64);
      const subType = str(parts?.subType);
      if (base64 === undefined || subType === undefined || !/^[0-9a-fA-F]{1,2}$/.test(subType) || !isBase64(base64)) return bad();
      return { t: "binary", base64, subType: parseInt(subType, 16) };
    }
    case "$uuid": {
      const text = str(inner);
      if (!only || text === undefined || !/^([0-9a-fA-F]{32}|[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/.test(text))
        return bad();
      return { t: "binary", base64: hexToBase64(text.replace(/-/g, "")), subType: 4 };
    }
    case "$timestamp": {
      const parts = only ? fields(inner, ["t", "i"]) : undefined;
      const time = uint32(parts?.t);
      const inc = uint32(parts?.i);
      return time !== undefined && inc !== undefined ? { t: "timestamp", time, inc } : bad();
    }
    case "$regularExpression": {
      const parts = only ? fields(inner, ["pattern", "options"]) : undefined;
      const pattern = str(parts?.pattern);
      const options = str(parts?.options);
      return pattern !== undefined && options !== undefined ? { t: "regex", pattern, options } : bad();
    }
    case "$minKey":
    case "$maxKey":
      return only && inner.k === "num" && inner.text === "1" ? { t: key === "$minKey" ? "minKey" : "maxKey" } : bad();
    case "$code":
    case "$symbol":
    case "$dbPointer":
    case "$undefined":
      throw new CodegenError(`Extended JSON ${key} values cannot be exported`);
    default:
      return undefined;
  }
}

function isBase64(text: string): boolean {
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(text) || text.length % 4 !== 0) return false;
  try {
    atob(text);
    return true;
  } catch {
    return false;
  }
}

function hexToBase64(hex: string): string {
  let bytes = "";
  for (let j = 0; j < hex.length; j += 2) bytes += String.fromCharCode(parseInt(hex.slice(j, j + 2), 16));
  return btoa(bytes);
}

function base64ToHex(base64: string): string {
  let hex = "";
  for (const ch of atob(base64)) hex += ch.charCodeAt(0).toString(16).padStart(2, "0");
  return hex;
}

/** The dashed UUID text of a 16-byte subtype 04 binary, undefined for any other binary. */
function uuidText(value: { base64: string; subType: number }): string | undefined {
  if (value.subType !== 4) return undefined;
  const hex = base64ToHex(value.base64);
  if (hex.length !== 32) return undefined;
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

// --- Writing values -------------------------------------------------------------------------------------------------

interface Context {
  lang: CodeLanguage;
  /** Indent step and line width: 2 / 80 for JavaScript, 4 / 88 (Black) for Python. */
  unit: number;
  width: number;
  /** Names to import, e.g. "ObjectId"; for Python "module:name". */
  used: Set<string>;
  flat: Map<Value, string>;
}

function newContext(lang: CodeLanguage): Context {
  const python = lang === "python";
  return { lang, unit: python ? 4 : 2, width: python ? 88 : 80, used: new Set(), flat: new Map() };
}

const pad = (n: number) => " ".repeat(n);
/** Characters that are valid but invisible in source: DEL, the C1 controls, the line separators and the BOM. */
const invisible = (c: number) => c === 0x7f || (c >= 0x80 && c <= 0x9f) || c === 0x2028 || c === 0x2029 || c === 0xfeff;

/** A double-quoted JavaScript string (JSON.stringify also escapes controls and lone surrogates). */
const jsString = (s: string) =>
  JSON.stringify(s).replace(/[^\x20-\x7e]/g, (ch) => (invisible(ch.charCodeAt(0)) ? `\\u${ch.charCodeAt(0).toString(16).padStart(4, "0")}` : ch));
const jsKey = (key: string) => (/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(key) ? key : jsString(key));

function pyString(s: string): string {
  let out = '"';
  for (const ch of s) {
    const c = ch.codePointAt(0) ?? 0;
    if (ch === "\\") out += "\\\\";
    else if (ch === '"') out += '\\"';
    else if (ch === "\n") out += "\\n";
    else if (ch === "\r") out += "\\r";
    else if (ch === "\t") out += "\\t";
    else if (c < 0x20 || c === 0x7f || (c >= 0x80 && c <= 0x9f)) out += `\\x${c.toString(16).padStart(2, "0")}`;
    else if (c === 0x2028 || c === 0x2029 || c === 0xfeff || (c >= 0xd800 && c <= 0xdfff)) out += `\\u${c.toString(16).padStart(4, "0")}`;
    else out += ch;
  }
  return `${out}"`;
}

const quote = (ctx: Context, s: string) => (ctx.lang === "python" ? pyString(s) : jsString(s));
const key = (ctx: Context, k: string) => (ctx.lang === "python" ? pyString(k) : jsKey(k));

/** `value` at column `col` of a line indented by `indent`, on one line while it fits (`col` counts a trailing comma). */
function render(value: Value, ctx: Context, indent: number, col: number): string {
  const scalar = atom(value, ctx);
  if (scalar !== undefined) return scalar;
  const flat = flatText(value, ctx);
  return col + flat.length <= ctx.width ? flat : broken(value, ctx, indent);
}

/** A document, array or tuple with one entry per line (a trailing comma in Python). */
function broken(value: Value, ctx: Context, indent: number): string {
  const inner = indent + ctx.unit;
  let parts: string[];
  let open: string;
  let close: string;
  if (value.t === "doc") {
    [open, close] = ["{", "}"];
    parts = value.entries.map(([k, v]) => {
      const head = `${key(ctx, k)}: `;
      return head + render(v, ctx, inner, inner + head.length + 1);
    });
  } else if (value.t === "arr" || value.t === "tuple") {
    [open, close] = value.t === "arr" ? ["[", "]"] : ["(", ")"];
    parts = value.items.map((v) => render(v, ctx, inner, inner + 1));
  } else {
    return atom(value, ctx) ?? "";
  }
  if (parts.length === 0) return `${open}${close}`;
  const comma = ctx.lang === "python" ? "," : "";
  return `${open}\n${parts.map((part) => pad(inner) + part).join(",\n")}${comma}\n${pad(indent)}${close}`;
}

function flatText(value: Value, ctx: Context): string {
  const cached = ctx.flat.get(value);
  if (cached !== undefined) return cached;
  let text: string;
  if (value.t === "doc") {
    const inner = value.entries.map(([k, v]) => `${key(ctx, k)}: ${flatText(v, ctx)}`).join(", ");
    text = inner === "" ? "{}" : ctx.lang === "python" ? `{${inner}}` : `{ ${inner} }`;
  } else if (value.t === "arr") {
    text = `[${value.items.map((v) => flatText(v, ctx)).join(", ")}]`;
  } else if (value.t === "tuple") {
    text = `(${value.items.map((v) => flatText(v, ctx)).join(", ")})`;
  } else {
    text = atom(value, ctx) ?? "";
  }
  ctx.flat.set(value, text);
  return text;
}

/** A scalar or BSON wrapper in the context's language, undefined for a document, array or tuple. */
function atom(value: Value, ctx: Context): string | undefined {
  switch (value.t) {
    case "doc":
    case "arr":
    case "tuple":
      return undefined;
    case "str":
      return quote(ctx, value.v);
    case "null":
      return ctx.lang === "python" ? "None" : "null";
    case "bool":
      return ctx.lang === "python" ? (value.v ? "True" : "False") : String(value.v);
    default:
      return ctx.lang === "python" ? pythonAtom(value, ctx) : jsAtom(value, ctx);
  }
}

const use = (ctx: Context, name: string) => {
  ctx.used.add(name);
  return name;
};

function jsDouble(v: number, ctx: Context): string {
  if (Number.isNaN(v)) return "NaN";
  if (v === Infinity) return "Infinity";
  if (v === -Infinity) return "-Infinity";
  if (Number.isInteger(v) && v >= INT32_MIN && v <= INT32_MAX) {
    // A whole number in the int32 range is stored as an int32 by mongosh and the Node driver.
    const text = Object.is(v, -0) ? "-0" : String(v);
    return ctx.lang === "node" ? `new ${use(ctx, "Double")}(${text})` : `Double(${text})`;
  }
  const text = String(v);
  // Past 2^53 a plain integer literal is refused by the shell parser (it would be rounded); the exponent form is exact.
  return Number.isInteger(v) && !/e/.test(text) && !Number.isSafeInteger(v) ? v.toExponential() : text;
}

function jsDate(ms: bigint, ctx: Context): string {
  if (ms > JS_DATE_LIMIT || ms < -JS_DATE_LIMIT) throw new CodegenError(`The date ${ms} ms is outside the range of a JavaScript Date`);
  const iso = new Date(Number(ms)).toISOString();
  const node = ctx.lang === "node";
  if (/^\d{4}-/.test(iso)) return node ? `new Date(${jsString(iso)})` : `ISODate(${jsString(iso)})`;
  return `new Date(${ms})`;
}

/** 
, 
 and the two Unicode line separators: none may appear in a regex literal. */
const LINE_BREAKS = new Set([0x0a, 0x0d, 0x2028, 0x2029]);

/** Whether `/pattern/options` is a JavaScript regex literal that reads back as exactly this pattern. */
function regexLiteralSafe(pattern: string, options: string): boolean {
  if (pattern === "" || pattern.startsWith("*") || [...pattern].some((ch) => LINE_BREAKS.has(ch.charCodeAt(0)))) return false;
  let inClass = false;
  for (let j = 0; j < pattern.length; j++) {
    const ch = pattern[j];
    if (ch === "\\") {
      j++;
      if (j >= pattern.length) return false;
    } else if (ch === "[") inClass = true;
    else if (ch === "]") inClass = false;
    else if (ch === "/" && !inClass) return false;
  }
  return jsRegexValid(pattern, options);
}

function jsRegexValid(pattern: string, options: string): boolean {
  if (!/^[imsu]*$/.test(options)) return false;
  try {
    new RegExp(pattern, options);
    return true;
  } catch {
    return false;
  }
}

function jsAtom(value: Value, ctx: Context): string {
  const node = ctx.lang === "node";
  switch (value.t) {
    case "int":
      return value.wrapped && !node ? `NumberInt(${value.v})` : String(value.v);
    case "double":
      return jsDouble(value.v, ctx);
    case "long":
      return node ? `${use(ctx, "Long")}.fromString(${jsString(value.v)})` : `NumberLong(${jsString(value.v)})`;
    case "decimal":
      return node ? `${use(ctx, "Decimal128")}.fromString(${jsString(value.v)})` : `NumberDecimal(${jsString(value.v)})`;
    case "oid":
      return node ? `new ${use(ctx, "ObjectId")}(${jsString(value.v)})` : `ObjectId(${jsString(value.v)})`;
    case "date":
      return jsDate(value.ms, ctx);
    case "binary": {
      const uuid = uuidText(value);
      if (uuid) return node ? `new ${use(ctx, "UUID")}(${jsString(uuid)})` : `UUID(${jsString(uuid)})`;
      return node
        ? `${use(ctx, "Binary")}.createFromBase64(${jsString(value.base64)}, ${value.subType})`
        : `BinData(${value.subType}, ${jsString(value.base64)})`;
    }
    case "timestamp":
      return `${node ? `new ${use(ctx, "Timestamp")}` : "Timestamp"}({ t: ${value.time}, i: ${value.inc} })`;
    case "regex": {
      const args = value.options ? `${jsString(value.pattern)}, ${jsString(value.options)}` : jsString(value.pattern);
      if (node) return `new ${use(ctx, "BSONRegExp")}(${args})`;
      if (regexLiteralSafe(value.pattern, value.options)) return `/${value.pattern}/${value.options}`;
      return jsRegexValid(value.pattern, value.options) ? `RegExp(${args})` : `BSONRegExp(${args})`;
    }
    case "minKey":
      return node ? `new ${use(ctx, "MinKey")}()` : "MinKey()";
    case "maxKey":
      return node ? `new ${use(ctx, "MaxKey")}()` : "MaxKey()";
    default:
      return "";
  }
}

function pythonDouble(v: number): string {
  if (Number.isNaN(v)) return 'float("nan")';
  if (v === Infinity) return 'float("inf")';
  if (v === -Infinity) return 'float("-inf")';
  if (Object.is(v, -0)) return "-0.0";
  const text = String(v);
  return /[.e]/.test(text) ? text : `${text}.0`;
}

function pythonDate(ms: bigint, ctx: Context): string {
  if (ms <= JS_DATE_LIMIT && ms >= -JS_DATE_LIMIT) {
    const d = new Date(Number(ms));
    const year = d.getUTCFullYear();
    if (year >= 1 && year <= 9999) {
      const parts = [year, d.getUTCMonth() + 1, d.getUTCDate(), d.getUTCHours(), d.getUTCMinutes(), d.getUTCSeconds(), d.getUTCMilliseconds() * 1000];
      while (parts.length > 3 && parts[parts.length - 1] === 0) parts.pop();
      use(ctx, "datetime:datetime");
      use(ctx, "datetime:timezone");
      return `datetime(${parts.join(", ")}, tzinfo=timezone.utc)`;
    }
  }
  // Outside datetime's years 1 to 9999: PyMongo's millisecond type.
  use(ctx, "bson.datetime_ms:DatetimeMS");
  return `DatetimeMS(${ms})`;
}

function pythonAtom(value: Value, ctx: Context): string {
  const bson = (name: string) => {
    use(ctx, `bson:${name}`);
    return name;
  };
  switch (value.t) {
    case "int":
      return String(value.v);
    case "double":
      return pythonDouble(value.v);
    case "long":
      return `${bson("Int64")}(${value.v})`;
    case "decimal":
      return `${bson("Decimal128")}(${pyString(value.v)})`;
    case "oid":
      return `${bson("ObjectId")}(${pyString(value.v)})`;
    case "date":
      return pythonDate(value.ms, ctx);
    case "binary": {
      const uuid = uuidText(value);
      if (uuid) {
        use(ctx, "uuid:UUID");
        return `${bson("Binary")}.from_uuid(UUID(${pyString(uuid)}))`;
      }
      const bytes = `bytes.fromhex(${pyString(base64ToHex(value.base64))})`;
      return `${bson("Binary")}(${bytes}${value.subType === 0 ? "" : `, ${value.subType}`})`;
    }
    case "timestamp":
      return `${bson("Timestamp")}(${value.time}, ${value.inc})`;
    case "regex":
      return `${bson("Regex")}(${pyString(value.pattern)}${value.options ? `, ${pyString(value.options)}` : ""})`;
    case "minKey":
      return `${bson("MinKey")}()`;
    case "maxKey":
      return `${bson("MaxKey")}()`;
    default:
      return "";
  }
}

// --- Requests -------------------------------------------------------------------------------------------------------

interface Plan {
  /** The filter (an empty document when there is none). */
  filter: Value;
  projection?: Value;
  sort?: Value;
  collation?: Value;
  skip?: number;
  limit?: number;
  pipeline?: Value[];
  allowDiskUse: boolean;
  maxTimeMS?: number;
}

const EMPTY_DOC: Value = { t: "doc", entries: [] };

function documentOf(text: string | null | undefined, what: string): Value | undefined {
  if (text === null || text === undefined || text.trim() === "") return undefined;
  const value = readEjson(text);
  if (value.t !== "doc") throw new CodegenError(`The ${what} must be a document`);
  return value.entries.length === 0 ? undefined : value;
}

const count = (n: number | undefined) => (n !== undefined && Number.isFinite(n) && Math.floor(n) > 0 ? Math.floor(n) : undefined);

function plan(request: CodegenRequest): Plan {
  const find = request.find ?? {};
  const options = request.options ?? {};
  const result: Plan = {
    filter: EMPTY_DOC,
    collation: documentOf(find.collation, "collation") ?? documentOf(options.collation, "collation"),
    allowDiskUse: options.allowDiskUse === true,
    maxTimeMS: count(options.maxTimeMS),
  };
  if (request.kind === "aggregate") {
    result.pipeline = (request.pipeline ?? []).map((text, index) => {
      const stage = readEjson(text);
      if (stage.t !== "doc") throw new CodegenError(`Stage ${index + 1} must be a document`);
      return stage;
    });
  } else {
    result.filter = documentOf(find.filter, "filter") ?? EMPTY_DOC;
    result.projection = documentOf(find.projection, "projection");
    result.sort = documentOf(find.sort, "sort");
    result.skip = count(find.skip);
    result.limit = count(find.limit);
  }
  return result;
}

const int = (v: number): Value => ({ t: "int", v, wrapped: false });

/** The aggregate options document, undefined when there are none. */
function aggregateOptions(p: Plan): Value | undefined {
  const entries: [string, Value][] = [];
  if (p.allowDiskUse) entries.push(["allowDiskUse", { t: "bool", v: true }]);
  if (p.maxTimeMS !== undefined) entries.push(["maxTimeMS", int(p.maxTimeMS)]);
  if (p.collation) entries.push(["collation", p.collation]);
  return entries.length ? { t: "doc", entries } : undefined;
}

interface Call {
  name: string;
  args: Value[];
  /** The first argument always breaks (a pipeline, one stage per line). */
  breakFirst?: boolean;
}

/** The cursor calls of a find or an aggregation, in the order mongosh and the Node driver chain them. */
function jsCalls(p: Plan, project: string): Call[] {
  if (p.pipeline) {
    const options = aggregateOptions(p);
    return [
      { name: "aggregate", args: options ? [{ t: "arr", items: p.pipeline }, options] : [{ t: "arr", items: p.pipeline }], breakFirst: p.pipeline.length > 0 },
    ];
  }
  const calls: Call[] = [];
  if (project === "find" && p.projection) calls.push({ name: "find", args: [p.filter, p.projection] });
  else calls.push({ name: "find", args: [p.filter] });
  if (project !== "find" && p.projection) calls.push({ name: project, args: [p.projection] });
  if (p.sort) calls.push({ name: "sort", args: [p.sort] });
  if (p.collation) calls.push({ name: "collation", args: [p.collation] });
  if (p.skip !== undefined) calls.push({ name: "skip", args: [int(p.skip)] });
  if (p.limit !== undefined) calls.push({ name: "limit", args: [int(p.limit)] });
  if (p.maxTimeMS !== undefined) calls.push({ name: "maxTimeMS", args: [int(p.maxTimeMS)] });
  if (p.allowDiskUse) calls.push({ name: "allowDiskUse", args: [] });
  return calls;
}

/** `.name(args)` on a chain line indented by `indent`: on one line while it fits, else hugging the first argument. */
function jsCall(call: Call, ctx: Context, indent: number, end: string): string {
  const head = `${pad(indent)}.${call.name}(`;
  if (!call.breakFirst) {
    const flat = call.args.map((arg) => flatText(arg, ctx)).join(", ");
    if (head.length + flat.length + 1 + end.length <= ctx.width) return `${head}${flat})${end}`;
  }
  const [first, ...rest] = call.args;
  const firstText = call.breakFirst ? broken(first, ctx, indent) : render(first, ctx, indent, head.length + 1);
  const restText = rest.map((arg) => `, ${render(arg, ctx, indent, indent + 1)}`).join("");
  return `${head}${firstText}${restText})${end}`;
}

function mongoshCommand(request: CodegenRequest, ctx: Context): string {
  const p = plan(request);
  const calls = jsCalls(p, "find");
  const lines = [`db.getSiblingDB(${jsString(request.database)})`, `  .getCollection(${jsString(request.collection)})`];
  calls.forEach((call, index) => lines.push(jsCall(call, ctx, 2, index === calls.length - 1 ? ";" : "")));
  return lines.join("\n");
}

function nodeScript(request: CodegenRequest, ctx: Context): string {
  const p = plan(request);
  const calls = [...jsCalls(p, "project"), { name: "toArray", args: [] }];
  const chain = calls.map((call, index) => jsCall(call, ctx, 6, index === calls.length - 1 ? ";" : "")).join("\n");
  const names = ["MongoClient", ...ctx.used].sort();
  const oneLine = `import { ${names.join(", ")} } from "mongodb";`;
  const imports = oneLine.length <= ctx.width ? oneLine : `import {\n${names.map((name) => `  ${name},`).join("\n")}\n} from "mongodb";`;
  return [
    imports,
    "",
    'const client = new MongoClient(process.env.MONGODB_URI ?? "mongodb://localhost:27017");',
    "",
    "async function main() {",
    "  try {",
    `    const collection = client.db(${jsString(request.database)}).collection(${jsString(request.collection)});`,
    "    const documents = await collection",
    chain,
    "    console.log(documents);",
    "  } finally {",
    "    await client.close();",
    "  }",
    "}",
    "",
    "main().catch(console.error);",
  ].join("\n");
}

interface PyArg {
  keyword?: string;
  value: Value;
  breaks?: boolean;
}

/** `callee(args)` at column `col`: on one line while it fits, else one argument per line with a trailing comma. */
function pyCall(callee: string, args: PyArg[], ctx: Context, col: number): string {
  const label = (arg: PyArg) => (arg.keyword ? `${arg.keyword}=` : "");
  if (!args.some((arg) => arg.breaks)) {
    const flat = args.map((arg) => label(arg) + flatText(arg.value, ctx)).join(", ");
    if (col + callee.length + flat.length + 2 <= ctx.width) return `${callee}(${flat})`;
  }
  const lines = args.map((arg) => {
    const head = pad(ctx.unit) + label(arg);
    return `${head}${arg.breaks ? broken(arg.value, ctx, ctx.unit) : render(arg.value, ctx, ctx.unit, head.length + 1)},`;
  });
  return `${callee}(\n${lines.join("\n")}\n)`;
}

function pythonArgs(p: Plan): { method: string; args: PyArg[] } {
  const args: PyArg[] = [];
  if (p.pipeline) {
    args.push({ value: { t: "arr", items: p.pipeline }, breaks: p.pipeline.length > 0 });
    if (p.allowDiskUse) args.push({ keyword: "allowDiskUse", value: { t: "bool", v: true } });
    if (p.maxTimeMS !== undefined) args.push({ keyword: "maxTimeMS", value: int(p.maxTimeMS) });
    if (p.collation) args.push({ keyword: "collation", value: p.collation });
    return { method: "aggregate", args };
  }
  args.push({ value: p.filter });
  if (p.projection) args.push({ keyword: "projection", value: p.projection });
  if (p.sort?.t === "doc") {
    // PyMongo takes the sort as (key, direction) pairs.
    const pairs: Value[] = p.sort.entries.map(([k, v]) => ({ t: "tuple", items: [{ t: "str", v: k }, v] }));
    args.push({ keyword: "sort", value: { t: "arr", items: pairs } });
  }
  if (p.skip !== undefined) args.push({ keyword: "skip", value: int(p.skip) });
  if (p.limit !== undefined) args.push({ keyword: "limit", value: int(p.limit) });
  if (p.collation) args.push({ keyword: "collation", value: p.collation });
  if (p.maxTimeMS !== undefined) args.push({ keyword: "max_time_ms", value: int(p.maxTimeMS) });
  if (p.allowDiskUse) args.push({ keyword: "allow_disk_use", value: { t: "bool", v: true } });
  return { method: "find", args };
}

function pythonImport(module: string, names: string[], width: number): string {
  const line = `from ${module} import ${names.join(", ")}`;
  return line.length <= width ? line : `from ${module} import (\n${names.map((name) => `    ${name},`).join("\n")}\n)`;
}

function pythonScript(request: CodegenRequest, ctx: Context): string {
  const p = plan(request);
  const { method, args } = pythonArgs(p);
  const call = `cursor = ${pyCall(`collection.${method}`, args, ctx, "cursor = ".length)}`;
  const byModule = new Map<string, string[]>();
  for (const entry of [...ctx.used].sort()) {
    const [module, name] = entry.split(":");
    byModule.set(module, [...(byModule.get(module) ?? []), name]);
  }
  const from = (module: string) => (byModule.has(module) ? [pythonImport(module, byModule.get(module) ?? [], ctx.width)] : []);
  const standard = ["import os", ...from("datetime"), ...from("uuid")];
  const external = [...from("bson"), ...from("bson.datetime_ms"), "from pymongo import MongoClient"];
  return [
    ...standard,
    "",
    ...external,
    "",
    'client = MongoClient(os.environ.get("MONGODB_URI", "mongodb://localhost:27017"))',
    `collection = client[${pyString(request.database)}][${pyString(request.collection)}]`,
    "",
    call,
    "for document in cursor:",
    "    print(document)",
  ].join("\n");
}
