/**
 * The Indexes section's model (pure): the `mongo.indexes` result read defensively, the keys and properties shown per
 * index, the "unused" hint, the list's sort and search, the mongosh `createIndex` text of an index or of the create
 * form, the form's `mongo.createIndex` arguments, a prefilled form from another section's request, and the summary the
 * assistant gets (names, keys and properties; never values of documents).
 */
import { shellText } from "./bson";
import type { Json } from "./documents";
import { parseField, type Target } from "./queryModel";

export type KeyKind = 1 | -1 | "text" | "2dsphere" | "2d" | "hashed";

export const KEY_KINDS: readonly { kind: KeyKind; label: string; title: string }[] = [
  { kind: 1, label: "1 ↑", title: "Ascending" },
  { kind: -1, label: "-1 ↓", title: "Descending" },
  { kind: "text", label: "text", title: "Text search" },
  { kind: "2dsphere", label: "2dsphere", title: "Geospatial on a sphere (GeoJSON)" },
  { kind: "hashed", label: "hashed", title: "Hashed (equality, shard keys)" },
];

export interface IndexKey {
  field: string;
  kind: KeyKind | string;
}

export interface IndexUsage {
  ops: number;
  since: string | null;
}

export interface IndexInfo {
  name: string;
  key: Record<string, Json>;
  keys: IndexKey[];
  options: Record<string, Json>;
  size: number | null;
  usage: IndexUsage | null;
}

export interface IndexesResult {
  indexes: IndexInfo[];
  totalSize: number | null;
  stats: { sizes: boolean; usage: boolean; note: string | null };
  elapsedMs: number;
}

export const ID_INDEX = "_id_";
/** Days without an operation after which an index is hinted as unused. */
export const UNUSED_DAYS = 7;

const isRecord = (value: unknown): value is Record<string, Json> => typeof value === "object" && value !== null && !Array.isArray(value);

function readKind(value: unknown): KeyKind | string {
  if (typeof value === "number") return value < 0 ? -1 : 1;
  if (typeof value === "string") return value;
  if (isRecord(value)) {
    const number = Number(value.$numberInt ?? value.$numberLong ?? value.$numberDouble);
    if (Number.isFinite(number)) return number < 0 ? -1 : 1;
  }
  return String(value);
}

function readIndex(value: unknown): IndexInfo | null {
  if (!isRecord(value) || typeof value.name !== "string") return null;
  const key = isRecord(value.key) ? value.key : {};
  const keys = Array.isArray(value.keys)
    ? value.keys.filter(isRecord).map((entry) => ({ field: String(entry.field), kind: readKind(entry.kind) }))
    : Object.entries(key).map(([field, kind]) => ({ field, kind: readKind(kind) }));
  const usage = isRecord(value.usage) ? { ops: Number(value.usage.ops) || 0, since: typeof value.usage.since === "string" ? value.usage.since : null } : null;
  return {
    name: value.name,
    key,
    keys,
    options: isRecord(value.options) ? value.options : {},
    size: typeof value.size === "number" ? value.size : null,
    usage,
  };
}

/** A `mongo.indexes` result read defensively; null when it is not one. */
export function readIndexesResult(value: unknown): IndexesResult | null {
  if (!isRecord(value) || !Array.isArray(value.indexes)) return null;
  const stats = isRecord(value.stats) ? value.stats : {};
  return {
    indexes: value.indexes.map(readIndex).filter((index): index is IndexInfo => index !== null),
    totalSize: typeof value.totalSize === "number" ? value.totalSize : null,
    stats: { sizes: stats.sizes === true, usage: stats.usage === true, note: typeof stats.note === "string" ? stats.note : null },
    elapsedMs: Number(value.elapsedMs) || 0,
  };
}

// --- Keys and properties ----------------------------------------------------------------------------------------------

/**
 * The keys as the user wrote them: a text index stores `_fts` / `_ftsx` and its fields in `weights`, shown here as
 * `field: text` (other keys of a compound text index kept in place).
 */
