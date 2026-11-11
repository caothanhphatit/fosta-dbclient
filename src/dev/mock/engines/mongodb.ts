/**
 * MongoDB 8.0 mock: shop (products, customers, orders with nested documents and arrays) and events databases.
 * The console takes database commands as JSON (`find`, `aggregate`, `count`, `distinct`, `insert`, `update`, `delete`,
 * `listCollections`, `ping`, `buildInfo`, …); raw filters are query documents.
 */
import type { ColumnInfo, ConnectInfo, RunRequest, StreamEvent, Value } from "../../../core/contract";
import type { StatementInfo } from "../../../core/types";
import { valueToJson } from "../../../core/values";
import {
  builtin,
  column,
  mongoFilter,
  MockCatalog,
  objectIdFor,
  queryAction,
  v,
  viewAction,
  type CatalogStyle,
  type MockTable,
  type TableSpec,
} from "../catalog";
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mockError, positionAt } from "../errors";
import { pause } from "../sqlRun";
import type { EngineMock, RunContext } from "../types";

export const descriptor = DESCRIPTORS.mongodb;
export const order = ORDER.indexOf("mongodb");

const ENGINE = "mongodb";
const VERSION = "8.0.4";

const serverError = (vendorCode: number, codeName: string, message: string) =>
  mockError("ENGINE_ERROR", message, { engine: ENGINE, vendorCode, detail: `codeName: ${codeName}`, kind: "server", retryable: false });

const style: CatalogStyle = {
  engine: ENGINE,
  quote: (name) => name,
  label: (table) => table.name,
  rawFilter: "mongoJson",
  countUnit: "docs",
  databaseActions: [
    builtin("newQuery", "New query", "newQuery"),
    builtin("diagram", "Diagram", "diagram"),
    builtin("copyName", "Copy name", "copyName"),
    builtin("refresh", "Refresh", "refresh", true),
  ],
  tableActions: (table) => [
    viewAction("design", "Design", "design"),
    builtin("copyName", "Copy name", "copyName"),
    queryAction("drop", table.kind === "view" ? "Drop view…" : "Drop collection…", '{ "drop": "{name}" }', true),
  ],
  errors: {
    noTable: (name: string) => serverError(26, "NamespaceNotFound", `ns does not exist: ${name}`),
    noColumn: (name: string) => serverError(2, "BadValue", `unknown field: ${name}`),
    syntax: (near: string) => serverError(9, "FailedToParse", `Failed to parse: ${near}`),
  },
};

const TAGS = ["home", "office", "outdoor", "kids", "eco", "sale", "new", "bestseller"];
const NAMES = ["Bamboo lamp", "Cork board", "Felt basket", "Linen pillow", "Ceramic vase", "Rattan chair", "Wool throw", "Oak tray"];
const CITIES = ["Lisbon", "Berlin", "Toronto", "Melbourne", "Osaka", "Dublin", "Oslo", "Austin"];
const pad = (n: number, width = 2) => String(n).padStart(width, "0");
const oid = (collection: number, n: number) => v.oid(objectIdFor(1_760_000_000 + collection * 1000, n));

