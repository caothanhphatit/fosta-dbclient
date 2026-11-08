/**
 * The Elasticsearch / OpenSearch console text (Kibana Dev Tools format): request blocks (`METHOD /path` and a JSON or
 * NDJSON body), the block under the cursor for "Run current statement", a body formatter, and completion of methods,
 * endpoints, index names, Query DSL keys and the request's index fields. Pure functions; `language.ts` wires them into
 * the editor.
 */
import type { TextRange } from "../../core/language";

export const METHODS = ["GET", "POST", "PUT", "DELETE", "HEAD"] as const;

const REQUEST_LINE = /^\s*(GET|POST|PUT|DELETE|HEAD|PATCH)(\s+|$)/i;

export interface Option {
  label: string;
  /** "keyword" (methods), "function" (endpoints), "variable" (indices, fields), "property" (body keys). */
  type: string;
  detail?: string;
  /** Text to insert when it differs from the label. */
  apply?: string;
}

export interface CompletionAnswer {
  from: number;
  to: number;
  options: Option[];
}

/** What the completion needs from the connection. */
export interface ConsoleNames {
  /** Index, alias and data stream names. */
  indices: string[];
  /** Field names of an index; null while they load. */
  fields(index: string): string[] | null;
}

export interface Block {
  /** Offset of the request line's first character. */
  from: number;
  to: number;
  /** Offset where the body starts (the line after the request line). */
  bodyFrom: number;
  method: string;
  path: string;
}

/** The request blocks of the text, in order; a block ends before the next request line (trailing blank lines cut). */
export function blocks(text: string): Block[] {
  const out: Block[] = [];
  let offset = 0;
  const lines = text.split("\n");
  let current: Block | null = null;
  let lastContent = 0;
  for (const line of lines) {
    const start = offset;
    offset += line.length + 1;
    const match = REQUEST_LINE.exec(line);
    const trimmed = line.trim();
    if (match && !trimmed.startsWith("{")) {
      if (current) {
        current.to = lastContent;
        out.push(current);
      }
      const lead = line.length - line.trimStart().length;
      const rest = trimmed.slice(match[1].length).trim();
      current = { from: start + lead, to: start + line.length, bodyFrom: Math.min(offset, text.length), method: match[1].toUpperCase(), path: rest };
      lastContent = start + line.trimEnd().length;
      continue;
    }
    if (trimmed !== "" && !trimmed.startsWith("#") && !trimmed.startsWith("//")) lastContent = start + line.trimEnd().length;
  }
  if (current) {
    current.to = Math.max(lastContent, current.from);
    out.push(current);
  }
  return out;
}

/** The request block around `offset` (between blocks: the one before, else the first). */
export function consoleStatementAt(text: string, offset: number): TextRange {
  const list = blocks(text);
  if (list.length === 0) return { from: offset, to: offset };
  let found = list[0];
  for (const block of list) {
    if (block.from <= offset) found = block;
  }
  return { from: found.from, to: found.to };
}

/** Pretty-prints each block's JSON body (two-space indent; NDJSON bodies keep one compact value per line). Bodies
 * that do not parse, or use Kibana's triple quotes, are left as written. */
export function formatConsole(text: string): string {
  const list = blocks(text);
  if (list.length === 0) return text;
  let out = text.slice(0, list[0].from);
  list.forEach((block, index) => {
    const end = index + 1 < list.length ? list[index + 1].from : text.length;
    const lineEnd = text.indexOf("\n", block.from);
    const requestEnd = lineEnd < 0 || lineEnd > end ? end : lineEnd;
    out += text.slice(block.from, requestEnd).trimEnd();
    const body = block.to > requestEnd ? text.slice(requestEnd + 1, block.to) : "";
    if (body.trim()) out += "\n" + formatBody(body.trim());
    // What follows the block (blank lines, comments) stays as written.
    out += text.slice(Math.max(block.to, requestEnd), end);
  });
  return out;
}

