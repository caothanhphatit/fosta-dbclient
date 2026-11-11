/**
 * The query bar's model, without DOM: the fields as typed (mongosh syntax) to `mongo.find` / `mongo.count` arguments
 * (Extended JSON from `parseMongoShell`), the canonical command text kept in the query history and read back from it,
 * the saved section state, and the page cursors (`_id` keyset or skip).
 */
import { parseMongoShell } from "../../lang/mongo/shell";

/** The document fields of the query bar. */
export type QueryField = "filter" | "project" | "sort" | "collation";

export const QUERY_FIELDS: readonly QueryField[] = ["filter", "project", "sort", "collation"];

export const FIELD_LABELS: Record<QueryField, string> = { filter: "Filter", project: "Project", sort: "Sort", collation: "Collation" };

export interface QueryState {
  filter: string;
  project: string;
  sort: string;
  collation: string;
  limit: number;
  skip: number;
}

export const DEFAULT_LIMIT = 20;
/** The most documents one page asks for (the backend caps `mongo.find` at 1000). */
export const MAX_LIMIT = 1000;

export const EMPTY_QUERY: QueryState = { filter: "", project: "", sort: "", collation: "", limit: DEFAULT_LIMIT, skip: 0 };

export interface Target {
  database: string;
  collection: string;
}

/** A field that does not parse; `position` is an offset into the field's own text. */
export interface FieldError {
  field: QueryField;
  message: string;
  position: number;
}

export interface FindArgs extends Target {
  filter?: string;
  projection?: string;
  sort?: string;
  collation?: string;
  limit: number;
  skip?: number;
  after?: unknown;
}

export interface CountArgs extends Target {
  filter?: string;
  collation?: string;
  /** Without a filter the count is read from the collection's metadata. */
  estimated: boolean;
}

export interface BuiltQuery {
  find: FindArgs;
  count: CountArgs;
  /** Whether a filter narrows the documents (the count is then exact). */
  filtered: boolean;
}

/**
 * The text a field parses: a field typed without its braces (`status: "paid"`) gets them, and `shift` is how far the
 * parsed text is from the typed one (for error positions and completion).
 */
export function braced(text: string): { text: string; shift: number } {
  const trimmed = text.trim();
  if (trimmed === "" || trimmed.startsWith("{")) return { text, shift: 0 };
  return { text: `{${text}}`, shift: 1 };
}

/** Parses one field to Extended JSON; null for an empty field or `{}`. */
export function parseField(field: QueryField, text: string): { ok: true; json: string | null } | { ok: false; error: FieldError } {
  if (text.trim() === "") return { ok: true, json: null };
  const wrapped = braced(text);
  const result = parseMongoShell(wrapped.text, { expect: "document" });
  if (!result.ok) {
    const position = Math.max(0, Math.min(text.length, result.position - wrapped.shift));
    return { ok: false, error: { field, message: result.error, position } };
  }
  return { ok: true, json: result.json === "{}" ? null : result.json };
}

/**
 * The `mongo.find` and `mongo.count` arguments of the bar's state, or the first field that does not parse. `exactCount`:
 * the count is never read from metadata (a view has none, so its estimate would fail).
 */
export function buildQuery(
  state: QueryState,
  target: Target,
  options: { exactCount?: boolean } = {},
): { ok: true; query: BuiltQuery } | { ok: false; error: FieldError } {
  const parsed: Partial<Record<QueryField, string | null>> = {};
  for (const field of QUERY_FIELDS) {
    const result = parseField(field, state[field]);
    if (!result.ok) return result;
    parsed[field] = result.json;
  }
  const limit = clampLimit(state.limit);
  const skip = Math.max(0, Math.floor(Number(state.skip) || 0));
  const find: FindArgs = { ...target, limit };
  if (parsed.filter) find.filter = parsed.filter;
  if (parsed.project) find.projection = parsed.project;
  if (parsed.sort) find.sort = parsed.sort;
  if (parsed.collation) find.collation = parsed.collation;
  if (skip > 0) find.skip = skip;
  const count: CountArgs = { ...target, estimated: !parsed.filter && !options.exactCount };
  if (parsed.filter) count.filter = parsed.filter;
  if (parsed.collation) count.collation = parsed.collation;
  return { ok: true, query: { find, count, filtered: !!parsed.filter } };
}