const tables: TableSpec[] = [
  {
    database: "shop",
    name: "products",
    kind: "collection",
    key: ["_id"],
    columns: [
      column("_id", "objectId", "objectId", { primary: true }),
      column("sku", "string", "text"),
      column("name", "string", "text"),
      column("price", "double", "float"),
      column("stock", "int", "int"),
      column("tags", "array", { kind: "array", of: { kind: "text" } }),
      column("dimensions", "object", "document"),
      column("updatedAt", "date", "timestampTz"),
    ],
    rows: () =>
      Array.from({ length: 320 }, (_, index) => {
        const n = index + 1;
        return [
          oid(1, n),
          v.text(`MG-${pad(n, 4)}`),
          v.text(`${NAMES[n % NAMES.length]} ${n}`),
          v.float(Math.round((9.99 + (n % 30) * 2.5) * 100) / 100),
          n % 8 === 0 ? v.int(0) : v.int((n * 11) % 90),
          v.arr([v.text(TAGS[n % TAGS.length]), v.text(TAGS[(n * 3) % TAGS.length])]),
          n % 7 === 0
            ? v.null()
            : v.doc([
                ["w", v.int(10 + (n % 20))],
                ["h", v.int(5 + (n % 15))],
                ["unit", v.text("cm")],
              ]),
          v.tstz(`2026-10-${pad((n % 28) + 1)} 12:${pad(n % 60)}:00+00:00`),
        ];
      }),
    indexes: [{ name: "sku_1", columns: ["sku"], unique: true, primary: false, method: null, definition: '{ "sku": 1 }' }],
  },
  {
    database: "shop",
    name: "customers",
    kind: "collection",
    key: ["_id"],
    columns: [
      column("_id", "objectId", "objectId", { primary: true }),
      column("name", "string", "text"),
      column("email", "string", "text"),
      column("address", "object", "document"),
      column("loyaltyPoints", "int", "int"),
      column("createdAt", "date", "timestampTz"),
    ],
    rows: () =>
      Array.from({ length: 150 }, (_, index) => {
        const n = index + 1;
        const city = CITIES[n % CITIES.length];
        return [
          oid(2, n),
          v.text(`Customer ${n}`),
          v.text(`customer${n}@example.org`),
          v.doc([
            ["city", v.text(city)],
            ["zip", v.text(String(10000 + n * 37))],
          ]),
          v.int((n * 53) % 2000),
          v.tstz(`2025-${pad((n % 12) + 1)}-${pad((n % 28) + 1)} 08:00:00+00:00`),
        ];
      }),
  },
  {
    database: "shop",
    name: "orders",
    kind: "collection",
    key: ["_id"],
    columns: [
      column("_id", "objectId", "objectId", { primary: true }),
      column("customerId", "objectId", "objectId"),
      column("status", "string", "text"),
      column("items", "array", { kind: "array", of: { kind: "document" } }),
      column("total", "decimal", "decimal"),
      column("placedAt", "date", "timestampTz"),
    ],
    rows: () =>
      Array.from({ length: 500 }, (_, index) => {
        const n = index + 1;
        return [
          oid(3, n),
          oid(2, ((n * 7) % 150) + 1),
          v.text(["new", "paid", "shipped", "returned"][n % 4]),
          v.arr(
            Array.from({ length: (n % 3) + 1 }, (_, line) =>
              v.doc([
                ["sku", v.text(`MG-${pad(((n + line) % 320) + 1, 4)}`)],
                ["qty", v.int(line + 1)],
              ]),
            ),
          ),
          v.dec((20 + ((n * 41) % 400) + 0.5).toFixed(2)),
          v.tstz(`2026-${pad((n % 10) + 1)}-${pad((n % 28) + 1)} 15:${pad(n % 60)}:00+00:00`),
        ];
      }),
  },
  {
    database: "shop",
    name: "top_products",
    kind: "view",
    key: [],
    columns: [column("_id", "string", "text"), column("sold", "int", "int")],
    rows: () => Array.from({ length: 10 }, (_, index) => [v.text(`MG-${pad(index * 13 + 1, 4)}`), v.int(400 - index * 31)]),
    comment: "orders → $unwind items → $group by sku",
  },
  {
    database: "events",
    name: "pageviews",
    kind: "collection",
    key: ["_id"],
    columns: [
      column("_id", "objectId", "objectId", { primary: true }),
      column("path", "string", "text"),
      column("userAgent", "string", "text"),
      column("durationMs", "int", "int"),
      column("at", "date", "timestampTz"),
    ],
    rows: () =>
      Array.from({ length: 800 }, (_, index) => {
        const n = index + 1;
        return [
          oid(4, n),
          v.text(["/", "/catalog", "/catalog/lamps", "/cart", "/checkout"][n % 5]),
          v.text(n % 3 ? "Mozilla/5.0 (Windows NT 10.0)" : "Mozilla/5.0 (Macintosh)"),
          v.int(200 + ((n * 97) % 5000)),
          v.tstz(`2026-11-${pad((n % 7) + 1)} ${pad(n % 24)}:${pad(n % 60)}:00+00:00`),
        ];
      }),
  },
];

