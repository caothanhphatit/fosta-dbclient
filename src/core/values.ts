/**
 * Typed cells (`Value`, see `contract.ts`) in the UI: the text a grid shows (`displayValue`, the same rules as Rust
 * `Value::display()`) and parsing what the user typed back to a value of a column's kind (`parseInput`, mirroring Rust
 * `contract::value::parse_input`; the server validates again).
 */
import type { Value, ValueKind } from "./contract";

const SAFE_INT = 2 ** 53;

/** Rust's `f64` text: shortest round-trip digits, never an exponent, NaN / Infinity spelled out. */
export function formatFloat(value: number | string): string {
  if (typeof value === "string") return value;
  if (Number.isNaN(value)) return "NaN";
  if (!Number.isFinite(value)) return value > 0 ? "Infinity" : "-Infinity";
  if (Object.is(value, -0)) return "-0";
  return expandExponent(String(value));
}

/** "1e+21" → "1000000000000000000000", "1.5e-7" → "0.00000015". */
function expandExponent(text: string): string {
  const match = /^(-?)(\d+)(?:\.(\d+))?e([+-]\d+)$/.exec(text);
  if (!match) return text;
  const [, sign, int, frac = "", exp] = match;
  const digits = int + frac;
  const point = int.length + Number(exp);
  if (point <= 0) return `${sign}0.${"0".repeat(-point)}${digits}`;
  if (point >= digits.length) return `${sign}${digits}${"0".repeat(point - digits.length)}`;
  return `${sign}${digits.slice(0, point)}.${digits.slice(point)}`;
}

/** Relaxed JSON like Rust `Value::to_json`: ObjectId as `{"$oid"}`, JSON text parsed, temporals and decimals as text. */
export function valueToJson(value: Value): unknown {
  switch (value.t) {
    case "null":
      return null;
    case "bool":
      return value.v;
    case "int":
      return typeof value.v === "number" ? value.v : Number(value.v);
    case "float":
      return typeof value.v === "number" && Number.isFinite(value.v) ? value.v : formatFloat(value.v);
    case "json":
      try {
        return JSON.parse(value.v) as unknown;
      } catch {
        return value.v;
      }
    case "array":
      return value.v.map(valueToJson);
    case "objectId":
      return { $oid: value.v };
    case "doc":
      return Object.fromEntries(value.v.map(([key, item]) => [key, valueToJson(item)]));
    default:
      return displayValue(value);
  }
}

/** The text of a grid cell; null for NULL. */
export function displayValue(value: Value): string | null {
  switch (value.t) {
    case "null":
      return null;
    case "bool":
      return String(value.v);
    case "int":
      return String(value.v);
    case "float":
      return formatFloat(value.v);
    case "doc":
      return JSON.stringify(valueToJson(value));
    case "array":
      return `[${value.v.map(arrayItem).join(", ")}]`;
    default:
      return value.v;
  }
}

function arrayItem(item: Value): string {
  switch (item.t) {
    case "null":
      return "null";
    case "text":
    case "uuid":
    case "interval":
    case "date":
    case "time":
    case "timestamp":
    case "timestamptz":
    case "objectId":
    case "other":
      return JSON.stringify(displayValue(item) ?? "");
    default:
      return displayValue(item) ?? "";
  }
}

/** Numbers are right-aligned in grids. */
export function isNumericKind(kind: ValueKind): boolean {
  return kind.kind === "int" || kind.kind === "float" || kind.kind === "decimal";
}

/** Whether two values are the same cell (same tag and payload). */
export function valuesEqual(a: Value, b: Value): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

// --- Parsing user input -------------------------------------------------------------------------------------------------

function invalid(text: string, what: string): Error {
  return new Error(`'${text}' is not a valid ${what}.`);
}

function isIntegerText(t: string): boolean {
  return /^[+-]?\d+$/.test(t);
}

