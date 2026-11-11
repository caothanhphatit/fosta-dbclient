/**
 * The Validation section's model, without DOM: the `mongo.validation` / `mongo.setValidation` / `mongo.validationCheck`
 * types, the validator text (mongosh syntax) parsed for the backend, a `$jsonSchema` generated from a `mongo.schema`
 * result, the warnings shown before applying, the saved state (the draft, bounded) and the brief the assistant reads.
 */
import { parseMongoShell } from "../../lang/mongo/shell";
import { shellText } from "./bson";
import type { Doc, Json } from "./documents";
import type { SchemaPath, SchemaResult } from "./schemaModel";

export type ValidationLevel = "off" | "strict" | "moderate";
export type ValidationAction = "error" | "warn";

export const LEVELS: readonly { value: ValidationLevel; label: string; title: string }[] = [
  { value: "strict", label: "Strict", title: "Every insert and update is checked" },
  {
    value: "moderate",
    label: "Moderate",
    title: "Inserts and updates of documents that already pass are checked; documents that already fail can still be updated",
  },
  { value: "off", label: "Off", title: "Nothing is checked (the validator is kept)" },
];
export const ACTIONS: readonly { value: ValidationAction; label: string; title: string }[] = [
  { value: "error", label: "Error", title: "Writes that break the rules are refused" },
  { value: "warn", label: "Warn", title: "Writes that break the rules go through and are logged" },
];

export interface ValidationRules {
  /** Canonical Extended JSON; null when the collection has no validator. */
  validator: Doc | null;
  validationLevel: ValidationLevel;
  validationAction: ValidationAction;
  type: string | null;
  statement: string;
  elapsedMs: number;
}

export interface SetValidationArgs {
  database: string;
  collection: string;
  /** Extended JSON text; "{}" removes the rules. */
  validator?: string;
  validationLevel?: ValidationLevel;
  validationAction?: ValidationAction;
}

export interface SetValidationResult {
  statement: string;
  elapsedMs: number;
}

export interface ValidationCheckArgs {
  database: string;
  collection: string;
  validator: string;
  limit?: number;
  maxTimeMS?: number;
}

export interface ValidationCheckResult {
  failing: number;
  capped: boolean;
  total: number | null;
  samples: Doc[];
  statement: string;
  elapsedMs: number;
  /** The dev mock's note when it could not evaluate part of the validator. */
  note?: string | null;
}

const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);

/** A `mongo.validation` result read defensively. */
export function readRules(value: unknown): ValidationRules | null {
  if (!isRecord(value)) return null;
  const level = LEVELS.some((item) => item.value === value.validationLevel) ? (value.validationLevel as ValidationLevel) : "strict";
  const action = ACTIONS.some((item) => item.value === value.validationAction) ? (value.validationAction as ValidationAction) : "error";
  return {
    validator: isRecord(value.validator) && Object.keys(value.validator).length > 0 ? value.validator : null,
    validationLevel: level,
    validationAction: action,
    type: typeof value.type === "string" ? value.type : null,
    statement: typeof value.statement === "string" ? value.statement : "",
    elapsedMs: Number(value.elapsedMs) || 0,
  };
}

/** The validator as editable mongosh text ("" for none). */
export function validatorText(validator: Doc | null): string {
  return validator ? schemaText(validator) : "";
}

const IDENTIFIER = /^[A-Za-z_$][\w$]*$/;
const SHORT_ARRAY = 72;

/**
 * mongosh text of a validator over lines, like `shellText(value, 2)`, but arrays of plain values (`required`, `enum`,
 * `bsonType` lists) stay on one line while short.
 */