const catalog = new MockCatalog(style, [
  { name: "shop", tables: tables.filter((table) => table.database === "shop") },
  { name: "events", tables: tables.filter((table) => table.database === "events") },
  ...["admin", "config", "local"].map((name) => ({ name, system: true, tables: [] })),
]);

export const connectInfo = (): ConnectInfo => ({
  engine: ENGINE,
  server: { product: "MongoDB", version: VERSION, label: `MongoDB ${VERSION}`, flavor: "standalone" },
  capabilities: descriptor.capabilities,
  languages: descriptor.languages,
  connectionActions: [builtin("createDatabase", "New database…", "createDatabase"), builtin("refresh", "Refresh", "refresh")],
});
export const tree: NonNullable<EngineMock["tree"]> = (parent) => catalog.tree(parent);
export const describe: NonNullable<EngineMock["describe"]> = (object) => catalog.describe(object);
export const browse: NonNullable<EngineMock["browse"]> = (request) => catalog.browse(request);
export const apply: NonNullable<EngineMock["apply"]> = (changes) => catalog.apply(changes);
export const schemaGraph = (database: string) => catalog.schemaGraph(database);
export const completion = (database: string) => catalog.completion(database);
export const createDatabase = (name: string, firstCollection: string | null) =>
  catalog.addDatabase(
    name,
    firstCollection
      ? [
          {
            database: name,
            name: firstCollection,
            kind: "collection",
            key: ["_id"],
            columns: [column("_id", "objectId", "objectId", { primary: true })],
            rows: () => [],
          },
        ]
      : [],
  );
export const touchRow = (table: string, key: unknown, remove: boolean, column?: string) => catalog.touch(table, key, remove, column);

// --- Engine operations (`invoke_capability`), like src-tauri/src/engines/mongo/ops.rs ------------------------------------

const JS_OPERATORS = ["$where", "$function", "$accumulator"];
const MAX_FIND_LIMIT = 1000;
const MAX_DISTINCT_LIMIT = 1000;
type Json = unknown;
type Doc = Record<string, Json>;

function deniedOperator(value: Json): string | null {
  if (Array.isArray(value)) return value.map(deniedOperator).find((found) => found !== null) ?? null;
  if (typeof value !== "object" || value === null) return null;
  for (const [key, inner] of Object.entries(value)) {
    if (JS_OPERATORS.includes(key)) return key;
    const found = deniedOperator(inner);
    if (found) return found;
  }
  return null;
}

/** A query-document argument: a JSON value as given, JSON text parsed (empty text is absent). */
function jsonArg(args: Doc, key: string): Json {
  const value = args[key];
  if (typeof value !== "string") return value ?? undefined;
  if (!value.trim()) return undefined;
  try {
    return JSON.parse(value) as Json;
  } catch (error) {
    throw mockError("ENGINE_ERROR", `${key} must be a JSON document (${error instanceof Error ? error.message : String(error)})`);
  }
}

/** Canonical number wrappers as the plain values the mock's rows compare with (decimals are text there). */
function plain(value: Json): Json {
  if (Array.isArray(value)) return value.map(plain);
  if (typeof value !== "object" || value === null) return value;
  const object = value as Doc;
  for (const key of ["$numberInt", "$numberLong", "$numberDouble"]) if (key in object) return Number(object[key]);
  if ("$numberDecimal" in object) return String(object.$numberDecimal);
  return Object.fromEntries(Object.entries(object).map(([key, inner]) => [key, plain(inner)]));
}

