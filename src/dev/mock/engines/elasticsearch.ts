/**
 * Elasticsearch 8.19 mock: a `products` index (300 documents with text, keyword, scaled_float, date, object and nested
 * fields), its alias `products-current`, and the `logs-app` data stream (2,000 log events). The tree has the Indices,
 * Data streams and Aliases folders; the grid takes structured filters and simple Query DSL raw filters (term, terms,
 * range, exists, match, prefix, wildcard, bool; scripts are refused like the backend does). The console answers
 * `_search`, `_count`, `_mapping`, `_cat/indices`, `_cluster/health` and `/` with a hits table and the JSON response;
 * writes are acknowledged without changing the data. OpenSearch (`opensearch.ts`) reuses this module.
 */
import type {
  ApplyResult,
  BrowseRequest,
  ColumnInfo,
  ConnectInfo,
  EngineDescriptor,
  FilterCond,
  ObjectDescription,
  ObjectNode,
  ObjectRef,
  Page,
  RowChangeSet,
  RunRequest,
  StreamEvent,
  TreeRequest,
  Value,
} from "../../../core/contract";
import type { StatementInfo } from "../../../core/types";
import { valueToJson } from "../../../core/values";
import { builtin, column, MockCatalog, queryAction, v, viewAction, type CatalogStyle, type TableSpec } from "../catalog";
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mockError } from "../errors";
import { pause } from "../sqlRun";
import type { EngineMock, RunContext } from "../types";

/** The flavor a mock instance stands for. */
export interface Flavor {
  id: "elasticsearch" | "opensearch";
  label: string;
  version: string;
}

const ELASTIC: Flavor = { id: "elasticsearch", label: "Elasticsearch", version: "8.19.23" };

export const order = ORDER.indexOf("elasticsearch");

const pad = (n: number, width = 2) => String(n).padStart(width, "0");
const BRANDS = ["acme", "globex", "initech", "umbrella", "hooli"];
const COLORS = ["red", "green", "blue", "black", "white"];
const LEVELS = ["INFO", "INFO", "INFO", "WARN", "ERROR", "DEBUG"];
const SERVICES = ["api", "worker", "scheduler", "auth"];