export function schemaText(value: Json, depth = 0): string {
  const pad = (level: number) => "  ".repeat(level);
  if (Array.isArray(value)) {
    if (value.length === 0) return "[]";
    if (value.every((item) => item === null || typeof item !== "object")) {
      const inline = `[${value.map((item) => shellText(item)).join(", ")}]`;
      if (inline.length <= SHORT_ARRAY) return inline;
    }
    return `[\n${value.map((item) => `${pad(depth + 1)}${schemaText(item, depth + 1)}`).join(",\n")}\n${pad(depth)}]`;
  }
  if (typeof value === "object" && value !== null && Object.keys(value).length > 0) {
    const plain = shellText(value);
    // BSON wrappers (ObjectId, ISODate, NumberLong …) print as one call.
    if (!plain.startsWith("{")) return plain;
    const entries = Object.entries(value as Doc).map(
      ([key, item]) => `${pad(depth + 1)}${IDENTIFIER.test(key) ? key : shellText(key)}: ${schemaText(item, depth + 1)}`,
    );
    return `{\n${entries.join(",\n")}\n${pad(depth)}}`;
  }
  return shellText(value);
}

export type ParsedValidator = { ok: true; json: string; empty: boolean } | { ok: false; error: string; position: number };

/** The editor's text as Extended JSON for the backend; an empty text (or `{}`) is no validator. */
export function parseValidator(text: string): ParsedValidator {
  if (!text.trim()) return { ok: true, json: "{}", empty: true };
  const parsed = parseMongoShell(text, { expect: "document" });
  if (!parsed.ok) return parsed;
  const empty = Object.keys(JSON.parse(parsed.json) as Doc).length === 0;
  return { ok: true, json: parsed.json, empty };
}

/** Whether two validator texts mean the same rules (both parse to the same JSON, or are the same text). */
export function sameValidator(a: string, b: string): boolean {
  if (a.trim() === b.trim()) return true;
  const left = parseValidator(a);
  const right = parseValidator(b);
  return left.ok && right.ok && left.json === right.json;
}

// --- Generating a $jsonSchema ------------------------------------------------------------------------------------------

export interface GenerateOptions {
  /** A field is required when present in at least this share (0-100) of the documents holding its parent. */
  requiredPercent: number;
  /** String fields with at most this many distinct values (all of them seen) get an `enum`; 0: none. */
  enumMax: number;
}

export const DEFAULT_GENERATE: GenerateOptions = { requiredPercent: 100, enumMax: 0 };
/** The most values an enum can list: the schema analysis reports at most 10 top values per path. */
export const MAX_ENUM = 10;

const parentOf = (entry: SchemaPath) => (entry.depth > 0 && entry.path.length > entry.name.length ? entry.path.slice(0, -(entry.name.length + 1)) : null);

function typeNames(types: readonly { type: string }[]): string[] {
  return [...new Set(types.map((type) => type.type))];
}

const bsonType = (names: readonly string[]) => (names.length === 1 ? names[0] : [...names]);

/** The complete list of string values of a path when it is small enough for an enum; null otherwise. */
function enumOf(entry: SchemaPath, max: number, withNull: boolean): Json[] | null {
  if (max <= 0 || entry.distinct === null || entry.distinct === 0 || entry.distinct > Math.min(max, MAX_ENUM)) return null;
  const values = entry.topValues ?? [];
  if (values.length !== entry.distinct || values.some((item) => typeof item.value !== "string" || item.value.endsWith("…"))) return null;
  const list: Json[] = values.map((item) => item.value).sort((a, b) => String(a).localeCompare(String(b)));
  if (withNull) list.push(null);
  return list;
}

/**
 * A `{ $jsonSchema }` validator built from a schema analysis: per path its `bsonType` (an array of types for mixed
 * fields, null included when seen), `properties` and `required` for sub-documents (also for documents in arrays, as
 * `items`), `items` with the element types for arrays, and optionally an `enum` for low-cardinality string fields.
 */