export function displayKeys(index: IndexInfo): IndexKey[] {
  if (!index.keys.some((key) => key.field === "_fts")) return index.keys;
  const weights = isRecord(index.options.weights) ? Object.keys(index.options.weights) : [];
  const shown: IndexKey[] = [];
  for (const key of index.keys) {
    if (key.field === "_fts") shown.push(...weights.map((field) => ({ field, kind: "text" as const })));
    else if (key.field !== "_ftsx") shown.push(key);
  }
  return shown;
}

export function kindLabel(kind: KeyKind | string): string {
  if (kind === 1) return "asc";
  if (kind === -1) return "desc";
  return String(kind);
}

/** The kind of index: single field, compound, text, geospatial, hashed or wildcard. */
export function indexType(index: IndexInfo): string {
  const keys = displayKeys(index);
  if (keys.some((key) => key.field === "$**" || key.field.endsWith(".$**"))) return "wildcard";
  for (const special of ["text", "2dsphere", "2d", "hashed"]) if (keys.some((key) => key.kind === special)) return special;
  return keys.length > 1 ? "compound" : "single";
}

export interface Badge {
  id: string;
  label: string;
  title: string;
}

/** A duration in seconds, in words (`30 d`, `2 h`, `90 s`). */
export function secondsText(seconds: number): string {
  if (seconds >= 86_400 && seconds % 86_400 === 0) return `${seconds / 86_400} d`;
  if (seconds >= 3600 && seconds % 3600 === 0) return `${seconds / 3600} h`;
  if (seconds >= 60 && seconds % 60 === 0) return `${seconds / 60} min`;
  return `${seconds} s`;
}

function numberOf(value: Json): number | null {
  if (typeof value === "number") return value;
  if (isRecord(value)) {
    const number = Number(value.$numberInt ?? value.$numberLong ?? value.$numberDouble);
    return Number.isFinite(number) ? number : null;
  }
  return null;
}

/** The properties of an index as badges: unique, sparse, partial, TTL, collation, hidden, wildcard projection, weights. */
export function indexBadges(index: IndexInfo): Badge[] {
  const options = index.options;
  const badges: Badge[] = [];
  if (index.name === ID_INDEX) badges.push({ id: "id", label: "_id", title: "The collection's _id index (unique, cannot be dropped)" });
  if (options.unique === true) badges.push({ id: "unique", label: "unique", title: "Refuses two documents with the same key" });
  if (options.sparse === true) badges.push({ id: "sparse", label: "sparse", title: "Leaves out documents without the indexed fields" });
  if (isRecord(options.partialFilterExpression))
    badges.push({ id: "partial", label: "partial", title: `Only documents matching ${shellText(options.partialFilterExpression)}` });
  const ttl = numberOf(options.expireAfterSeconds ?? null);
  if (ttl !== null) badges.push({ id: "ttl", label: `TTL ${secondsText(ttl)}`, title: `Documents expire ${ttl} seconds after the indexed date` });
  if (isRecord(options.collation) && typeof options.collation.locale === "string" && options.collation.locale !== "simple")
    badges.push({ id: "collation", label: `collation ${options.collation.locale}`, title: shellText(options.collation) });
  if (options.hidden === true) badges.push({ id: "hidden", label: "hidden", title: "Kept up to date but not used by the query planner" });
  if (isRecord(options.wildcardProjection))
    badges.push({ id: "projection", label: "projection", title: `Wildcard projection ${shellText(options.wildcardProjection)}` });
  if (isRecord(options.weights) && Object.values(options.weights).some((weight) => numberOf(weight) !== 1))
    badges.push({ id: "weights", label: "weights", title: `Text weights ${shellText(options.weights)}` });
  return badges;
}

/** Whether the index had no operation for at least `days` days (by `$indexStats`); never the `_id` index. */
export function isUnused(index: IndexInfo, now = Date.now(), days = UNUSED_DAYS): boolean {
  if (index.name === ID_INDEX || !index.usage || index.usage.ops > 0 || !index.usage.since) return false;
  const since = Date.parse(index.usage.since);
  return Number.isFinite(since) && now - since >= days * 86_400_000;
}

