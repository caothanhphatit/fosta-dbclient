/**
 * Readable BSON values, without DOM. `mongo.find` returns relaxed Extended JSON; this reads each wrapper as what it is:
 * an ObjectId (its creation time on hover), a date in local time (UTC on hover), Decimal128, Long, Binary and UUID, a
 * regex, a Timestamp, MinKey / MaxKey. `shellText` writes any value back as mongosh syntax (what "Copy value" copies,
 * and what the query bar's parser reads back).
 */
import { isWrapper, type Doc, type Json, type ValueKind } from "./documents";

/** The BSON type of a value as read from relaxed Extended JSON (a plain number is a double or an int32 there). */
export type BsonType =
  | "string"
  | "number"
  | "long"
  | "decimal"
  | "bool"
  | "null"
  | "objectId"
  | "date"
  | "binary"
  | "uuid"
  | "regex"
  | "timestamp"
  | "minKey"
  | "maxKey"
  | "object"
  | "array"
  | "other";

export const BSON_TYPE_LABELS: Record<BsonType, string> = {
  string: "String",
  number: "Number",
  long: "Int64",
  decimal: "Decimal128",
  bool: "Boolean",
  null: "Null",
  objectId: "ObjectId",
  date: "Date",
  binary: "Binary",
  uuid: "UUID",
  regex: "Regex",
  timestamp: "Timestamp",
  minKey: "MinKey",
  maxKey: "MaxKey",
  object: "Object",
  array: "Array",
  other: "Other",
};

const record = (value: Json): Doc => (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Doc) : {});

export function bsonType(value: Json): BsonType {
  if (value === null || value === undefined) return "null";
  if (typeof value === "string") return "string";
  if (typeof value === "number") return "number";
  if (typeof value === "boolean") return "bool";
  if (Array.isArray(value)) return "array";
  if (typeof value !== "object") return "other";
  if (!isWrapper(value)) return "object";
  const [key] = Object.keys(value);
  switch (key) {
    case "$oid":
      return "objectId";
    case "$date":
      return "date";
    case "$numberLong":
      return "long";
    case "$numberDecimal":
      return "decimal";
    case "$numberInt":
    case "$numberDouble":
      return "number";
    case "$uuid":
      return "uuid";
    case "$binary":
      return isUuidSubtype(record(value.$binary).subType) ? "uuid" : "binary";
    case "$regularExpression":
      return "regex";
    case "$timestamp":
      return "timestamp";
    case "$minKey":
      return "minKey";
    case "$maxKey":
      return "maxKey";
    default:
      return "other";
  }
}

function isUuidSubtype(subType: Json): boolean {
  const n = parseInt(String(subType ?? ""), 16);
  return n === 3 || n === 4;
}

/** The time an ObjectId was made (its first four bytes, seconds since the epoch); null when it is not 24 hex digits. */
export function objectIdTime(hex: string): Date | null {
  if (!/^[0-9a-fA-F]{24}$/.test(hex)) return null;
  return new Date(parseInt(hex.slice(0, 8), 16) * 1000);
}

/** The instant of an Extended JSON date (`"2026-…Z"`, `{ $numberLong: "…" }` or milliseconds); null when invalid. */
export function dateOf(inner: Json): Date | null {
  const ms = typeof inner === "string" ? Date.parse(inner) : Number(typeof inner === "object" && inner !== null ? (inner as Doc).$numberLong : inner);
  if (!Number.isFinite(ms)) return null;
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? null : date;
}

const pad = (n: number, width = 2) => String(n).padStart(width, "0");

/** `2026-10-02 14:05:00` in the local time zone, with milliseconds only when there are some. */
export function localDateText(date: Date): string {
  const ms = date.getMilliseconds();
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}${ms ? `.${pad(ms, 3)}` : ""}`;
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${time}`;
}