function tables(withVector: boolean): TableSpec[] {
  return [
    {
      database: "",
      name: "products",
      kind: "collection",
      key: ["_id"],
      versioned: true,
      columns: [
        column("_id", "_id", "text", { primary: true, nullable: false }),
        column("brand", "keyword", "text"),
        column("created", "date", "timestampTz"),
        column("description", "text", "text"),
        column("in_stock", "boolean", "bool"),
        column("location", "geo_point", "json"),
        column("name", "text", "text"),
        column("price", "scaled_float", "decimal"),
        column("rating", "half_float", "float"),
        column("supplier", "object", "document"),
        column("tags", "keyword", { kind: "array", of: { kind: "text" } }),
        column("variants", "nested", { kind: "array", of: { kind: "document" } }),
        ...(withVector ? [column("embedding", "dense_vector", { kind: "array", of: { kind: "float" } })] : []),
      ],
      rows: () =>
        Array.from({ length: 300 }, (_, index) => {
          const n = index + 1;
          return [
            v.text(String(n)),
            v.text(BRANDS[n % BRANDS.length]),
            v.tstz(`2026-${pad((n % 12) + 1)}-${pad((n % 28) + 1)} 09:30:00+00:00`),
            v.text(`A sturdy ${COLORS[n % COLORS.length]} product for everyday use (${n}).`),
            v.bool(n % 4 !== 0),
            v.json({ lat: 10 + (n % 50), lon: 100 + (n % 60) }),
            v.text(`Product ${n}`),
            v.dec(((n % 90) * 1.5 + 4.99).toFixed(2)),
            v.float(Math.round((1 + (n % 40) / 10) * 10) / 10),
            v.doc([
              ["name", v.text(`${BRANDS[(n + 1) % BRANDS.length]} supply`)],
              ["country", v.text(["VN", "DE", "PE", "JP", "US"][n % 5])],
            ]),
            v.arr([v.text(COLORS[n % COLORS.length]), v.text(n % 3 ? "sale" : "new")]),
            v.arr([
              v.doc([
                ["sku", v.text(`P${n}-A`)],
                ["color", v.text(COLORS[n % COLORS.length])],
                ["stock", v.int((n * 7) % 40)],
              ]),
            ]),
            ...(withVector ? [v.arr([0.1, 0.2, 0.3, 0.4].map((x) => v.float(Math.round(((x + n / 300) % 1) * 1000) / 1000)))] : []),
          ];
        }),
      extra: [
        ["Shards", "1"],
        ["Replicas", "0"],
        ["Key", "_id"],
      ],
    },
    {
      database: "",
      name: "logs-app",
      kind: "collection",
      key: ["_index", "_id"],
      versioned: true,
      columns: [
        column("_id", "_id", "text", { primary: true, nullable: false }),
        column("_index", "_index", "text", { primary: true, nullable: false }),
        column("@timestamp", "date", "timestampTz"),
        column("duration_ms", "long", "int"),
        column("host", "object", "document"),
        column("level", "keyword", "text"),
        column("message", "text", "text"),
        column("service", "keyword", "text"),
        column("status", "short", "int"),
      ],
      rows: () =>
        Array.from({ length: 2000 }, (_, i) => {
          const at = new Date(Date.UTC(2026, 9, 26) + i * 30_000).toISOString().replace("T", " ").replace(".000Z", "+00:00");
          return [
            v.text(`log-${pad(i, 5)}`),
            v.text(`.ds-logs-app-2026.10.26-00000${i < 1000 ? 1 : 2}`),
            v.tstz(at),
            v.int((i * 7919) % 2000),
            v.doc([["name", v.text(`node-${i % 3}`)]]),
            v.text(LEVELS[i % 6].toLowerCase()),
            v.text(`request ${i} handled`),
            v.text(SERVICES[i % 4]),
            v.int(i % 17 === 0 ? 500 : 200),
          ];
        }),
      extra: [
        ["Kind", "dataStream"],
        ["Backing indices", "2"],
      ],
    },
  ];
}

function catalogFor(flavor: Flavor): MockCatalog {
  const style: CatalogStyle = {
    engine: flavor.id,
    quote: (name) => name,
    label: (table) => table.name,
    rawFilter: "esQueryDsl",
    countUnit: "docs",
    databaseActions: [],
    tableActions: (table) => [
      viewAction("design", "Mapping", "design"),
      queryAction("search", "Search in console", `GET /${table.name}/_search\n{\n  "query": { "match_all": {} },\n  "size": 20\n}`),
      builtin("copyName", "Copy name", "copyName"),
    ],
    errors: {
      noTable: (name: string) => esError(404, "index_not_found_exception", `no such index [${name}]`),
      noColumn: (name: string) => esError(400, "illegal_argument_exception", `No mapping found for [${name}] in order to sort on`),
      syntax: (near: string) => esError(400, "parsing_exception", `unknown query [${near}]`),
    },
  };
  return new MockCatalog(style, [{ name: "", tables: tables(flavor.id === "elasticsearch") }]);
}

function esError(status: number, type: string, reason: string, engine = "elasticsearch") {
  return mockError("ENGINE_ERROR", reason, { engine, vendorCode: type, status, kind: "server", retryable: false, cause: [`${type}: ${reason}`] });
}

// --- Raw Query DSL filters --------------------------------------------------------------------------------------------

const DENIED = ["script", "script_score", "script_fields", "scripted_metric", "_script", "runtime_mappings", "wrapper"];

function deniedKey(value: unknown): string | null {
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = deniedKey(item);
      if (found) return found;
    }
    return null;
  }
  if (value && typeof value === "object") {
    for (const [key, inner] of Object.entries(value)) {
      const lower = key.toLowerCase();
      if (DENIED.includes(lower) || lower.endsWith("_script")) return key;
      const found = deniedKey(inner);
      if (found) return found;
    }
  }
  return null;
}