export function bytesText(bytes: number | null): string {
  if (bytes === null) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GiB`;
}

// --- The list -------------------------------------------------------------------------------------------------------------

export type IndexSort = "name" | "size" | "usage";
export const INDEX_SORTS: readonly IndexSort[] = ["name", "size", "usage"];

export interface IndexesSavedState {
  sort: IndexSort;
  descending: boolean;
  search: string;
}

export const DEFAULT_INDEXES_STATE: IndexesSavedState = { sort: "name", descending: false, search: "" };

export function readIndexesState(value: unknown): IndexesSavedState {
  if (!isRecord(value)) return { ...DEFAULT_INDEXES_STATE };
  return {
    sort: INDEX_SORTS.includes(value.sort as IndexSort) ? (value.sort as IndexSort) : "name",
    descending: value.descending === true,
    search: typeof value.search === "string" ? value.search.slice(0, 200) : "",
  };
}

/** The indexes matching `search` (name or a key field), sorted; `_id_` first when sorted by name. */
export function visibleIndexes(indexes: readonly IndexInfo[], state: IndexesSavedState): IndexInfo[] {
  const needle = state.search.trim().toLowerCase();
  const shown = needle
    ? indexes.filter((index) => index.name.toLowerCase().includes(needle) || displayKeys(index).some((key) => key.field.toLowerCase().includes(needle)))
    : [...indexes];
  const value = (index: IndexInfo): number | string =>
    state.sort === "size" ? (index.size ?? -1) : state.sort === "usage" ? (index.usage?.ops ?? -1) : index.name === ID_INDEX ? "" : index.name.toLowerCase();
  shown.sort((a, b) => {
    const [left, right] = [value(a), value(b)];
    const order = left < right ? -1 : left > right ? 1 : a.name.localeCompare(b.name);
    return state.descending ? -order : order;
  });
  return shown;
}

// --- mongosh --------------------------------------------------------------------------------------------------------------

const KEY_TEXT = /^[A-Za-z_$][\w$]*$/;

function keysShell(keys: readonly IndexKey[]): string {
  if (keys.length === 0) return "{}";
  const parts = keys.map(
    (key) =>
      `${KEY_TEXT.test(key.field) ? key.field : `'${key.field.replace(/\\/g, "\\\\").replace(/'/g, "\\'")}'`}: ${typeof key.kind === "number" ? key.kind : `'${key.kind}'`}`,
  );
  return `{ ${parts.join(", ")} }`;
}

function collectionShell(collection: string): string {
  return KEY_TEXT.test(collection) && !collection.startsWith("$") ? `db.${collection}` : `db.getCollection('${collection.replace(/'/g, "\\'")}')`;
}

/** Options of an existing index for `createIndex` (`name` first; internal fields such as `textIndexVersion` left out). */
const INTERNAL_OPTIONS = new Set(["v", "key", "ns", "textIndexVersion", "2dsphereIndexVersion", "language_override"]);

/** The mongosh line that creates `index` again (copy definition). */
export function indexShell(collection: string, index: IndexInfo): string {
  const options: Record<string, Json> = { name: index.name };
  for (const [key, value] of Object.entries(index.options)) if (!INTERNAL_OPTIONS.has(key)) options[key] = value;
  return `${collectionShell(collection)}.createIndex(${keysShell(displayKeys(index))}, ${shellText(options)})`;
}

// --- The create form ----------------------------------------------------------------------------------------------------

export interface FormKey {
  field: string;
  kind: KeyKind;
}

export interface IndexForm {
  keys: FormKey[];
  name: string;
  unique: boolean;
  sparse: boolean;
  hidden: boolean;
  /** Shell or Extended JSON text, braces optional. */
  partial: string;
  /** Seconds, as typed; empty for no TTL. */
  ttl: string;
  collation: string;
  wildcardProjection: string;
  defaultLanguage: string;
}

export const EMPTY_FORM: IndexForm = {
  keys: [{ field: "", kind: 1 }],
  name: "",
  unique: false,
  sparse: false,
  hidden: false,
  partial: "",
  ttl: "",
  collation: "",
  wildcardProjection: "",
  defaultLanguage: "",
};

export type FormField = "keys" | "name" | "partial" | "ttl" | "collation" | "wildcardProjection";

export interface CreateIndexArgs extends Target {
  /** The key document as JSON text (keeps the order of the keys). */
  keys: string;
  name?: string;
  unique?: boolean;
  sparse?: boolean;
  hidden?: boolean;
  partialFilterExpression?: string;
  expireAfterSeconds?: number;
  collation?: string;
  wildcardProjection?: string;
  defaultLanguage?: string;
}

