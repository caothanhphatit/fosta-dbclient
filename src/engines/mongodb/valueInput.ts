/**
 * The value editor's types, without DOM: which type a value is edited as, the text it starts with, and the Extended
 * JSON a typed text becomes. "Number" keeps the stored numeric type (an int stays an int, a long a long); Double, Int32,
 * Int64 and Decimal128 set the type on purpose. Object / array (and any other BSON type) are edited as mongosh syntax,
 * parsed by the query bar's parser (`src/lang/mongo/shell.ts`), so ObjectId('…'), ISODate('…') and NumberLong('…')
 * keep their types.
 */
import { parseMongoShell } from "../../lang/mongo/shell";
import { bsonType, dateOf, shellText } from "./bson";
import type { Doc, Json } from "./documents";

export type EditType = "string" | "number" | "double" | "int" | "long" | "decimal" | "bool" | "date" | "objectId" | "null" | "json";

export const EDIT_TYPES: readonly { id: EditType; label: string }[] = [
  { id: "string", label: "String" },
  { id: "number", label: "Number (keeps the stored type)" },
  { id: "double", label: "Double" },
  { id: "int", label: "Int32" },
  { id: "long", label: "Int64 (Long)" },
  { id: "decimal", label: "Decimal128" },
  { id: "bool", label: "Boolean" },
  { id: "date", label: "Date" },
  { id: "objectId", label: "ObjectId" },
  { id: "null", label: "Null" },
  { id: "json", label: "Object / array / other (mongosh syntax)" },
];

export type ParsedInput = { ok: true; value: Json; keepType: boolean } | { ok: false; error: string };

const record = (value: Json): Doc => (typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Doc) : {});

/** The type a value is edited as by default. */
export function defaultType(value: Json): EditType {
  switch (bsonType(value)) {
    case "string":
      return "string";
    case "number":
      return "number";
    case "long":
      return "long";
    case "decimal":
      return "decimal";
    case "bool":
      return "bool";
    case "null":
      return "null";
    case "objectId":
      return "objectId";
    case "date":
      return "date";
    default:
      return "json";
  }
}

/** The text a value starts with in the editor for `type` (empty when it has no such form). */
export function initialText(value: Json, type: EditType): string {
  const inner = record(value);
  const current = bsonType(value);
  switch (type) {
    case "string":
      return typeof value === "string" ? value : value === null || value === undefined ? "" : shellText(value);
    case "number":
    case "double":
    case "int":
    case "long":
    case "decimal": {
      if (typeof value === "number") return String(value);
      const text = inner.$numberLong ?? inner.$numberDecimal ?? inner.$numberInt ?? inner.$numberDouble;
      return typeof text === "string" ? text : typeof value === "string" && value.trim() !== "" && Number.isFinite(Number(value)) ? value : "";
    }
    case "bool":
      return value === false ? "false" : "true";
    case "date": {
      const date = current === "date" ? dateOf(inner.$date) : null;
      return date ? date.toISOString() : "";
    }
    case "objectId":
      return current === "objectId" ? String(inner.$oid) : typeof value === "string" && /^[0-9a-f]{24}$/i.test(value) ? value : "";
    case "null":
      return "";
    case "json":
      return value === undefined ? "" : shellText(value, 2);
  }
}

const INT32 = 2 ** 31;
const INT64 = 2n ** 63n;

/** The Extended JSON of `text` typed as `type`, or why it does not fit. */
export function parseInput(type: EditType, text: string): ParsedInput {
  const trimmed = text.trim();
  const fail = (error: string): ParsedInput => ({ ok: false, error });
  const ok = (value: Json, keepType = false): ParsedInput => ({ ok: true, value, keepType });
  switch (type) {
    case "string":
      return ok(text);
    case "number":
    case "double": {
      const n = Number(trimmed);
      if (trimmed === "" || !Number.isFinite(n)) return fail("Enter a number, e.g. 42 or 4.2.");
      return type === "number" ? ok(n, true) : ok({ $numberDouble: String(n) });
    }
    case "int": {
      const n = Number(trimmed);
      if (!/^-?\d+$/.test(trimmed) || n < -INT32 || n >= INT32) return fail("Enter a whole number between -2147483648 and 2147483647.");
      return ok({ $numberInt: String(n) });
    }
    case "long": {
      if (!/^-?\d+$/.test(trimmed)) return fail("Enter a whole number, e.g. 9007199254740993.");
      const n = BigInt(trimmed);
      if (n < -INT64 || n >= INT64) return fail("The number is out of the Int64 range.");
      return ok({ $numberLong: n.toString() });
    }
    case "decimal":
      if (!/^[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/.test(trimmed)) return fail("Enter a decimal number, e.g. 19.99.");
      return ok({ $numberDecimal: trimmed.replace(/^\+/, "") });
    case "bool":
      if (!/^(true|false)$/i.test(trimmed)) return fail("Enter true or false.");
      return ok(trimmed.toLowerCase() === "true");
    case "date": {
      if (!trimmed) return fail("Enter a date, e.g. 2026-01-02T08:30:00Z.");
      // A date alone is midnight UTC; a time without a zone is read as UTC as well (said under the field).
      const withZone = /^\d{4}-\d{2}-\d{2}$/.test(trimmed)
        ? `${trimmed}T00:00:00Z`
        : /[zZ]|[+-]\d{2}:?\d{2}$/.test(trimmed)
          ? trimmed
          : `${trimmed.replace(" ", "T")}Z`;
      const ms = Date.parse(withZone);
      if (Number.isNaN(ms)) return fail("Not a date; write it as 2026-01-02 or 2026-01-02T08:30:00Z.");
      return ok({ $date: new Date(ms).toISOString() });
    }
    case "objectId":
      if (!/^[0-9a-f]{24}$/i.test(trimmed)) return fail("An ObjectId is 24 hex digits.");
      return ok({ $oid: trimmed.toLowerCase() });
    case "null":
      return ok(null);
    case "json": {
      if (!trimmed) return fail("Enter a value in mongosh syntax, e.g. { city: 'Oslo' } or [1, 2].");
      const parsed = parseMongoShell(trimmed);
      if (!parsed.ok) return fail(parsed.error);
      return ok(JSON.parse(parsed.json) as Json);
    }
  }
}

/** A new document typed in mongosh syntax (insert, duplicate). */
export function parseDocumentInput(text: string): { ok: true; document: Doc } | { ok: false; error: string; position: number } {
  const parsed = parseMongoShell(text.trim() || "{}", { expect: "document" });
  if (!parsed.ok) return { ok: false, error: parsed.error, position: parsed.position };
  return { ok: true, document: JSON.parse(parsed.json) as Doc };
}

/** The text a duplicate starts with: the document without its `_id` (the server gives the copy a new one). */
export function duplicateText(document: Doc): string {
  const copy: Doc = { ...document };
  delete copy._id;
  return shellText(copy, 2);
}