/** A simple Query DSL query as structured conditions (what the mock catalog filters by). */
function dslToConditions(query: Record<string, unknown>, out: FilterCond[]): void {
  const [type, body] = Object.entries(query)[0] ?? [];
  const entry = (value: unknown): [string, unknown] => Object.entries((value ?? {}) as Record<string, unknown>)[0] ?? ["", null];
  const text = (value: unknown) =>
    typeof value === "object" && value !== null && "value" in value ? String((value as { value: unknown }).value) : String(value);
  switch (type) {
    case "match_all":
      return;
    case "term":
    case "match":
    case "match_phrase": {
      const [field, value] = entry(body);
      const operand = typeof value === "object" && value !== null && "query" in value ? (value as { query: unknown }).query : value;
      out.push({ column: field, op: type === "term" ? "eq" : "contains", values: [text(operand)] });
      return;
    }
    case "terms": {
      const [field, values] = entry(body);
      out.push({ column: field, op: "in", values: (values as unknown[]).map(String) });
      return;
    }
    case "prefix": {
      const [field, value] = entry(body);
      out.push({ column: field, op: "startsWith", values: [text(value)] });
      return;
    }
    case "wildcard": {
      const [field, value] = entry(body);
      out.push({ column: field, op: "like", values: [text(value).replace(/\*/g, "%").replace(/\?/g, "_")] });
      return;
    }
    case "exists":
      out.push({ column: String((body as { field?: string }).field ?? ""), op: "isNotNull", values: [] });
      return;
    case "range": {
      const [field, bounds] = entry(body);
      const ops: Record<string, FilterCond["op"]> = { gt: "gt", gte: "ge", lt: "lt", lte: "le" };
      for (const [bound, value] of Object.entries((bounds ?? {}) as Record<string, unknown>)) {
        if (ops[bound]) out.push({ column: field, op: ops[bound], values: [String(value)] });
      }
      return;
    }
    case "bool": {
      const clauses = body as Record<string, unknown>;
      for (const key of ["must", "filter"]) {
        const items = clauses[key];
        for (const item of Array.isArray(items) ? items : items ? [items] : []) dslToConditions(item as Record<string, unknown>, out);
      }
      const negated = clauses.must_not;
      for (const item of Array.isArray(negated) ? negated : negated ? [negated] : []) {
        const inner: FilterCond[] = [];
        dslToConditions(item as Record<string, unknown>, inner);
        for (const condition of inner) {
          const opposite: Partial<Record<FilterCond["op"], FilterCond["op"]>> = {
            eq: "ne",
            isNotNull: "isNull",
            in: "notIn",
            contains: "notContains",
            like: "notLike",
          };
          const op = opposite[condition.op];
          if (!op) throw mockError("INVALID_FILTER", "The dev mock negates only term, terms, exists, match and wildcard.");
          out.push({ ...condition, op });
        }
      }
      return;
    }
    default:
      throw mockError("INVALID_FILTER", `The dev mock understands term, terms, range, exists, match, prefix, wildcard and bool, not ${type}.`);
  }
}