export type FormResult = { ok: true; args: CreateIndexArgs } | { ok: false; field: FormField; error: string };

export const isWildcard = (field: string) => field === "$**" || field.endsWith(".$**");

/** The server's default name of the keys (`status_1_createdAt_-1`). */
export function defaultIndexName(keys: readonly FormKey[]): string {
  return keys
    .filter((key) => key.field.trim())
    .map((key) => `${key.field.trim()}_${key.kind}`)
    .join("_");
}

function checkPath(field: string): string | null {
  const path = isWildcard(field) ? field.slice(0, -3).replace(/\.$/, "") : field;
  if (!isWildcard(field) && !path) return "A key needs a field";
  if (path && path.split(".").some((part) => !part || part.startsWith("$"))) return `'${field}' is not a field path`;
  return null;
}

/** The form's `mongo.createIndex` arguments, or the first field that is not right. */
export function formArgs(form: IndexForm, target: Target): FormResult {
  const keys = form.keys.map((key) => ({ field: key.field.trim(), kind: key.kind })).filter((key) => key.field);
  if (keys.length === 0) return { ok: false, field: "keys", error: "Add at least one key field" };
  const seen = new Set<string>();
  for (const key of keys) {
    const problem = checkPath(key.field);
    if (problem) return { ok: false, field: "keys", error: problem };
    if (seen.has(key.field)) return { ok: false, field: "keys", error: `${key.field} is twice in the keys` };
    seen.add(key.field);
    if (isWildcard(key.field) && typeof key.kind !== "number") return { ok: false, field: "keys", error: "A wildcard key is 1 or -1" };
  }
  const keyText = `{${keys.map((key) => `${JSON.stringify(key.field)}:${JSON.stringify(key.kind)}`).join(",")}}`;
  const args: CreateIndexArgs = { ...target, keys: keyText };
  if (form.name.trim()) args.name = form.name.trim();
  if (form.unique) args.unique = true;
  if (form.sparse) args.sparse = true;
  if (form.hidden) args.hidden = true;
  const documents: [FormField, "filter" | "collation" | "project", "partialFilterExpression" | "collation" | "wildcardProjection"][] = [
    ["partial", "filter", "partialFilterExpression"],
    ["collation", "collation", "collation"],
    ["wildcardProjection", "project", "wildcardProjection"],
  ];
  for (const [field, kind, arg] of documents) {
    const parsed = parseField(kind, form[field] as string);
    if (!parsed.ok) return { ok: false, field, error: parsed.error.message };
    if (parsed.json) args[arg] = parsed.json;
  }
  if (form.ttl.trim()) {
    const seconds = Number(form.ttl.trim());
    if (!Number.isInteger(seconds) || seconds < 0 || seconds > 2_147_483_647) return { ok: false, field: "ttl", error: "TTL is a whole number of seconds" };
    if (keys.length !== 1) return { ok: false, field: "ttl", error: "A TTL index has a single date field" };
    args.expireAfterSeconds = seconds;
  }
  if (args.wildcardProjection && !keys.some((key) => key.field === "$**"))
    return { ok: false, field: "wildcardProjection", error: "A wildcard projection needs the key $**" };
  if (form.defaultLanguage.trim() && keys.some((key) => key.kind === "text")) args.defaultLanguage = form.defaultLanguage.trim();
  return { ok: true, args };
}