/** Relaxed Extended JSON of a mock value. */
function ejson(value: Value): Json {
  switch (value.t) {
    case "objectId":
      return { $oid: value.v };
    case "decimal":
      return { $numberDecimal: value.v };
    case "timestamptz":
    case "timestamp":
    case "date":
      return { $date: new Date(value.v.replace(" ", "T")).toISOString() };
    case "array":
      return value.v.map(ejson);
    case "doc":
      return Object.fromEntries(value.v.map(([key, item]) => [key, ejson(item)]));
    default:
      return valueToJson(value);
  }
}

function documentOf(table: MockTable, row: Value[]): Doc {
  return Object.fromEntries(table.spec.columns.flatMap((spec, index) => (row[index].t === "null" ? [] : [[spec.name, ejson(row[index])]])));
}

/** The values at a dotted path, through arrays (like a query path). */
function pathOf(document: Json, path: string): Json[] {
  let current: Json[] = [document];
  for (const part of path.split(".")) {
    current = current
      .flatMap((item) => (Array.isArray(item) ? item : [item]))
      .filter((item): item is Doc => typeof item === "object" && item !== null)
      .map((item) => item[part])
      .filter((item) => item !== undefined);
  }
  return current.flatMap((item) => (Array.isArray(item) ? item : [item]));
}

function sortKey(value: Json): string | number {
  if (typeof value === "number") return value;
  if (typeof value === "object" && value !== null) {
    const object = value as Doc;
    if ("$oid" in object) return String(object.$oid);
    if ("$date" in object) return String(object.$date);
    if ("$numberDecimal" in object) return Number(object.$numberDecimal);
  }
  return typeof value === "string" ? value : JSON.stringify(value ?? null);
}

function compare(a: Json, b: Json): number {
  const [x, y] = [sortKey(a), sortKey(b)];
  return typeof x === "number" && typeof y === "number" ? x - y : String(x).localeCompare(String(y));
}

/** Inclusion or exclusion projections; computed fields are not evaluated by the mock. */
function project(document: Doc, projection: Json): Doc {
  if (typeof projection !== "object" || projection === null) return document;
  const entries = Object.entries(projection as Doc);
  const included = entries.filter(([, flag]) => flag === 1 || flag === true).map(([key]) => key);
  const excluded = entries.filter(([, flag]) => flag === 0 || flag === false).map(([key]) => key);
  if (included.length > 0) {
    const keep = new Set(excluded.includes("_id") ? included : ["_id", ...included]);
    return Object.fromEntries(Object.entries(document).filter(([key]) => keep.has(key)));
  }
  return Object.fromEntries(Object.entries(document).filter(([key]) => !excluded.includes(key)));
}

/** The collection the arguments name; null when it does not exist (MongoDB reads it as empty). */
function operationTable(args: Doc): MockTable | null {
  const database = String(args.database ?? "");
  const name = String(args.collection ?? "");
  if (!database || !name) throw mockError("ENGINE_ERROR", "database and collection are required");
  const table = catalog.resolve(name, database);
  return table && table.spec.database === database ? table : null;
}

function matching(table: MockTable | null, args: Doc): Doc[] {
  if (!table) return [];
  const filter = plain(jsonArg(args, "filter"));
  const predicate = filter === undefined ? () => true : mongoFilter(table, JSON.stringify(filter));
  return table.rows.filter(predicate).map((row) => documentOf(table, row));
}

const bounded = (value: Json, fallback: number, max: number) => Math.min(Math.max(Number(value ?? fallback) || fallback, 1), max);