export function clampLimit(value: unknown): number {
  const n = Math.floor(Number(value));
  return Number.isFinite(n) && n >= 1 ? Math.min(n, MAX_LIMIT) : DEFAULT_LIMIT;
}

/** The token range to underline at `position`: the word or punctuation there, or an empty range at the end. */
export function errorRange(text: string, position: number): { from: number; to: number } {
  const at = Math.max(0, Math.min(position, text.length));
  if (at >= text.length) return { from: text.length, to: text.length };
  let to = at + 1;
  if (/[\w$]/.test(text[at])) while (to < text.length && /[\w$]/.test(text[to])) to++;
  return { from: at, to };
}

// --- History: one canonical command text per run ---------------------------------------------------------------------

/** Line breaks and the indentation after them, outside strings, become one space (history entries are one line). */
export function oneLine(text: string): string {
  let out = "";
  let quote: string | null = null;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quote) {
      out += c;
      if (c === "\\" && i + 1 < text.length) out += text[++i];
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'") quote = c;
    if (c === "\n" || c === "\r") {
      out = out.replace(/[ \t]+$/, "");
      while (i + 1 < text.length && /\s/.test(text[i + 1])) i++;
      out += " ";
      continue;
    }
    out += c;
  }
  return out.trim();
}

/** The prefix every command of a collection starts with (also what the history is searched for). */
export function commandPrefix(collection: string): string {
  return `db.getCollection(${JSON.stringify(collection)}).find(`;
}

/** `db.getCollection("orders").find({ status: "paid" }, { name: 1 }).sort({ total: -1 }).skip(5).limit(20)`. */
export function commandText(state: QueryState, collection: string): string {
  const doc = (text: string) => {
    const line = oneLine(text);
    return line === "" ? "{}" : braced(line).text;
  };
  const project = state.project.trim() ? `, ${doc(state.project)}` : "";
  let text = `${commandPrefix(collection)}${doc(state.filter)}${project})`;
  if (state.sort.trim()) text += `.sort(${doc(state.sort)})`;
  if (state.collation.trim()) text += `.collation(${doc(state.collation)})`;
  const skip = Math.max(0, Math.floor(Number(state.skip) || 0));
  if (skip > 0) text += `.skip(${skip})`;
  text += `.limit(${clampLimit(state.limit)})`;
  return text;
}

/** The end of the call argument list that starts after `(` at `from` (strings and nesting respected); -1 when open. */
function closingParen(text: string, from: number): number {
  let depth = 0;
  let quote: string | null = null;
  for (let i = from; i < text.length; i++) {
    const c = text[i];
    if (quote) {
      if (c === "\\") i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'") quote = c;
    else if ("{[(".includes(c)) depth++;
    else if ("}])".includes(c)) {
      if (depth === 0) return c === ")" ? i : -1;
      depth--;
    }
  }
  return -1;
}

/** Splits call arguments at top-level commas. */
function splitArgs(text: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let quote: string | null = null;
  let start = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quote) {
      if (c === "\\") i++;
      else if (c === quote) quote = null;
      continue;
    }
    if (c === '"' || c === "'") quote = c;
    else if ("{[(".includes(c)) depth++;
    else if ("}])".includes(c)) depth--;
    else if (c === "," && depth === 0) {
      parts.push(text.slice(start, i).trim());
      start = i + 1;
    }
  }
  const last = text.slice(start).trim();
  if (last || parts.length > 0) parts.push(last);
  return parts;
}