export function generateSchema(result: SchemaResult, options: GenerateOptions = DEFAULT_GENERATE): Doc {
  const children = new Map<string | null, SchemaPath[]>();
  const known = new Set(result.paths.map((entry) => entry.path));
  for (const entry of result.paths) {
    let parent = parentOf(entry);
    if (parent !== null && !known.has(parent)) parent = null;
    if (!children.has(parent)) children.set(parent, []);
    children.get(parent)?.push(entry);
  }
  const threshold = Math.max(0, Math.min(100, options.requiredPercent));
  const objectOf = (path: string | null, holders: number): Doc => {
    const node: Doc = { bsonType: "object" };
    const kids = children.get(path) ?? [];
    const required = kids.filter((kid) => holders > 0 && (kid.count / holders) * 100 >= threshold - 1e-9).map((kid) => kid.name);
    if (required.length > 0) node.required = required;
    if (kids.length > 0) node.properties = Object.fromEntries(kids.map((kid) => [kid.name, fieldOf(kid)]));
    return node;
  };
  const fieldOf = (entry: SchemaPath): Doc => {
    const names = typeNames(entry.types);
    const node: Doc = names.length > 0 ? { bsonType: bsonType(names) } : {};
    const hasKids = (children.get(entry.path) ?? []).length > 0;
    if (names.includes("object") && hasKids && !names.includes("array")) {
      const inner = objectOf(entry.path, entry.count);
      delete inner.bsonType;
      Object.assign(node, inner);
    }
    if (names.includes("array") && entry.array) {
      const elements = typeNames(entry.array.elementTypes);
      if (elements.length > 0) {
        let items: Doc = { bsonType: bsonType(elements) };
        if (elements.length === 1 && elements[0] === "object" && hasKids) items = objectOf(entry.path, entry.count);
        else if (elements.every((name) => name === "string") && names.length === 1) {
          const values = enumOf(entry, options.enumMax, false);
          if (values) items.enum = values;
        }
        node.items = items;
      }
    }
    if (names.length > 0 && names.every((name) => name === "string" || name === "null") && names.includes("string")) {
      const values = enumOf(entry, options.enumMax, names.includes("null"));
      if (values) node.enum = values;
    }
    return node;
  };
  return { $jsonSchema: objectOf(null, result.sample.count) };
}

// --- Applying ------------------------------------------------------------------------------------------------------------

export interface Draft {
  validator: string;
  level: ValidationLevel;
  action: ValidationAction;
}

/** What changes between the applied rules and the draft. */
export function draftChanges(applied: Draft, draft: Draft): { validator: boolean; level: boolean; action: boolean } {
  return { validator: !sameValidator(applied.validator, draft.validator), level: applied.level !== draft.level, action: applied.action !== draft.action };
}

export function isDirty(applied: Draft, draft: Draft): boolean {
  const changes = draftChanges(applied, draft);
  return changes.validator || changes.level || changes.action;
}

/** The `mongo.setValidation` arguments of a draft: only what changed. */
export function setArgs(
  target: { database: string; collection: string },
  applied: Draft,
  draft: Draft,
): SetValidationArgs | { error: string; position: number } {
  const changes = draftChanges(applied, draft);
  const args: SetValidationArgs = { ...target };
  if (changes.validator) {
    const parsed = parseValidator(draft.validator);
    if (!parsed.ok) return { error: parsed.error, position: parsed.position };
    args.validator = parsed.json;
  }
  if (changes.level) args.validationLevel = draft.level;
  if (changes.action) args.validationAction = draft.action;
  return args;
}

/** The warnings shown before applying: failing documents under strict rules, an emptied validator, validation off. */
export function applyWarnings(draft: Draft, failing: { count: number; capped: boolean } | null): string[] {
  const warnings: string[] = [];
  const parsed = parseValidator(draft.validator);
  const empty = parsed.ok && parsed.empty;
  if (empty) warnings.push("The validator is empty: applying removes the collection's validation rules.");
  if (draft.level === "off" && !empty) warnings.push("Level Off keeps the validator but checks nothing.");
  if (failing && failing.count > 0 && !empty && draft.level !== "off") {
    const count = `${failing.count.toLocaleString()}${failing.capped ? "+" : ""} existing ${failing.count === 1 && !failing.capped ? "document fails" : "documents fail"} these rules`;
    if (draft.level === "strict" && draft.action === "error")
      warnings.push(`${count}. With Strict and Error, any update to them is refused until they are fixed; consider Moderate or Warn first.`);
    else if (draft.action === "error") warnings.push(`${count}. With Moderate they can still be updated; new documents must pass.`);
    else warnings.push(`${count}. With Warn, writes that break the rules are logged, not refused.`);
  }
  return warnings;
}