function find(args: Doc): Json {
  const table = operationTable(args);
  const sortArg = jsonArg(args, "sort") as Doc | undefined;
  const sort: [string, number][] =
    sortArg && Object.keys(sortArg).length > 0 ? Object.entries(sortArg).map(([key, way]) => [key, Number(plain(way)) < 0 ? -1 : 1]) : [["_id", 1]];
  const keyset = sort.length === 1 && sort[0][0] === "_id";
  const limit = bounded(args.limit, 50, MAX_FIND_LIMIT);
  const skip = Math.max(0, Number(args.skip ?? 0) || 0);
  let documents = matching(table, args).sort((a, b) => {
    for (const [key, way] of sort) {
      const order = compare(pathOf(a, key)[0], pathOf(b, key)[0]) * way;
      if (order !== 0) return order;
    }
    return 0;
  });
  const after = jsonArg(args, "after");
  if (after !== undefined) {
    if (!keyset) throw mockError("ENGINE_ERROR", "after pages by _id, so it needs the sort to be _id alone");
    documents = documents.filter((document) => compare(document._id, plain(after)) * sort[0][1] > 0);
  }
  const page = documents.slice(skip, skip + limit + 1);
  const hasMore = page.length > limit;
  const shown = page.slice(0, limit).map((document) => project(document, jsonArg(args, "projection")));
  const last = shown[shown.length - 1];
  const sortDoc = Object.fromEntries(sort);
  const filterText = JSON.stringify(jsonArg(args, "filter") ?? {});
  return {
    documents: shown,
    hasMore,
    keyset,
    nextAfter: keyset && hasMore && last?._id !== undefined ? last._id : null,
    sort: sortDoc,
    elapsedMs: 3,
    statement: `db.getCollection(${JSON.stringify(String(args.collection))}).find(${filterText}).sort(${JSON.stringify(sortDoc)})${skip ? `.skip(${skip})` : ""}.limit(${limit})`,
  };
}

function count(args: Doc): Json {
  const table = operationTable(args);
  const filter = jsonArg(args, "filter");
  const unfiltered = filter === undefined || (typeof filter === "object" && filter !== null && Object.keys(filter).length === 0);
  if (args.estimated === true && unfiltered) return { count: table?.rows.length ?? 0, estimated: true, elapsedMs: 1 };
  let n = matching(table, args).length - Math.max(0, Number(args.skip ?? 0) || 0);
  if (Number(args.limit) > 0) n = Math.min(n, Number(args.limit));
  return { count: Math.max(0, n), estimated: false, elapsedMs: 2 };
}

function distinct(args: Doc): Json {
  const field = String(args.field ?? "").trim();
  if (!field || field.split(".").some((part) => !part || part.startsWith("$"))) throw mockError("ENGINE_ERROR", `'${field}' is not a field path`);
  const limit = bounded(args.limit, 100, MAX_DISTINCT_LIMIT);
  const seen = new Map<string, Json>();
  for (const document of matching(operationTable(args), args)) {
    for (const value of pathOf(document, field)) if (value !== null) seen.set(JSON.stringify(value), value);
  }
  const values = [...seen.values()].sort(compare);
  return { values: values.slice(0, limit), truncated: values.length > limit, elapsedMs: 2 };
}

function resolveRef(args: Doc): Json {
  if (!("id" in args)) throw mockError("ENGINE_ERROR", "id is required");
  const id = JSON.stringify(plain(args.id));
  const table = operationTable(args);
  const found = table?.rows.map((row) => documentOf(table, row)).find((document) => JSON.stringify(document._id) === id);
  return { document: found ? project(found, jsonArg(args, "projection")) : null };
}

const OPERATIONS: Record<string, (args: Doc) => Json> = {
  "mongo.find": find,
  "mongo.count": count,
  "mongo.distinct": distinct,
  "mongo.resolveRef": resolveRef,
};