/** Rust `is_decimal_text`: digits with an optional fraction and exponent. */
export function isDecimalText(t: string): boolean {
  return /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(t);
}

const pad = (n: number, width = 2) => String(n).padStart(width, "0");

/** ".250" / ".000001" / "" like chrono's `%.f`: 0, 3, 6 or 9 digits. */
function fraction(digits: string | undefined): string {
  if (!digits) return "";
  const nanos = Number(digits.slice(0, 9).padEnd(9, "0"));
  if (nanos === 0) return "";
  const text = String(nanos).padStart(9, "0");
  if (nanos % 1_000_000 === 0) return `.${text.slice(0, 3)}`;
  if (nanos % 1_000 === 0) return `.${text.slice(0, 6)}`;
  return `.${text}`;
}

function validDate(year: number, month: number, day: number): boolean {
  if (month < 1 || month > 12 || day < 1) return false;
  return day <= new Date(Date.UTC(year, month, 0)).getUTCDate();
}

function parseDate(t: string): string | null {
  const match = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(t);
  if (!match) return null;
  const [year, month, day] = [Number(match[1]), Number(match[2]), Number(match[3])];
  return validDate(year, month, day) ? `${match[1]}-${pad(month)}-${pad(day)}` : null;
}

function parseTime(t: string): string | null {
  const match = /^(\d{1,2}):(\d{1,2})(?::(\d{1,2})(?:\.(\d+))?)?$/.exec(t);
  if (!match) return null;
  const [hour, minute, second] = [Number(match[1]), Number(match[2]), Number(match[3] ?? 0)];
  if (hour > 23 || minute > 59 || second > 60) return null;
  return `${pad(hour)}:${pad(minute)}:${pad(second)}${fraction(match[4])}`;
}

function parseTimestamp(t: string): string | null {
  const match = /^(\S+?)(?:[ T](.+))?$/.exec(t);
  if (!match) return null;
  const date = parseDate(match[1]);
  if (!date) return null;
  if (match[2] === undefined) return `${date} 00:00:00`;
  const time = parseTime(match[2]);
  return time ? `${date} ${time}` : null;
}

function parseTimestampTz(t: string): string | null {
  const match = /^(.+?)\s?(Z|z|[+-]\d{2}(?::?\d{2})?)$/.exec(t);
  if (!match) {
    const bare = parseTimestamp(t);
    return bare ? `${bare}+00:00` : null;
  }
  const local = parseTimestamp(match[1]);
  if (!local) return null;
  const zone = match[2];
  if (zone === "Z" || zone === "z") return `${local}+00:00`;
  const digits = zone.slice(1).replace(":", "");
  const hours = Number(digits.slice(0, 2));
  const minutes = Number(digits.slice(2) || "0");
  if (hours > 23 || minutes > 59) return null;
  return `${local}${zone[0]}${pad(hours)}:${pad(minutes)}`;
}