/** The mongosh line the form would run (the typed text where a document does not parse yet). */
export function formShell(form: IndexForm, collection: string): string {
  const keys = form.keys.filter((key) => key.field.trim()).map((key) => ({ field: key.field.trim(), kind: key.kind }));
  const options: string[] = [];
  const name = form.name.trim() || defaultIndexName(keys);
  if (name) options.push(`name: '${name.replace(/'/g, "\\'")}'`);
  if (form.unique) options.push("unique: true");
  if (form.sparse) options.push("sparse: true");
  if (form.hidden) options.push("hidden: true");
  const documentText = (kind: "filter" | "collation" | "project", text: string): string => {
    const parsed = parseField(kind, text);
    if (parsed.ok && parsed.json) return shellText(JSON.parse(parsed.json) as Json);
    return text.trim().startsWith("{") ? text.trim() : `{ ${text.trim()} }`;
  };
  if (form.partial.trim()) options.push(`partialFilterExpression: ${documentText("filter", form.partial)}`);
  if (form.ttl.trim()) options.push(`expireAfterSeconds: ${form.ttl.trim()}`);
  if (form.collation.trim()) options.push(`collation: ${documentText("collation", form.collation)}`);
  if (form.wildcardProjection.trim()) options.push(`wildcardProjection: ${documentText("project", form.wildcardProjection)}`);
  if (form.defaultLanguage.trim() && keys.some((key) => key.kind === "text")) options.push(`default_language: '${form.defaultLanguage.trim()}'`);
  return `${collectionShell(collection)}.createIndex(${keysShell(keys)}${options.length ? `, { ${options.join(", ")} }` : ""})`;
}

/** A request from another section (Explain): `{ createIndex: { keys, options } }`, keys as a document or `[{ field, kind }]`. */
export interface CreateIndexRequest {
  createIndex: { keys: Record<string, KeyKind> | FormKey[]; options?: Record<string, Json> };
}

const KINDS = new Set<unknown>([1, -1, "text", "2dsphere", "2d", "hashed"]);

function textOf(value: Json): string {
  return isRecord(value) ? shellText(value) : "";
}

/** The form a request opens (null when it is not a create-index request). */
export function formOfRequest(request: unknown): IndexForm | null {
  if (!isRecord(request) || !isRecord(request.createIndex)) return null;
  const { keys, options } = request.createIndex as { keys?: unknown; options?: unknown };
  const entries: [unknown, unknown][] = Array.isArray(keys)
    ? keys.filter(isRecord).map((key) => [key.field, key.kind])
    : isRecord(keys)
      ? Object.entries(keys)
      : [];
  const formKeys = entries
    .filter(([field]) => typeof field === "string" && field.trim())
    .map(([field, kind]) => ({ field: String(field), kind: (KINDS.has(readKind(kind)) ? readKind(kind) : 1) as KeyKind }));
  if (formKeys.length === 0) return null;
  const opts = isRecord(options) ? options : {};
  const ttl = numberOf(opts.expireAfterSeconds ?? null);
  return {
    ...EMPTY_FORM,
    keys: formKeys,
    name: typeof opts.name === "string" ? opts.name : "",
    unique: opts.unique === true,
    sparse: opts.sparse === true,
    hidden: opts.hidden === true,
    partial: textOf(opts.partialFilterExpression ?? null),
    ttl: ttl === null ? "" : String(ttl),
    collation: textOf(opts.collation ?? null),
    wildcardProjection: textOf(opts.wildcardProjection ?? null),
    defaultLanguage: typeof opts.defaultLanguage === "string" ? opts.defaultLanguage : typeof opts.default_language === "string" ? opts.default_language : "",
  };
}

// --- Assistant ------------------------------------------------------------------------------------------------------------

/** The indexes in a few lines for the assistant: name, keys, properties, size and usage. */
export function indexesSummary(result: IndexesResult, target: Target, maxIndexes = 40): string {
  const lines = [`Indexes of ${target.database}.${target.collection} (${result.indexes.length}):`];
  for (const index of result.indexes.slice(0, maxIndexes)) {
    const badges = indexBadges(index)
      .filter((badge) => badge.id !== "id")
      .map((badge) => (badge.id === "partial" ? `partial ${shellText(index.options.partialFilterExpression ?? null)}` : badge.label));
    const parts = [`- ${index.name}: ${keysShell(displayKeys(index))}`];
    if (badges.length) parts.push(badges.join(", "));
    if (index.size !== null) parts.push(bytesText(index.size));
    if (index.usage)
      parts.push(`${index.usage.ops} ops${index.usage.since ? ` since ${index.usage.since.slice(0, 10)}` : ""}${isUnused(index) ? " (unused)" : ""}`);
    lines.push(parts.join("; "));
  }
  if (result.indexes.length > maxIndexes) lines.push(`…and ${result.indexes.length - maxIndexes} more`);
  if (result.stats.note) lines.push(result.stats.note);
  return lines.join("\n");
}