function translateFilter(request: BrowseRequest): BrowseRequest {
  if (request.filter.kind !== "raw" || !request.filter.text.trim()) return request;
  let parsed: unknown;
  try {
    parsed = JSON.parse(request.filter.text);
  } catch (error) {
    throw mockError("INVALID_FILTER", `The filter must be a Query DSL query in JSON. (${String(error)})`, { filter: request.filter.text });
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
    throw mockError("INVALID_FILTER", "The filter must be a JSON object (a Query DSL query).");
  const denied = deniedKey(parsed);
  if (denied) throw mockError("INVALID_FILTER", `'${denied}' is not allowed in a filter: it runs code on the cluster or hides a query from the check.`);
  const query =
    (parsed as { query?: unknown }).query && Object.keys(parsed).length === 1
      ? (parsed as { query: Record<string, unknown> }).query
      : (parsed as Record<string, unknown>);
  const conditions: FilterCond[] = [];
  dslToConditions(query, conditions);
  return { ...request, filter: { kind: "conditions", conditions, matchAny: false } };
}

// --- The engine mock --------------------------------------------------------------------------------------------------

function treeNode(prefix: string, ref: ObjectRef, icon: string, detail: string, kindLabel: string): ObjectNode {
  const object: ObjectRef = { ...ref, path: [kindLabel] };
  return {
    key: { id: `${prefix}:${ref.name}`, kind: "collection", object },
    label: ref.name,
    kind: "collection",
    icon,
    object,
    detail,
    badges: [],
    expandable: false,
    defaultView: "data",
    actions: [
      viewAction("design", "Mapping", "design"),
      queryAction("search", "Search in console", `GET /${ref.name}/_search\n{\n  "query": { "match_all": {} },\n  "size": 20\n}`),
      queryAction("mapping", "Get mapping", `GET /${ref.name}/_mapping`),
      builtin("copyName", "Copy name", "copyName"),
    ],
  };
}

function folderNode(id: string, label: string, icon: string): ObjectNode {
  return {
    key: { id, kind: "folder", object: null },
    label,
    kind: "folder",
    icon,
    object: null,
    detail: null,
    badges: [],
    expandable: true,
    defaultView: null,
    actions: [builtin("refresh", "Refresh", "refresh")],
  };
}

const ref = (name: string): ObjectRef => ({ database: null, schema: null, name, kind: "collection" });

/** Builds the mock for a flavor (Elasticsearch here, OpenSearch in `opensearch.ts`). */
export function createSearchMock(flavor: Flavor, base: EngineDescriptor) {
  const catalog = catalogFor(flavor);
  const desc: EngineDescriptor = base;
  const table = (object: ObjectRef) => catalog.table({ database: "", schema: null, name: object.name === "products-current" ? "products" : object.name });
  const onTable = (object: ObjectRef): ObjectRef => ({
    ...object,
    database: "",
    schema: null,
    name: object.name === "products-current" ? "products" : object.name,
  });

  const connectInfo = (): ConnectInfo => ({
    engine: flavor.id,
    server: {
      product: flavor.label,
      version: flavor.version,
      label: `${flavor.label} ${flavor.version}`,
      flavor: flavor.id === "opensearch" ? "opensearch" : "default",
    },
    capabilities: desc.capabilities,
    languages: desc.languages,
    connectionActions: [queryAction("health", "Cluster health", "GET /_cluster/health"), queryAction("indices", "List indices", "GET /_cat/indices?v&s=index")],
  });

  const tree = (parent: TreeRequest["parent"]): ObjectNode[] => {
    if (!parent)
      return [
        folderNode("folder:indices", "Indices", "layers"),
        folderNode("folder:streams", "Data streams", "logs"),
        folderNode("folder:aliases", "Aliases", "bookmark"),
      ];
    const products = table(ref("products")).rows.length;
    if (parent.id === "folder:indices") return [treeNode("index", ref("products"), "table", `${products} docs · 412.3 KB`, "index")];
    if (parent.id === "folder:streams")
      return [treeNode("stream", ref("logs-app"), "logs", `${table(ref("logs-app")).rows.length} docs · 1.1 MB · 2 backing`, "dataStream")];
    if (parent.id === "folder:aliases") return [treeNode("alias", ref("products-current"), "bookmark", `→ products · ${products} docs · 412.3 KB`, "alias")];
    return [];
  };

  const mappingOf = (object: ObjectRef) => {
    const properties: Record<string, unknown> = {};
    for (const info of table(object).spec.columns) {
      if (info.name.startsWith("_")) continue;
      properties[info.name] =
        info.typeName === "text" ? { type: "text", fields: { keyword: { type: "keyword", ignore_above: 256 } } } : { type: info.typeName };
    }
    return { mappings: { properties } };
  };

  const describe = (object: ObjectRef): ObjectDescription => {
    const base = catalog.describe(onTable(object));
    const key = table(object).spec.key;
    return { ...base, object, primaryKey: { name: null, columns: key }, foreignKeys: [], indexes: [], ddl: JSON.stringify(mappingOf(object), null, 2) };
  };

  const browse = (request: BrowseRequest): Page => {
    const page = catalog.browse(translateFilter({ ...request, object: onTable(request.object) }));
    return {
      ...page,
      statement: `POST /${request.object.name}/_search\n${JSON.stringify({ query: request.filter.kind === "raw" ? JSON.parse(request.filter.text || "{}") : { match_all: {} }, from: request.offset, size: request.limit })}`,
    };
  };

  const apply = (changes: RowChangeSet): ApplyResult => catalog.apply({ ...changes, object: onTable(changes.object) });

  const hitsOf = (name: string, body: Record<string, unknown>) => {
    const size = Number(body.size ?? 10);
    const from = Number(body.from ?? 0);
    const query = (body.query ?? { match_all: {} }) as Record<string, unknown>;
    const request: BrowseRequest = {
      object: ref(name),
      filter: { kind: "raw", language: "esQueryDsl", text: JSON.stringify(query) },
      sort: [],
      offset: from,
      limit: Math.max(size, 1),
      after: null,
      estimateTotal: false,
      exactTotal: true,
      columns: null,
    };
    const page = browse(request);
    const hits = page.rows.slice(0, size).map((row) => {
      const source: Record<string, unknown> = {};
      page.columns.forEach((info, index) => {
        if (!info.name.startsWith("_") && row[index].t !== "null") source[info.name] = valueToJson(row[index]);
      });
      return { _index: name, _id: String(row[0].v), _score: 1, _source: source };
    });
    return { took: 3, timed_out: false, hits: { total: { value: page.total?.value ?? hits.length, relation: "eq" }, max_score: 1, hits } };
  };

  const respond = (method: string, path: string, body: Record<string, unknown> | null): { status: number; body: unknown } => {
    const clean = path.split("?")[0].replace(/^\/+/, "");
    const [first = "", api = ""] = clean.split("/");
    const known = ["products", "products-current", "logs-app"];
    if (clean === "")
      return {
        status: 200,
        body: {
          name: "mock-node",
          cluster_name: "fosta-dev",
          version: { number: flavor.version, ...(flavor.id === "opensearch" ? { distribution: "opensearch" } : {}) },
          tagline: flavor.id === "opensearch" ? "The OpenSearch Project" : "You Know, for Search",
        },
      };
    if (clean === "_cluster/health")
      return {
        status: 200,
        body: {
          cluster_name: "fosta-dev",
          status: "green",
          number_of_nodes: 1,
          number_of_data_nodes: 1,
          active_primary_shards: 4,
          active_shards: 4,
          unassigned_shards: 0,
        },
      };
    if (clean === "_cat/indices")
      return {
        status: 200,
        body: "health status index uuid pri rep docs.count\ngreen open products p1 1 0 300\ngreen open .ds-logs-app-2026.10.26-000001 l1 1 0 1000\ngreen open .ds-logs-app-2026.10.26-000002 l2 1 0 1000\n",
      };
    if (!first.startsWith("_") && !known.includes(first))
      return { status: 404, body: { error: { type: "index_not_found_exception", reason: `no such index [${first}]` }, status: 404 } };
    if (api === "_search") return { status: 200, body: hitsOf(first, body ?? {}) };
    if (api === "_count") return { status: 200, body: { count: hitsOf(first, { ...(body ?? {}), size: 10_000 }).hits.total.value } };
    if (api === "_mapping" && method === "GET") return { status: 200, body: { [first]: mappingOf(ref(first)) } };
    if (method === "GET" || method === "HEAD") return { status: 200, body: { acknowledged: true, note: "The dev mock knows only a few read APIs." } };
    return { status: 200, body: { acknowledged: true, result: "noop", note: "Acknowledged; the dev mock does not change its data from the console." } };
  };

  const run = async (request: RunRequest, emit: (event: StreamEvent) => void, context: RunContext): Promise<number> => {
    if (context.session.language !== "es-console") {
      emit({ event: "statement", index: 0, sql: request.text.trim() });
      emit({ event: "notice", index: 0, text: "The dev mock runs console requests only; ES|QL and SQL need a real cluster." });
      emit({ event: "done", index: 0, affected: null, rows: 0, truncated: false, elapsedMs: 1 });
      return 1;
    }
    const requests = parseConsole(request.text);
    for (const [index, item] of requests.entries()) {
      emit({ event: "statement", index, sql: `${item.method} ${item.path}` });
      await pause(60, context, () => mockError("CANCELLED", "The request was cancelled.", { engine: flavor.id }));
      const response = respond(item.method, item.path, item.body);
      if (response.status >= 400) {
        const error = (response.body as { error: { type: string; reason: string } }).error;
        throw { ...esError(response.status, error.type, error.reason, flavor.id), statement: { index, text: `${item.method} ${item.path}` } };
      }
      const hits = (response.body as { hits?: { hits?: { _index: string; _id: string; _score: number; _source: Record<string, unknown> }[] } }).hits?.hits;
      if (hits) {
        const fields = [...new Set(hits.flatMap((hit) => Object.keys(hit._source)))];
        const columns: ColumnInfo[] = [
          column("_index", "_index", "text"),
          column("_id", "_id", "text"),
          column("_score", "float", "float"),
          ...fields.map((field) => column(field, "", "json")),
        ];
        emit({ event: "columns", index, columns });
        const rows: Value[][] = hits.map((hit) => [
          v.text(hit._index),
          v.text(hit._id),
          v.float(hit._score),
          ...fields.map((field) => toValue(hit._source[field])),
        ]);
        emit({ event: "rows", index, rows });
        emit({ event: "done", index, affected: null, rows: rows.length, truncated: false, elapsedMs: 3 });
        emit({ event: "statement", index, sql: `${item.method} ${item.path} (response)` });
      }
      const text = typeof response.body === "string";
      emit({ event: "columns", index, columns: [column("status", "http status", "int"), column("response", text ? "text" : "json", text ? "text" : "json")] });
      emit({ event: "rows", index, rows: [[v.int(response.status), text ? v.text(String(response.body)) : v.json(response.body)]] });
      emit({ event: "done", index, affected: null, rows: 1, truncated: false, elapsedMs: 2 });
    }
    return requests.length;
  };

  return {
    descriptor: desc,
    connectInfo,
    tree,
    describe,
    browse,
    apply,
    run,
    classify,
    invoke: (name: string, args: unknown) => invokeOperation(name, args, mappingOf),
  };
}

function toValue(json: unknown): Value {
  if (json === null || json === undefined) return v.null();
  if (typeof json === "number") return Number.isInteger(json) ? v.int(json) : v.float(json);
  if (typeof json === "boolean") return v.bool(json);
  if (typeof json === "string") return v.text(json);
  return v.json(json);
}

function invokeOperation(name: string, args: unknown, mappingOf: (object: ObjectRef) => { mappings: { properties: Record<string, unknown> } }): unknown {
  if (name === "es.names") return { names: ["logs-app", "products", "products-current"] };
  if (name === "es.fields") {
    const index = String((args as { index?: string } | null)?.index ?? "products");
    const object = ref(index === "products-current" ? "products" : index);
    try {
      return {
        fields: Object.entries(mappingOf(object).mappings.properties).map(([field, definition]) => ({
          name: field,
          type: (definition as { type: string }).type,
        })),
      };
    } catch {
      return { fields: [] };
    }
  }
  if (name === "es.clusterHealth")
    return {
      status: "green",
      numberOfNodes: 1,
      numberOfDataNodes: 1,
      activePrimaryShards: 4,
      activeShards: 4,
      relocatingShards: 0,
      initializingShards: 0,
      unassignedShards: 0,
      activeShardsPercent: 100,
    };
  return undefined;
}

interface ConsoleItem {
  method: string;
  path: string;
  body: Record<string, unknown> | null;
}

/** Request blocks of the console text (bodies parsed as one JSON value; NDJSON bodies are kept unparsed). */
function parseConsole(text: string): ConsoleItem[] {
  const items: ConsoleItem[] = [];
  let current: { method: string; path: string; body: string[] } | null = null;
  const flush = () => {
    if (!current) return;
    const raw = current.body.join("\n").trim();
    let body: Record<string, unknown> | null = null;
    if (raw) {
      try {
        body = JSON.parse(raw) as Record<string, unknown>;
      } catch {
        body = null;
      }
    }
    items.push({ method: current.method, path: current.path.startsWith("/") ? current.path : `/${current.path}`, body });
  };
  for (const line of text.split("\n")) {
    const match = /^\s*(GET|POST|PUT|DELETE|HEAD)\s+(\S+)/i.exec(line);
    if (match) {
      flush();
      current = { method: match[1].toUpperCase(), path: match[2], body: [] };
    } else if (current && !/^\s*(#|\/\/)/.test(line)) current.body.push(line);
  }
  flush();
  if (items.length === 0) throw mockError("ENGINE_ERROR", "Write a request such as GET /_cluster/health.", { vendorCode: "console_parse_error" });
  return items;
}

/** How the gateway sees console text (mirrors `classify.rs`: the API by position, rewritten paths refused). */
export function classify(text: string): StatementInfo[] {
  let items: ConsoleItem[];
  try {
    items = parseConsole(text);
  } catch {
    return [{ category: "unknown", kind: "request", tables: [], excerpt: text.slice(0, 200), reason: "Not a console request." }];
  }
  return items.map((item) => {
    const excerpt = `${item.method} ${item.path}`;
    const raw = item.path.split("?")[0];
    if (/(^|\/)(\.|%2e){1,2}(\/|$)/i.test(raw) || raw.includes("#") || raw.includes("\\")) {
      return { category: "admin", kind: "invalid path", tables: [], excerpt, reason: "Refused: the path would be rewritten before it is sent." };
    }
    const segments = raw.split("/").filter(Boolean);
    const target = segments[0] && !segments[0].startsWith("_") ? segments[0] : null;
    const api = segments[target ? 1 : 0] ?? "";
    const tables = target ? [target] : [];
    const info = (category: StatementInfo["category"], kind: string, reason: string | null = null): StatementInfo => ({
      category,
      kind,
      tables,
      excerpt,
      reason,
    });
    if (["_search", "_count", "_mget", "_msearch", "_field_caps", "_validate", "_explain", "_query", "_pit", "_analyze"].includes(api))
      return info("read", api.slice(1));
    if (item.method === "GET" || item.method === "HEAD") return info("read", api.replace(/^_/, "") || "get");
    if (api === "_delete_by_query") return info("destructive", "delete by query", "Deletes every document the query matches.");
    if (api === "_update_by_query") return info("destructive", "update by query", "Changes every document the query matches.");
    if (["_doc", "_create", "_update", "_bulk", "_reindex"].includes(api))
      return info("write", item.method === "DELETE" ? "delete document" : "index document");
    if (api === "_close") return info("destructive", "close index");
    if (["_mapping", "_settings", "_alias", "_aliases", "_index_template", "_component_template", "_ingest", "_data_stream"].includes(api))
      return info(item.method === "DELETE" ? "destructive" : "ddl", api.slice(1));
    if (target && !api)
      return item.method === "DELETE" ? info("destructive", "delete index") : item.method === "PUT" ? info("ddl", "create index") : info("admin", "unknown");
    if (api === "_cluster" && segments[1] === "settings") return info("admin", "cluster settings", "Changes cluster-wide settings.");
    return info("admin", api.replace(/^_/, "") || "unknown");
  });
}

const mock = createSearchMock(ELASTIC, DESCRIPTORS.elasticsearch);
export const descriptor: EngineDescriptor = mock.descriptor;
export const connectInfo = mock.connectInfo;
export const tree: NonNullable<EngineMock["tree"]> = mock.tree;
export const describe: NonNullable<EngineMock["describe"]> = mock.describe;
export const browse: NonNullable<EngineMock["browse"]> = mock.browse;
export const apply: NonNullable<EngineMock["apply"]> = mock.apply;
export const run: NonNullable<EngineMock["run"]> = mock.run;
export const invoke: NonNullable<EngineMock["invoke"]> = mock.invoke;