/** `+07:00`: the local offset at `date`. */
function localOffset(date: Date): string {
  const minutes = -date.getTimezoneOffset();
  const sign = minutes < 0 ? "-" : "+";
  const abs = Math.abs(minutes);
  return `${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
}

/** The canonical UUID text of 16 bytes given as base64; null when they are not 16 bytes. */
export function uuidFromBase64(base64: string): string | null {
  const bytes = bytesOf(base64);
  if (!bytes || bytes.length !== 16) return null;
  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function bytesOf(base64: string): Uint8Array | null {
  try {
    const raw = atob(base64);
    return Uint8Array.from(raw, (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

/** How a value that is not nested reads: its text, a class suffix (`md-<kind>`), the type, and what hovering tells. */
export interface Readable {
  text: string;
  kind: ValueKind;
  type: BsonType;
  title: string | null;
}

/** Characters of a long base64 payload shown before an ellipsis. */
const BINARY_PREVIEW = 24;

export function readable(value: Json): Readable {
  const type = bsonType(value);
  const make = (text: string, kind: ValueKind, title: string | null = null): Readable => ({ text, kind, type, title });
  switch (type) {
    case "null":
      return make("null", "null");
    case "string":
      return make(JSON.stringify(value), "string");
    case "bool":
      return make(String(value), "boolean");
    case "array":
    case "object":
      return make(JSON.stringify(value), type);
    case "number": {
      if (typeof value === "number") return make(String(value), "number");
      const inner = record(value);
      const text = String(inner.$numberInt ?? inner.$numberDouble);
      return make(text, "number", "$numberInt" in inner ? "Int32" : "Double");
    }
    case "long":
      return make(String(record(value).$numberLong), "number", "Int64 (Long)");
    case "decimal":
      return make(String(record(value).$numberDecimal), "number", "Decimal128");
    case "objectId": {
      const hex = String(record(value).$oid);
      const time = objectIdTime(hex);
      const title = time ? `ObjectId, created ${localDateText(time)} (${localOffset(time)}) · ${time.toISOString()} UTC` : "ObjectId";
      return make(`ObjectId("${hex}")`, "objectId", title);
    }
    case "date": {
      const inner = record(value).$date;
      const date = dateOf(inner);
      if (!date) return make(`Date(${JSON.stringify(inner)})`, "date", "Date outside the range JavaScript can show");
      return make(localDateText(date), "date", `${date.toISOString()} UTC · shown in local time (${localOffset(date)})`);
    }
    case "uuid": {
      const inner = record(value);
      const text = typeof inner.$uuid === "string" ? inner.$uuid : uuidFromBase64(String(record(inner.$binary).base64 ?? ""));
      const subType = typeof inner.$uuid === "string" ? 4 : parseInt(String(record(inner.$binary).subType), 16);
      if (text) return make(`UUID("${text}")`, "binary", `Binary subtype ${subType} (UUID${subType === 3 ? ", legacy" : ""})`);
      return binaryReadable(record(inner.$binary), type);
    }
    case "binary":
      return binaryReadable(record(record(value).$binary), type);
    case "regex": {
      const regex = record(record(value).$regularExpression);
      return make(`/${String(regex.pattern ?? "")}/${String(regex.options ?? "")}`, "regex", "Regular expression");
    }
    case "timestamp": {
      const ts = record(record(value).$timestamp);
      const t = Number(ts.t);
      const at = Number.isFinite(t) ? new Date(t * 1000) : null;
      const title = at ? `Timestamp: ${localDateText(at)} (${localOffset(at)}), increment ${String(ts.i)}` : "Timestamp";
      return make(`Timestamp(${String(ts.t)}, ${String(ts.i)})`, "special", title);
    }
    case "minKey":
      return make("MinKey", "special", "MinKey (sorts before every value)");
    case "maxKey":
      return make("MaxKey", "special", "MaxKey (sorts after every value)");
    default:
      return make(JSON.stringify(value), "special");
  }
}

function binaryReadable(binary: Doc, type: BsonType): Readable {
  const base64 = String(binary.base64 ?? "");
  const subType = parseInt(String(binary.subType ?? "0"), 16) || 0;
  const bytes = bytesOf(base64)?.length ?? Math.floor((base64.length * 3) / 4);
  const preview = base64.length > BINARY_PREVIEW ? `${base64.slice(0, BINARY_PREVIEW)}…` : base64;
  return { text: `Binary(${preview})`, kind: "binary", type, title: `Binary subtype ${subType}, ${bytes} ${bytes === 1 ? "byte" : "bytes"}` };
}

// --- mongosh syntax --------------------------------------------------------------------------------------------------

/** A string in single quotes, as mongosh prints it. */
function quote(text: string): string {
  const escaped = JSON.stringify(text).slice(1, -1).replace(/\\"/g, '"').replace(/'/g, "\\'");
  return `'${escaped}'`;
}

const IDENTIFIER = /^[A-Za-z_$][\w$]*$/;

function keyText(key: string): string {
  return IDENTIFIER.test(key) ? key : quote(key);
}

/**
 * A value as mongosh syntax: `ObjectId('…')`, `ISODate('…')`, `NumberLong('…')`, `NumberDecimal('…')`, `UUID('…')`,
 * `BinData(0, '…')`, `/re/i`, `Timestamp({ t: 1, i: 2 })`, documents with unquoted keys. `indent` > 0 spreads
 * documents and arrays over lines.
 */
export function shellText(value: Json, indent = 0, depth = 0): string {
  const type = bsonType(value);
  const inner = record(value);
  switch (type) {
    case "null":
      return "null";
    case "string":
      return quote(String(value));
    case "bool":
      return String(value);
    case "number":
      // NaN and the infinities come as `{ $numberDouble: "NaN" }`; mongosh spells them as globals.
      return typeof value === "number" ? String(value) : String(inner.$numberInt ?? inner.$numberDouble);
    case "long":
      return `NumberLong('${String(inner.$numberLong)}')`;
    case "decimal":
      return `NumberDecimal('${String(inner.$numberDecimal)}')`;
    case "objectId":
      return `ObjectId('${String(inner.$oid)}')`;
    case "date": {
      const date = dateOf(inner.$date);
      return date ? `ISODate('${date.toISOString()}')` : `new Date(${Number(record(inner.$date).$numberLong ?? inner.$date)})`;
    }
    case "uuid": {
      const text = typeof inner.$uuid === "string" ? inner.$uuid : uuidFromBase64(String(record(inner.$binary).base64 ?? ""));
      if (text && (typeof inner.$uuid === "string" || parseInt(String(record(inner.$binary).subType), 16) === 4)) return `UUID('${text}')`;
      return binaryShell(record(inner.$binary));
    }
    case "binary":
      return binaryShell(record(inner.$binary));
    case "regex": {
      const regex = record(inner.$regularExpression);
      return `/${String(regex.pattern ?? "").replace(/(^|[^\\])\//g, "$1\\/")}/${String(regex.options ?? "")}`;
    }
    case "timestamp": {
      const ts = record(inner.$timestamp);
      return `Timestamp({ t: ${String(ts.t)}, i: ${String(ts.i)} })`;
    }
    case "minKey":
      return "MinKey()";
    case "maxKey":
      return "MaxKey()";
    case "array":
    case "object": {
      const entries: [string | null, Json][] = Array.isArray(value)
        ? value.map((item) => [null, item])
        : Object.entries(value as Doc).map(([key, item]) => [key, item]);
      const [open, close] = Array.isArray(value) ? ["[", "]"] : ["{", "}"];
      if (entries.length === 0) return `${open}${close}`;
      const parts = entries.map(([key, item]) => `${key === null ? "" : `${keyText(key)}: `}${shellText(item, indent, depth + 1)}`);
      if (indent <= 0) return Array.isArray(value) ? `[${parts.join(", ")}]` : `{ ${parts.join(", ")} }`;
      const pad = " ".repeat(indent * (depth + 1));
      return `${open}\n${parts.map((part) => `${pad}${part}`).join(",\n")}\n${" ".repeat(indent * depth)}${close}`;
    }
    default:
      return JSON.stringify(value);
  }
}

function binaryShell(binary: Doc): string {
  const subType = parseInt(String(binary.subType ?? "0"), 16) || 0;
  return `BinData(${subType}, '${String(binary.base64 ?? "")}')`;
}