function normalizeUuid(t: string): string | null {
  const hex = t.replace(/^\{|\}$/g, "").replace(/-/g, "");
  if (!/^[0-9a-fA-F]{32}$/.test(hex)) return null;
  const h = hex.toLowerCase();
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

function unhex(t: string): string | null {
  const digits = /^(?:0x|0X|\\x)([0-9a-fA-F]*)$/.exec(t)?.[1];
  if (digits === undefined || digits.length % 2 !== 0) return null;
  return `0x${digits.toLowerCase()}`;
}

function utf8Hex(text: string): string {
  return `0x${[...new TextEncoder().encode(text)].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

/**
 * Parses what the user typed into a cell or filter as a value of `kind`. Empty text is an empty string for text columns
 * and an error for the others (NULL is chosen explicitly, never by an empty box). Returns an `Error` instead of throwing.
 */
export function parseInput(kind: ValueKind, text: string): Value | Error {
  const t = text.trim();
  switch (kind.kind) {
    case "text":
      return { t: "text", v: text };
    case "other":
      return { t: "other", type: "", v: text };
    case "bool": {
      const lower = t.toLowerCase();
      if (["true", "t", "1", "yes", "y", "on"].includes(lower)) return { t: "bool", v: true };
      if (["false", "f", "0", "no", "n", "off"].includes(lower)) return { t: "bool", v: false };
      return invalid(text, "boolean (true or false)");
    }
    case "int": {
      if (!isIntegerText(t)) return invalid(text, "whole number");
      const digits = t.replace(/^\+/, "");
      const big = BigInt(digits);
      if (big < -(2n ** 63n) || big > 2n ** 63n - 1n) return { t: "decimal", v: digits };
      return { t: "int", v: big >= -BigInt(SAFE_INT) && big <= BigInt(SAFE_INT) ? Number(big) : big.toString() };
    }
    case "float": {
      const lower = t.toLowerCase();
      if (lower === "nan") return { t: "float", v: "NaN" };
      if (["inf", "infinity", "+infinity", "+inf"].includes(lower)) return { t: "float", v: "Infinity" };
      if (["-inf", "-infinity"].includes(lower)) return { t: "float", v: "-Infinity" };
      if (!isDecimalText(t)) return invalid(text, "number");
      const number = Number(t);
      return Number.isFinite(number) ? { t: "float", v: number } : { t: "float", v: number > 0 ? "Infinity" : "-Infinity" };
    }
    case "decimal":
      return isDecimalText(t) ? { t: "decimal", v: t.replace(/^\+/, "") } : invalid(text, "number");
    case "bytes":
      return { t: "bytes", v: unhex(t) ?? utf8Hex(text) };
    case "date": {
      const date = parseDate(t);
      return date ? { t: "date", v: date } : invalid(text, "date (YYYY-MM-DD)");
    }
    case "time": {
      const time = parseTime(t);
      return time ? { t: "time", v: time } : invalid(text, "time (HH:MM:SS)");
    }
    case "timestamp": {
      const timestamp = parseTimestamp(t);
      return timestamp ? { t: "timestamp", v: timestamp } : invalid(text, "timestamp (YYYY-MM-DD HH:MM:SS)");
    }
    case "timestampTz": {
      const timestamp = parseTimestampTz(t);
      return timestamp ? { t: "timestamptz", v: timestamp } : invalid(text, "timestamp with time zone");
    }
    case "interval":
      return t ? { t: "interval", v: t } : invalid(text, "interval");
    case "uuid": {
      const uuid = normalizeUuid(t);
      return uuid ? { t: "uuid", v: uuid } : invalid(text, "UUID");
    }
    case "objectId": {
      const hex = t
        .replace(/^ObjectId\(/, "")
        .replace(/\)$/, "")
        .replace(/^["']+|["']+$/g, "");
      return /^[0-9a-fA-F]{24}$/.test(hex) ? { t: "objectId", v: hex.toLowerCase() } : invalid(text, "ObjectId (24 hex characters)");
    }
    case "json":
    case "document": {
      let parsed: unknown;
      try {
        parsed = JSON.parse(t);
      } catch (error) {
        return new Error(`The JSON is not valid: ${error instanceof Error ? error.message : String(error)}.`);
      }
      if (kind.kind === "document" && (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))) {
        return invalid(text, "document (a JSON object)");
      }
      return { t: "json", v: t };
    }
    case "array": {
      let parsed: unknown;
      try {
        parsed = JSON.parse(t);
      } catch {
        return invalid(text, "list (a JSON array such as [1, 2])");
      }
      if (!Array.isArray(parsed)) return invalid(text, "list (a JSON array such as [1, 2])");
      const items: Value[] = [];
      for (const item of parsed) {
        const value: Value | Error = item === null ? { t: "null", v: null } : parseInput(kind.of, typeof item === "string" ? item : JSON.stringify(item));
        if (value instanceof Error) return value;
        items.push(value);
      }
      return { t: "array", v: items };
    }
  }
}