/** `mongo.find`, `mongo.count`, `mongo.distinct`, `mongo.resolveRef`; undefined (refused) for any other name. */
export const invoke: NonNullable<EngineMock["invoke"]> = (name, raw) => {
  const operation = Object.hasOwn(OPERATIONS, name) ? OPERATIONS[name] : undefined;
  if (!operation) return undefined;
  const args = (raw ?? {}) as Doc;
  const operator = deniedOperator(args) ?? ["filter", "projection", "sort"].map((key) => deniedOperator(jsonArg(args, key))).find(Boolean);
  if (operator) throw mockError("BLOCKED", `Refused: ${operator} runs JavaScript on the server.`);
  return operation(args);
};

const READ = new Set([
  "find",
  "aggregate",
  "count",
  "countDocuments",
  "distinct",
  "listCollections",
  "listDatabases",
  "listIndexes",
  "ping",
  "buildInfo",
  "serverStatus",
  "dbStats",
  "collStats",
  "explain",
]);
const WRITE = new Set(["insert", "update", "delete", "findAndModify"]);

function commandOf(text: string): { name: string; body: Record<string, unknown> } | null {
  try {
    const body = JSON.parse(text) as unknown;
    if (typeof body !== "object" || body === null || Array.isArray(body)) return null;
    const name = Object.keys(body)[0];
    return name ? { name, body: body as Record<string, unknown> } : null;
  } catch {
    return null;
  }
}

export function classify(text: string): StatementInfo[] {
  const command = commandOf(text);
  const name = command?.name ?? "unknown";
  const target = typeof command?.body[name] === "string" ? [String(command.body[name])] : [];
  const category = READ.has(name)
    ? "read"
    : WRITE.has(name)
      ? "write"
      : name === "drop" || name === "dropDatabase"
        ? "destructive"
        : name === "create" || name === "createIndexes"
          ? "ddl"
          : "unknown";
  return [
    {
      category,
      kind: name,
      tables: target,
      excerpt: text.trim().slice(0, 200),
      reason: category === "destructive" ? "Removes the collection and its documents" : null,
    },
  ];
}