/** Reads a command written by `commandText` back into a state; null for anything else (another collection, other text). */
export function parseCommandText(text: string, collection: string): QueryState | null {
  const prefix = commandPrefix(collection);
  if (!text.startsWith(prefix)) return null;
  let i = prefix.length;
  const end = closingParen(text, i);
  if (end < 0) return null;
  const args = splitArgs(text.slice(i, end));
  if (args.length > 2) return null;
  const state: QueryState = { ...EMPTY_QUERY, filter: args[0] === "{}" ? "" : (args[0] ?? ""), project: args[1] ?? "" };
  i = end + 1;
  while (i < text.length) {
    const call = /^\.(sort|collation|skip|limit)\(/.exec(text.slice(i));
    if (!call) return null;
    const open = i + call[0].length;
    const close = closingParen(text, open);
    if (close < 0) return null;
    const arg = text.slice(open, close).trim();
    if (call[1] === "sort") state.sort = arg;
    else if (call[1] === "collation") state.collation = arg;
    else if (call[1] === "skip") state.skip = Math.max(0, Number(arg) || 0);
    else state.limit = clampLimit(arg);
    i = close + 1;
  }
  return state;
}

// --- Saved section state ---------------------------------------------------------------------------------------------

export type DocumentsMode = "list" | "json" | "table";

export const DOCUMENT_MODES: readonly DocumentsMode[] = ["list", "json", "table"];

export interface DocumentsSavedState extends QueryState {
  mode: DocumentsMode;
  /** The Project / Sort / Skip / Limit row is shown. */
  options: boolean;
  /** Filters run lately on this collection, newest first. */
  recent: string[];
}

export const MAX_RECENT = 15;
/** Longer fields are not saved with the tabs (they stay in the query history). */
const MAX_SAVED_FIELD = 4000;

const savedText = (value: unknown): string => (typeof value === "string" && value.length <= MAX_SAVED_FIELD ? value : "");

/** The section's saved state read back; anything missing or malformed takes its default. */
export function readSavedState(value: unknown): DocumentsSavedState {
  const saved = typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
  const mode = DOCUMENT_MODES.includes(saved.mode as DocumentsMode) ? (saved.mode as DocumentsMode) : "list";
  const recent = Array.isArray(saved.recent)
    ? saved.recent.filter((item): item is string => typeof item === "string" && item.length <= MAX_SAVED_FIELD).slice(0, MAX_RECENT)
    : [];
  return {
    filter: savedText(saved.filter),
    project: savedText(saved.project),
    sort: savedText(saved.sort),
    collation: savedText(saved.collation),
    limit: clampLimit(saved.limit ?? DEFAULT_LIMIT),
    skip: Math.max(0, Math.floor(Number(saved.skip) || 0)),
    mode,
    options: saved.options === true,
    recent,
  };
}

/** `filter` moved to the front of `recent` (empty filters are not kept). */
export function rememberFilter(recent: string[], filter: string): string[] {
  const text = filter.trim();
  if (!text || text === "{}") return recent;
  return [text, ...recent.filter((item) => item !== text)].slice(0, MAX_RECENT);
}

/** `filters` without repeats, first one kept; `status: 1` and `{ status: 1 }` (the history's form) are the same. */
export function uniqueFilters(filters: readonly string[]): string[] {
  const seen = new Set<string>();
  return filters.filter((filter) => {
    const key = braced(oneLine(filter)).text.replace(/\s+/g, "");
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

// --- Paging ----------------------------------------------------------------------------------------------------------

/** Where a page starts: after an `_id` (keyset, when the sort is `_id` alone) or after skipping documents. */
export interface PageCursor {
  after: unknown;
  skip: number;
  /** The 0-based position of the page's first document among all matches. */
  offset: number;
}

export interface FindPage {
  documents: unknown[];
  hasMore: boolean;
  keyset: boolean;
  nextAfter: unknown;
}

export function firstCursor(state: Pick<QueryState, "skip">): PageCursor {
  const skip = Math.max(0, Math.floor(Number(state.skip) || 0));
  return { after: null, skip, offset: skip };
}

/** The cursor of the page after `page` (read at `cursor`), or null when it was the last. */
export function nextCursor(cursor: PageCursor, page: FindPage): PageCursor | null {
  if (!page.hasMore) return null;
  const offset = cursor.offset + page.documents.length;
  if (page.keyset && page.nextAfter !== null && page.nextAfter !== undefined) return { after: page.nextAfter, skip: 0, offset };
  return { after: null, skip: cursor.skip + page.documents.length, offset };
}

/** The `mongo.find` arguments of a page. */
export function pageArgs(find: FindArgs, cursor: PageCursor): FindArgs {
  const args: FindArgs = { ...find };
  delete args.skip;
  delete args.after;
  if (cursor.skip > 0) args.skip = cursor.skip;
  if (cursor.after !== null && cursor.after !== undefined) args.after = cursor.after;
  return args;
}

/** "1–20 of 1,234", "21–40 of ~5,000", "No documents". */
export function rangeText(offset: number, shown: number, total: number | null, estimated: boolean, format: (n: number) => string): string {
  const of = total === null ? "" : ` of ${estimated ? "~" : ""}${format(total)}`;
  if (shown === 0) return "No documents";
  return `${format(offset + 1)}–${format(offset + shown)}${of}`;
}