function formatBody(body: string): string {
  if (body.includes('"""') || /^\s*(#|\/\/)/m.test(body)) return body;
  const values = parseValues(body);
  if (!values) return body;
  if (values.length === 1) return JSON.stringify(values[0], null, 2);
  return values.map((value) => JSON.stringify(value)).join("\n");
}

/** Splits NDJSON or one JSON value; null when the text is not JSON. */
function parseValues(body: string): unknown[] | null {
  try {
    return [JSON.parse(body)];
  } catch {
    // NDJSON: one value per non-empty line.
  }
  try {
    return body
      .split("\n")
      .filter((line) => line.trim() !== "")
      .map((line) => JSON.parse(line) as unknown);
  } catch {
    return null;
  }
}

// --- Vocabulary -------------------------------------------------------------------------------------------------------

const ROOT_ENDPOINTS: [string, string][] = [
  ["_search", "search every index"],
  ["_cat/indices?v", "indices as a table"],
  ["_cat/health?v", "cluster health as a table"],
  ["_cat/nodes?v", "nodes as a table"],
  ["_cat/shards?v", "shards as a table"],
  ["_cat/aliases?v", "aliases as a table"],
  ["_cluster/health", "cluster health"],
  ["_cluster/settings", "cluster settings"],
  ["_cluster/stats", "cluster statistics"],
  ["_nodes/stats", "node statistics"],
  ["_tasks", "running tasks"],
  ["_bulk", "bulk index, update and delete"],
  ["_mget", "get several documents"],
  ["_msearch", "several searches"],
  ["_aliases", "change aliases"],
  ["_alias", "list aliases"],
  ["_mapping", "every mapping"],
  ["_data_stream", "data streams"],
  ["_index_template", "index templates"],
  ["_component_template", "component templates"],
  ["_ingest/pipeline", "ingest pipelines"],
  ["_analyze", "analyze text"],
  ["_reindex", "copy documents between indices"],
  ["_sql", "SQL (Elasticsearch)"],
  ["_query", "ES|QL (Elasticsearch 8.11+)"],
  ["_plugins/_sql", "SQL (OpenSearch)"],
  ["_ilm/policy", "ILM policies (Elasticsearch)"],
  ["_plugins/_ism/policies", "ISM policies (OpenSearch)"],
  ["_snapshot", "snapshot repositories"],
  ["_security/_authenticate", "who am I"],
];

const INDEX_ENDPOINTS: [string, string][] = [
  ["_search", "search"],
  ["_count", "count documents"],
  ["_mapping", "the mapping"],
  ["_settings", "the settings"],
  ["_stats", "statistics"],
  ["_doc/", "a document by id"],
  ["_create/", "create a document with an id"],
  ["_update/", "update a document"],
  ["_bulk", "bulk operations"],
  ["_delete_by_query", "delete matching documents"],
  ["_update_by_query", "update matching documents"],
  ["_alias", "aliases of the index"],
  ["_refresh", "make writes searchable"],
  ["_forcemerge", "merge segments"],
  ["_pit?keep_alive=1m", "open a point in time"],
  ["_validate/query?explain", "validate a query"],
  ["_field_caps?fields=*", "field capabilities"],
  ["_open", "open the index"],
  ["_close", "close the index"],
  ["_rollover", "roll over (alias or data stream)"],
];

const TOP_KEYS = [
  "query",
  "size",
  "from",
  "sort",
  "_source",
  "fields",
  "aggs",
  "track_total_hits",
  "highlight",
  "search_after",
  "pit",
  "collapse",
  "post_filter",
  "timeout",
  "explain",
  "profile",
  "knn",
  "seq_no_primary_term",
];

const QUERY_TYPES = [
  "match_all",
  "match",
  "match_phrase",
  "match_phrase_prefix",
  "multi_match",
  "term",
  "terms",
  "range",
  "exists",
  "prefix",
  "wildcard",
  "regexp",
  "fuzzy",
  "ids",
  "bool",
  "nested",
  "query_string",
  "simple_query_string",
  "constant_score",
  "dis_max",
  "geo_distance",
  "has_child",
  "has_parent",
  "knn",
];

const BOOL_KEYS = ["must", "filter", "should", "must_not", "minimum_should_match", "boost"];
const RANGE_KEYS = ["gte", "gt", "lte", "lt", "format", "time_zone"];
const MATCH_KEYS = ["query", "operator", "fuzziness", "analyzer", "minimum_should_match"];
const AGG_TYPES = [
  "terms",
  "date_histogram",
  "histogram",
  "range",
  "filter",
  "filters",
  "avg",
  "sum",
  "min",
  "max",
  "cardinality",
  "value_count",
  "stats",
  "extended_stats",
  "percentiles",
  "top_hits",
  "composite",
  "nested",
];
const QUERY_CONTAINERS = new Set(["query", "filter", "must", "should", "must_not", "post_filter", "positive", "negative"]);
const FIELD_QUERIES = new Set(["match", "match_phrase", "match_phrase_prefix", "term", "terms", "range", "prefix", "wildcard", "regexp", "fuzzy"]);

// --- Body context -----------------------------------------------------------------------------------------------------

interface BodyContext {
  /** Keys of the enclosing objects, outermost first ("[]" marks an array level). */
  path: string[];
  /** The cursor is where an object key goes. */
  expectKey: boolean;
  /** Start of the word being typed (after an opening quote when there is one). */
  wordFrom: number;
  quoted: boolean;
}

/** Walks the JSON body up to `pos` and says what is being typed there. */
export function bodyContext(text: string, from: number, pos: number): BodyContext {
  /** `name`: the key the container is the value of ("" for the root, "[]" inside an array). */
  const frames: { array: boolean; expectKey: boolean; key: string | null; name: string }[] = [];
  const path = () => frames.map((frame) => frame.name);
  const top = () => frames[frames.length - 1];
  let i = from;
  let lastString: string | null = null;
  while (i < pos) {
    const c = text[i];
    if (c === '"') {
      let j = i + 1;
      let value = "";
      while (j < pos && text[j] !== '"') {
        if (text[j] === "\\") j++;
        value += text[j] ?? "";
        j++;
      }
      if (j >= pos) {
        // Inside an open string: the word starts after the quote.
        const frame = top();
        return { path: path(), expectKey: Boolean(frame && !frame.array && frame.expectKey), wordFrom: i + 1, quoted: true };
      }
      lastString = value;
      i = j + 1;
      continue;
    }
    const frame = top();
    if (c === "{" || c === "[") {
      const name = !frame ? "" : frame.array ? "[]" : (frame.key ?? "");
      frames.push({ array: c === "[", expectKey: c === "{", key: null, name });
    } else if (c === "}" || c === "]") {
      frames.pop();
    } else if (c === ":" && frame && !frame.array) {
      frame.key = lastString;
      frame.expectKey = false;
    } else if (c === "," && frame && !frame.array) {
      frame.expectKey = true;
    }
    i++;
  }
  const frame = top();
  let wordFrom = pos;
  while (wordFrom > from && /[\w.@-]/.test(text[wordFrom - 1])) wordFrom--;
  return { path: path(), expectKey: Boolean(frame && !frame.array && frame.expectKey), wordFrom, quoted: false };
}

/** Keys for an object whose enclosing keys are `path` (outermost first; "" is the body root). */
export function keysFor(path: string[], fields: string[]): { keys: string[]; type: string; detail: string } {
  const named = path.filter((key) => key !== "[]");
  const last = named[named.length - 1] ?? "";
  const parent = named[named.length - 2] ?? "";
  if (path.length <= 1) return { keys: TOP_KEYS, type: "property", detail: "search" };
  if (QUERY_CONTAINERS.has(last) && parent !== "range") return { keys: QUERY_TYPES, type: "property", detail: "query" };
  if (last === "bool") return { keys: BOOL_KEYS, type: "property", detail: "bool" };
  if (FIELD_QUERIES.has(last)) return { keys: fields, type: "variable", detail: "field" };
  if (parent === "range") return { keys: RANGE_KEYS, type: "property", detail: "range" };
  if (parent === "match" || parent === "match_phrase") return { keys: MATCH_KEYS, type: "property", detail: "match" };
  if (last === "sort" || last === "highlight") return { keys: fields, type: "variable", detail: "field" };
  if (parent === "aggs" || parent === "aggregations") return { keys: AGG_TYPES, type: "property", detail: "aggregation" };
  return { keys: [], type: "property", detail: "" };
}

/** The index a request targets: the first path segment that is not an API (`_…`). */
export function indexOfPath(path: string): string | null {
  const first = path.replace(/^\//, "").split(/[/?]/)[0] ?? "";
  return first && !first.startsWith("_") ? decodeURIComponent(first) : null;
}

/** Completions at `pos`; null when there is nothing to offer. */
export function consoleCompletions(text: string, pos: number, names: ConsoleNames, explicit = false): CompletionAnswer | null {
  const lineStart = text.lastIndexOf("\n", pos - 1) + 1;
  const line = text.slice(lineStart, pos);
  // The request line: method first, then the path.
  const methodTyped = /^\s*([A-Za-z]*)$/.exec(line);
  const block = blocks(text)
    .filter((item) => item.from <= pos)
    .pop();
  const inBody = block !== undefined && pos >= block.bodyFrom && lineStart >= block.bodyFrom;
  if (methodTyped && !inBody && (explicit || methodTyped[1] !== "")) {
    const from = lineStart + line.length - methodTyped[1].length;
    return { from, to: pos, options: METHODS.map((method) => ({ label: method, type: "keyword", apply: `${method} ` })) };
  }
  const request = /^\s*(GET|POST|PUT|DELETE|HEAD|PATCH)\s+(\S*)$/i.exec(line);
  if (request) {
    const path = request[2];
    const pathFrom = pos - path.length;
    const slash = path.startsWith("/") ? 1 : 0;
    const segments = path.slice(slash).split("/");
    if (path.includes("?")) return null;
    if (segments.length <= 1) {
      const from = pathFrom + slash;
      const options: Option[] = [
        ...names.indices.map((name) => ({ label: name, type: "variable", detail: "index" })),
        ...ROOT_ENDPOINTS.map(([label, detail]) => ({ label, type: "function", detail })),
      ];
      return { from, to: pos, options };
    }
    if (segments.length === 2 && !segments[0].startsWith("_")) {
      const from = pos - segments[1].length;
      return { from, to: pos, options: INDEX_ENDPOINTS.map(([label, detail]) => ({ label, type: "function", detail })) };
    }
    return null;
  }
  if (!block || pos < block.bodyFrom) return null;
  const context = bodyContext(text, block.bodyFrom, pos);
  if (!context.expectKey) {
    // `"field": "…"` values (exists, aggregations) are field names.
    const before = text.slice(block.bodyFrom, context.wordFrom);
    if (context.quoted && /"field"\s*:\s*"?$/.test(before)) {
      const index = indexOfPath(block.path);
      const fields = index ? (names.fields(index) ?? []) : [];
      return fields.length ? { from: context.wordFrom, to: pos, options: fields.map((label) => ({ label, type: "variable", detail: "field" })) } : null;
    }
    return null;
  }
  const index = indexOfPath(block.path);
  const fields = index ? (names.fields(index) ?? []) : [];
  const { keys, type, detail } = keysFor(context.path, fields);
  if (keys.length === 0) return null;
  const options = keys.map((key) => ({ label: key, type, detail, apply: context.quoted ? key : `"${key}"` }));
  return { from: context.wordFrom, to: pos, options };
}