export async function run(request: RunRequest, emit: (event: StreamEvent) => void, context: RunContext): Promise<number> {
  const text = request.text.trim();
  emit({ event: "statement", index: 0, sql: text });
  await pause(80, context, () => mockError("CANCELLED", "operation was interrupted", { engine: ENGINE, vendorCode: 11601 }));
  const started = performance.now();
  let body: unknown;
  try {
    body = JSON.parse(text);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const at = Number(/position (\d+)/.exec(message)?.[1] ?? 0);
    throw mockError("ENGINE_ERROR", `The command is not valid JSON: ${message}`, {
      engine: ENGINE,
      kind: "input",
      hint: 'Write a database command as JSON, e.g. { "find": "products", "filter": { "stock": 0 }, "limit": 50 }',
      position: positionAt(request.text, request.text.indexOf(text) + at),
      statement: { index: 0, text: text.slice(0, 200) },
    });
  }
  if (typeof body !== "object" || body === null || Array.isArray(body)) throw serverError(9, "FailedToParse", "A database command must be a JSON object");
  const command = body as Record<string, unknown>;
  const name = Object.keys(command)[0] ?? "";
  const database = (typeof command.$db === "string" ? command.$db : null) ?? request.database ?? context.session.database ?? "shop";
  const done = (rows: number, affected: number | null) =>
    emit({ event: "done", index: 0, affected, rows, truncated: false, elapsedMs: Math.max(1, Math.round(performance.now() - started)) });
  const documents = (columns: ColumnInfo[], rows: Value[][]) => {
    emit({ event: "columns", index: 0, columns });
    for (let start = 0; start < rows.length; start += 100) emit({ event: "rows", index: 0, rows: rows.slice(start, start + 100) });
    done(rows.length, null);
  };
  const collection = () => {
    const table = catalog.resolve(String(command[name]), database);
    if (!table) throw serverError(26, "NamespaceNotFound", `ns does not exist: ${database}.${String(command[name])}`);
    return table;
  };
  switch (name) {
    case "ping":
      documents([column("ok", "double", "float")], [[v.float(1)]]);
      break;
    case "buildInfo":
      documents(
        [column("version", "string", "text"), column("gitVersion", "string", "text"), column("ok", "double", "float")],
        [[v.text(VERSION), v.text("dev-mock"), v.float(1)]],
      );
      break;
    case "listDatabases":
      documents(
        [column("name", "string", "text"), column("empty", "bool", "bool")],
        catalog.databases.map((item) => [v.text(item.name), v.bool(item.tables.length === 0)]),
      );
      break;
    case "listCollections":
      documents(
        [column("name", "string", "text"), column("type", "string", "text")],
        catalog.tablesOf(database).map((table) => [v.text(table.spec.name), v.text(table.spec.kind === "view" ? "view" : "collection")]),
      );
      break;
    case "find": {
      const table = collection();
      const filter = command.filter ? mongoFilter(table, JSON.stringify(command.filter)) : () => true;
      const skip = Number(command.skip ?? 0);
      const limit = Number(command.limit ?? 101);
      const rows = table.rows.filter(filter).slice(skip, skip + limit);
      // Like a real find: only the fields some returned document has.
      const keep = table.spec.columns.map((_, index) => index).filter((index) => index === 0 || rows.some((row) => row[index].t !== "null"));
      documents(
        keep.map((index) => table.spec.columns[index]),
        rows.map((row) => keep.map((index) => row[index])),
      );
      break;
    }
    case "count":
    case "countDocuments": {
      const table = collection();
      const filter = command.query ?? command.filter;
      const n = filter ? table.rows.filter(mongoFilter(table, JSON.stringify(filter))).length : table.rows.length;
      documents([column("n", "int", "int"), column("ok", "double", "float")], [[v.int(n), v.float(1)]]);
      break;
    }
    case "distinct": {
      const table = collection();
      const key = String(command.key ?? "");
      const index = table.columnIndex(key);
      if (index < 0) throw serverError(2, "BadValue", `unknown field: ${key}`);
      const values = [...new Set(table.rows.map((row) => JSON.stringify(valueToJson(row[index]))))].slice(0, 200);
      documents(
        [column("value", "string", "text")],
        values.map((item) => [v.text(item)]),
      );
      break;
    }
    case "aggregate": {
      const table = collection();
      const pipeline = Array.isArray(command.pipeline) ? command.pipeline : [];
      const match = pipeline.find((stage) => typeof stage === "object" && stage !== null && "$match" in stage) as { $match?: unknown } | undefined;
      const limitStage = pipeline.find((stage) => typeof stage === "object" && stage !== null && "$limit" in stage) as { $limit?: number } | undefined;
      const rows = table.rows.filter(match ? mongoFilter(table, JSON.stringify(match.$match)) : () => true).slice(0, limitStage?.$limit ?? 20);
      emit({ event: "notice", index: 0, text: "The dev mock applies $match and $limit only; other stages are ignored." });
      documents(table.spec.columns, rows);
      break;
    }
    case "insert":
    case "update":
    case "delete": {
      collection();
      const list = command[name === "insert" ? "documents" : name === "update" ? "updates" : "deletes"];
      const n = Array.isArray(list) ? list.length : 1;
      emit({
        event: "notice",
        index: 0,
        text: `${n} document(s) ${name === "insert" ? "inserted" : name === "update" ? "matched" : "deleted"} (the dev mock does not change its data from commands)`,
      });
      done(0, n);
      break;
    }
    case "drop":
    case "create":
    case "createIndexes":
      emit({ event: "notice", index: 0, text: `${name} acknowledged (not applied by the dev mock)` });
      done(0, 0);
      break;
    default:
      throw { ...serverError(59, "CommandNotFound", `no such command: '${name}'`), statement: { index: 0, text: text.slice(0, 200) } };
  }
  return 1;
}