/** The mongosh command applying a draft (shown in the review). */
export function applyShell(collection: string, args: SetValidationArgs): string {
  const command: Doc = { collMod: collection };
  if (args.validator !== undefined) command.validator = JSON.parse(args.validator) as Json;
  if (args.validationLevel) command.validationLevel = args.validationLevel;
  if (args.validationAction) command.validationAction = args.validationAction;
  return `db.runCommand(${schemaText(command)})`;
}

/** The Documents filter of the documents a validator refuses. */
export function failingFilter(validatorJson: string): string {
  return `{ $nor: [ ${shellText(JSON.parse(validatorJson) as Json)} ] }`;
}

/** The Documents filter of one document by `_id`. */
export function idFilter(id: Json): string {
  return `{ _id: ${shellText(id)} }`;
}

// --- Saved state and the assistant -------------------------------------------------------------------------------------

export interface ValidationSavedState {
  /** The draft when it differs from the applied rules; null otherwise. */
  draft: Draft | null;
  generate: GenerateOptions;
}

/** Characters of a saved draft (the tab's saved view is about 32 KB, shared with other sections). */
export const MAX_SAVED_DRAFT_CHARS = 8000;

export const DEFAULT_VALIDATION_STATE: ValidationSavedState = { draft: null, generate: { ...DEFAULT_GENERATE } };

const clampInt = (value: unknown, min: number, max: number, fallback: number) => {
  const number = Number(value);
  return Number.isFinite(number) ? Math.max(min, Math.min(max, Math.round(number))) : fallback;
};

export function readValidationState(value: unknown): ValidationSavedState {
  if (!isRecord(value)) return structuredClone(DEFAULT_VALIDATION_STATE);
  const draft = isRecord(value.draft) ? value.draft : null;
  const generate = isRecord(value.generate) ? value.generate : {};
  return {
    draft:
      draft && typeof draft.validator === "string"
        ? {
            validator: draft.validator.slice(0, MAX_SAVED_DRAFT_CHARS),
            level: LEVELS.some((item) => item.value === draft.level) ? (draft.level as ValidationLevel) : "strict",
            action: ACTIONS.some((item) => item.value === draft.action) ? (draft.action as ValidationAction) : "error",
          }
        : null,
    generate: {
      requiredPercent: clampInt(generate.requiredPercent, 0, 100, DEFAULT_GENERATE.requiredPercent),
      enumMax: clampInt(generate.enumMax, 0, MAX_ENUM, DEFAULT_GENERATE.enumMax),
    },
  };
}

/** The state to save: the draft only while it differs from the applied rules and fits. */
export function savedValidationState(draft: Draft | null, generate: GenerateOptions): ValidationSavedState {
  return { draft: draft && draft.validator.length <= MAX_SAVED_DRAFT_CHARS ? { ...draft } : null, generate: { ...generate } };
}

/** What the assistant reads: the applied rules (validator cut), whether a draft differs, the last check. */
export function rulesBrief(
  target: { database: string; collection: string },
  rules: ValidationRules | null,
  draft: Draft | null,
  check: ValidationCheckResult | null,
  maxChars = 1500,
): string {
  if (!rules) return `Validation of ${target.database}.${target.collection}: not read yet.`;
  const cut = (text: string) => (text.length > maxChars ? `${text.slice(0, maxChars - 1)}…` : text);
  const lines = [
    `Validation of ${target.database}.${target.collection}: level ${rules.validationLevel}, action ${rules.validationAction}${rules.validator ? "" : ", no validator"}.`,
  ];
  if (rules.validator) lines.push(`Validator:\n${cut(schemaText(rules.validator))}`);
  if (draft) {
    const applied = { validator: validatorText(rules.validator), level: rules.validationLevel, action: rules.validationAction };
    if (isDirty(applied, draft))
      lines.push(`Unapplied draft (level ${draft.level}, action ${draft.action}):\n${cut(draft.validator.trim() || "(empty: removes the rules)")}`);
  }
  if (check) lines.push(`Last check: ${check.failing}${check.capped ? "+" : ""} of ${check.total ?? "?"} documents fail the draft.`);
  return lines.join("\n");
}
