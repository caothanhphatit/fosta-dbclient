/**
 * ClickHouse 25.8 mock: a `shop` database with MergeTree tables (editable, as mutations), a Log table (read-only), a view,
 * a materialized view and a dictionary, laid out like the backend's tree (folders, engine · rows · size, a "read-only"
 * badge). It answers the engine's operations (`clickhouse.parts`, `.mutations`, `.processes`, `.queryLog`, `.kill`), keeps
 * the mutations your edits create, and logs the statements you run.
 *
 * Conflict demo: open `shop.customers`, edit a name, then in the browser console run
 * `__fostaMock.touchRow("c-local", "shop.customers", 3)` (someone else changes the row) and save.
 */
import type { ConnectInfo, EngineDescriptor, ObjectDescription, ObjectNode, ObjectRef, RowChangeSet, RunRequest } from "../../../core/contract";
import { builtin, column, MockCatalog, queryAction, v, viewAction, type CatalogStyle, type TableSpec } from "../catalog";
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mockError } from "../errors";
import { classifySql, runSql, type SqlDialectMock } from "../sqlRun";
import type { EngineMock } from "../types";
import type { StatementInfo } from "../../../core/types";

// The backend's descriptor is ready (the shared dump still says planned).
export const descriptor: EngineDescriptor = DESCRIPTORS.clickhouse;
export const order = ORDER.indexOf("clickhouse");

const ENGINE = "clickhouse";
const VERSION = "25.8.12.28";

const serverError = (vendorCode: number, name: string, message: string, extra: Record<string, unknown> = {}) =>
  mockError("ENGINE_ERROR", `${message} (${name})`, { engine: ENGINE, vendorCode: String(vendorCode), kind: "server", retryable: false, ...extra });

const errors = {
  syntax: (near: string) =>
    serverError(62, "SYNTAX_ERROR", `Syntax error: failed at position 1 (${near.split(/\s+/)[0]}): ${near}.`, {
      detail: "Expected one of: Query, SELECT, INSERT, ALTER, CREATE, DROP, SHOW, EXPLAIN, WITH",
    }),
  noTable: (name: string) => serverError(60, "UNKNOWN_TABLE", `Unknown table expression identifier '${name}' in scope SELECT`),
  noColumn: (name: string, where: string) => serverError(47, "UNKNOWN_IDENTIFIER", `Unknown expression identifier '${name}' in scope ${where}`),
  cancelled: () => mockError("CANCELLED", "The statement was cancelled.", { engine: ENGINE, vendorCode: "394", kind: "cancelled", retryable: false }),
};

const style: CatalogStyle = {
  engine: ENGINE,
  quote: (name) => `\`${name.replace(/`/g, "\\`")}\``,
  label: (table) => table.name,
  rawFilter: "sql",
  countUnit: "rows",
  databaseActions: [],
  tableActions: () => [],
  errors,
};

const pad = (n: number, width = 2) => String(n).padStart(width, "0");
const STATUS = ["paid", "shipped", "pending", "refunded"];
const COUNTRIES = ["VN", "US", "DE", "JP", "FR"];

/** Table engines by `database.table`; the catalog only knows tables and views. */
const ENGINES: Record<string, string> = {
  "shop.customers": "MergeTree",
  "shop.orders": "MergeTree",
  "shop.products": "ReplacingMergeTree",
  "shop.page_views": "Log",
  "shop.order_totals": "View",
  "shop.daily_revenue_mv": "MaterializedView",
  "shop.category_dict": "Dictionary",
};

const tables: TableSpec[] = [
  {
    database: "shop",
    name: "customers",
    kind: "table",
    key: ["id"],
    columns: [
      column("id", "UInt64", "int", { primary: true, comment: "Customer number" }),
      column("email", "String", "text", { nullable: false }),
      column("full_name", "String", "text", { nullable: false }),
      column("country", "LowCardinality(String)", "text", { nullable: false, default: "'VN'" }),
      column("created_at", "DateTime('UTC')", "timestamp", { nullable: false }),
      column("vip", "Bool", "bool", { nullable: false, default: "false" }),
    ],
    rows: () =>
      Array.from({ length: 500 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(n),
          v.text(`customer${n}@example.com`),
          v.text(`Customer ${n}`),
          v.text(COUNTRIES[n % COUNTRIES.length]),
          v.ts(`2025-${pad((n % 12) + 1)}-${pad((n % 28) + 1)} 10:${pad(n % 60)}:00`),
          v.bool(n % 17 === 0),
        ];
      }),
    extra: [
      ["engine", "MergeTree"],
      ["sortingKey", "id"],
      ["primaryKey", "id"],
    ],
  },
  {
    database: "shop",
    name: "orders",
    kind: "table",
    key: ["customer_id", "id"],
    columns: [
      column("customer_id", "UInt64", "int", { primary: true }),
      column("id", "UInt64", "int", { primary: true }),
      column("status", "Enum8('pending' = 1, 'paid' = 2, 'shipped' = 3, 'refunded' = 4)", "text", { nullable: false }),
      column("total", "Decimal(12, 2)", "decimal", { nullable: false }),
      column("ordered_at", "DateTime('UTC')", "timestamp", { nullable: false }),
      column("note", "Nullable(String)", "text"),
    ],
    rows: () =>
      Array.from({ length: 4000 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(((n * 7) % 500) + 1),
          v.int(n),
          v.text(STATUS[n % STATUS.length]),
          v.dec((20 + ((n * 53) % 900) + 0.5).toFixed(2)),
          v.ts(`2025-${pad((n % 12) + 1)}-${pad((n % 28) + 1)} 14:${pad(n % 60)}:00`),
          n % 5 === 0 ? v.text(`gift wrap ${n}`) : v.null(),
        ];
      }),
    extra: [
      ["engine", "MergeTree"],
      ["partitionKey", "toYYYYMM(ordered_at)"],
      ["sortingKey", "customer_id, id"],
      ["primaryKey", "customer_id, id"],
    ],
  },
  {
    database: "shop",
    name: "products",
    kind: "table",
    key: ["id"],
    columns: [
      column("id", "UInt32", "int", { primary: true }),
      column("name", "String", "text", { nullable: false }),
      column("price", "Decimal(10, 2)", "decimal", { nullable: false }),
      column("tags", "Array(String)", "array"),
      column("version", "UInt32", "int", { nullable: false }),
    ],
    rows: () =>
      Array.from({ length: 60 }, (_, index) => {
        const n = index + 1;
        return [v.int(n), v.text(`Product ${n}`), v.dec((4.5 + n * 1.25).toFixed(2)), v.arr([v.text("new"), v.text(n % 2 ? "sale" : "eco")]), v.int(1)];
      }),
    extra: [
      ["engine", "ReplacingMergeTree"],
      ["sortingKey", "id"],
      ["primaryKey", "id"],
    ],
  },
  {
    database: "shop",
    name: "page_views",
    kind: "table",
    key: [],
    columns: [
      column("at", "DateTime('UTC')", "timestamp", { nullable: false }),
      column("url", "String", "text", { nullable: false }),
      column("ms", "UInt32", "int", { nullable: false }),
    ],
    rows: () =>
      Array.from({ length: 300 }, (_, index) => [
        v.ts(`2026-11-0${(index % 8) + 1} 09:${pad(index % 60)}:00`),
        v.text(`/products/${(index * 7) % 60}`),
        v.int(20 + ((index * 13) % 400)),
      ]),
    extra: [["engine", "Log"]],
  },
  {
    database: "shop",
    name: "order_totals",
    kind: "view",
    key: [],
    columns: [column("customer_id", "UInt64", "int", { nullable: false }), column("total", "Decimal(38, 2)", "decimal", { nullable: false })],
    rows: () => Array.from({ length: 40 }, (_, index) => [v.int(index + 1), v.dec((100 + index * 37.5).toFixed(2))]),
    ddl: "CREATE VIEW shop.order_totals AS\nSELECT customer_id, sum(total) AS total FROM shop.orders GROUP BY customer_id",
    extra: [["engine", "View"]],
  },
  {
    database: "shop",
    name: "daily_revenue_mv",
    kind: "view",
    key: [],
    columns: [column("day", "Date", "date", { nullable: false }), column("revenue", "Decimal(38, 2)", "decimal", { nullable: false })],
    rows: () => Array.from({ length: 30 }, (_, index) => [v.date(`2026-10-${pad(index + 1)}`), v.dec((1000 + index * 83.25).toFixed(2))]),
    ddl: "CREATE MATERIALIZED VIEW shop.daily_revenue_mv ENGINE = SummingMergeTree ORDER BY day AS\nSELECT toDate(ordered_at) AS day, sum(total) AS revenue FROM shop.orders GROUP BY day",
    extra: [["engine", "MaterializedView"]],
  },
  {
    database: "shop",
    name: "category_dict",
    kind: "view",
    key: [],
    columns: [column("id", "UInt64", "int", { nullable: false }), column("name", "String", "text", { nullable: false })],
    rows: () => ["Electronics", "Books", "Home", "Phones", "Laptops"].map((name, index) => [v.int(index + 1), v.text(name)]),
    ddl: "CREATE DICTIONARY shop.category_dict (`id` UInt64, `name` String)\nPRIMARY KEY id SOURCE(CLICKHOUSE(TABLE 'categories' DB 'shop')) LIFETIME(MIN 0 MAX 300) LAYOUT(HASHED())",
    extra: [["engine", "Dictionary"]],
  },
];

const catalog = new MockCatalog(style, [
  { name: "shop", tables },
  { name: "default", tables: [] },
  { name: "system", system: true, tables: [] },
]);
const dialect: SqlDialectMock = { split: {}, errors, version: VERSION };

const keyOf = (object: Pick<ObjectRef, "database" | "name">): string => `${object.database ?? "default"}.${object.name}`;
const engineOf = (object: Pick<ObjectRef, "database" | "name">): string => ENGINES[keyOf(object)] ?? "MergeTree";
const mutable = (engine: string): boolean => engine.endsWith("MergeTree");

const READ_ONLY_REASON = (name: string, engine: string): string =>
  `${name} uses the ${engine} engine. ClickHouse can change rows (as mutations) only in MergeTree tables, so this table is read-only here. Use an INSERT statement in a query tab to add rows.`;

function human(bytes: number): string {
  const units = ["B", "KiB", "MiB", "GiB"];
  let size = bytes;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B` : `${size.toFixed(1).replace(/\.0$/, "")} ${units[unit]}`;
}

function shortCount(n: number): string {
  return n < 10_000 ? String(n) : n < 1_000_000 ? `${(n / 1000).toFixed(1).replace(/\.0$/, "")}K` : `${(n / 1e6).toFixed(1).replace(/\.0$/, "")}M`;
}

const FOLDERS: [string, string, string[]][] = [
  ["tables", "Tables", ["table"]],
  ["views", "Views", ["view"]],
  ["materialized", "Materialized views", ["materializedView"]],
  ["dictionaries", "Dictionaries", ["dictionary"]],
];

function kindOf(engine: string): string {
  return engine === "View" ? "view" : engine === "MaterializedView" ? "materializedView" : engine === "Dictionary" ? "dictionary" : "table";
}

function databaseNode(name: string, system: boolean): ObjectNode {
  const object: ObjectRef = { database: name, schema: null, name, kind: "database" };
  return {
    key: { id: `db:${name}`, kind: "database", object },
    label: name,
    kind: "database",
    icon: "database",
    object,
    detail: system ? "system" : null,
    badges: [],
    expandable: true,
    defaultView: null,
    actions: [builtin("newQuery", "New query", "newQuery"), builtin("copyName", "Copy name", "copyName"), builtin("refresh", "Refresh", "refresh", true)],
  };
}

function objectNode(database: string, folder: string, name: string, rows: number): ObjectNode {
  const engine = engineOf({ database, name });
  const kind = kindOf(engine);
  const object: ObjectRef = { database, schema: null, name, kind };
  const isTable = kind === "table";
  return {
    key: { id: `db:${database}/folder:${folder}/${kind}:${name}`, kind, object },
    label: name,
    kind,
    icon: kind === "table" || kind === "dictionary" ? "table" : "view",
    object,
    detail: isTable ? [engine, ...(mutable(engine) || rows > 0 ? [`${shortCount(rows)} rows`, human(rows * 84)] : [])].join(" · ") : null,
    badges: isTable && !mutable(engine) ? [{ text: "read-only", tone: "neutral" }] : [],
    expandable: false,
    defaultView: "data",
    actions: [
      viewAction("design", "Design", "design"),
      builtin("copyName", "Copy name", "copyName"),
      ...(isTable ? [viewAction("parts", "Parts and mutations", "parts"), queryAction("truncate", "Truncate…", "TRUNCATE TABLE {qualified}", true)] : []),
    ],
  };
}

export const tree: NonNullable<EngineMock["tree"]> = (parent) => {
  if (!parent) {
    const names = catalog.databases.map((database) => ({ name: database.name, system: database.system }));
    return names.sort((a, b) => Number(a.system) - Number(b.system) || a.name.localeCompare(b.name)).map(({ name, system }) => databaseNode(name, system));
  }
  const [databasePart, folderPart] = parent.id.split("/");
  const database = databasePart.replace(/^db:/, "");
  const specs = catalog.tablesOf(database);
  const entries = specs.map((table) => ({ name: table.spec.name, rows: table.rows.length, kind: kindOf(engineOf({ database, name: table.spec.name })) }));
  if (!folderPart)
    return FOLDERS.flatMap(([id, label, kinds]) => {
      const count = entries.filter((entry) => kinds.includes(entry.kind)).length;
      if (count === 0) return [];
      const key = { id: `db:${database}/folder:${id}`, kind: "folder", object: null };
      return [
        {
          key,
          label,
          kind: "folder",
          icon: "layers",
          object: null,
          detail: String(count),
          badges: [],
          expandable: true,
          defaultView: null,
          actions: [],
        } satisfies ObjectNode,
      ];
    });
  const folder = FOLDERS.find(([id]) => `folder:${id}` === folderPart);
  return entries.filter((entry) => folder?.[2].includes(entry.kind)).map((entry) => objectNode(database, folder?.[0] ?? "tables", entry.name, entry.rows));
};

export const describe = (object: ObjectRef): ObjectDescription => {
  const description = catalog.describe(object);
  const engine = engineOf(description.object);
  const kind = kindOf(engine);
  const isTable = kind === "table";
  const editable = isTable && mutable(engine);
  const extra = description.extra.filter(([name]) => name !== "engine");
  return {
    ...description,
    object: { ...description.object, kind },
    columns: isTable && !editable ? description.columns.map((item) => ({ ...item, hints: { ...item.hints, readOnly: true } })) : description.columns,
    primaryKey: description.primaryKey && isTable ? { name: "PRIMARY KEY (not unique)", columns: description.primaryKey.columns } : null,
    indexes: description.indexes.map((index) => ({ ...index, unique: false, method: "sparse" })),
    extra: [
      ["engine", engine],
      ...extra,
      ...(isTable
        ? ([
            [
              "edits",
              editable
                ? "asynchronous mutations: ALTER TABLE … UPDATE and DELETE FROM, one after another, no transaction"
                : `read-only: ${READ_ONLY_REASON(description.object.name, engine)}`,
            ],
          ] as [string, string][])
        : []),
    ],
  };
};

export const browse: typeof catalog.browse = (request) => {
  const page = catalog.browse(request);
  const object = request.object;
  return { ...page, keyColumns: kindOf(engineOf(object)) === "table" ? page.keyColumns : [] };
};

// --- Mutations, the query log and running queries ---------------------------------------------------------------------------

interface Mutation {
  database: string;
  table: string;
  id: string;
  command: string;
  createdAt: Date;
}
const mutations: Mutation[] = [];
const log: { at: Date; type: string; ms: number; memory: number; readRows: number; writtenRows: number; query: string; code: string | null; id: string }[] = [];
const processes = [
  {
    id: "7f3c1e9a-aaaa-4b11-9c10-0000000000a1",
    user: "etl",
    elapsed: 412.6,
    readRows: 1_204_000_000,
    memory: "2.1 GiB",
    query: "INSERT INTO shop.daily_revenue_mv SELECT toDate(ordered_at), sum(total) FROM shop.orders GROUP BY 1",
  },
  {
    id: "7f3c1e9a-aaaa-4b11-9c10-0000000000b2",
    user: "grafana",
    elapsed: 3.2,
    readRows: 48_000_000,
    memory: "320 MiB",
    query: "SELECT toStartOfHour(ordered_at) AS h, count() FROM shop.orders GROUP BY h ORDER BY h",
  },
];

function note(sql: string, ms: number, error: string | null): void {
  log.unshift({
    at: new Date(),
    type: error ? "ExceptionWhileProcessing" : "QueryFinish",
    ms,
    memory: 1_048_576 * (1 + (sql.length % 40)),
    readRows: /select/i.test(sql) ? 4000 : 0,
    writtenRows: /insert/i.test(sql) ? 1 : 0,
    query: sql,
    code: error,
    id: `${Math.random().toString(16).slice(2, 10)}-0000-4000-8000-${Date.now().toString(16).padStart(12, "0")}`,
  });
  log.length = Math.min(log.length, 500);
}

const time = (date: Date): string => date.toISOString().replace("T", " ").slice(0, 19);

export const apply: NonNullable<EngineMock["apply"]> = (changes: RowChangeSet) => {
  const engine = engineOf(changes.object);
  if (kindOf(engine) !== "table" || !mutable(engine))
    throw mockError("ENGINE_ERROR", READ_ONLY_REASON(changes.object.name, engine), { engine: ENGINE, code: "READ_ONLY" });
  const result = catalog.apply(changes);
  for (const change of changes.changes) {
    if (change.op === "insert") continue;
    const where = change.key.map((field) => `${field.column} = ${JSON.stringify(field.value.v)}`).join(" AND ");
    mutations.unshift({
      database: changes.object.database ?? "shop",
      table: changes.object.name,
      id: String(mutations.length + 1).padStart(10, "0"),
      command:
        change.op === "update"
          ? `UPDATE ${change.values.map((field) => `${field.column} = ${JSON.stringify(field.value.v)}`).join(", ")} WHERE ${where}`
          : `DELETE WHERE ${where}`,
      createdAt: new Date(),
    });
  }
  return result;
};

type Args = Record<string, unknown>;

export const invoke: NonNullable<EngineMock["invoke"]> = (name, raw) => {
  const args = (raw ?? {}) as Args;
  switch (name) {
    case "clickhouse.parts": {
      const table = catalog.table({ database: String(args.database ?? "shop"), schema: null, name: String(args.table ?? "") });
      const rows = table.rows.length;
      const parts = Math.max(1, Math.ceil(rows / 1500));
      return {
        columns: ["part", "partition", "rows", "bytes_on_disk", "modification_time", "level", "part_type", "active"],
        rows: Array.from({ length: parts }, (_, index) => [
          `all_${index + 1}_${index + 1}_0`,
          "tuple()",
          String(Math.ceil(rows / parts)),
          String(Math.ceil((rows / parts) * 84)),
          time(new Date(Date.now() - index * 3_600_000)),
          "0",
          "Compact",
          "1",
        ]),
        note: null,
      };
    }
    case "clickhouse.mutations": {
      const rows = mutations.filter((item) => item.database === args.database && item.table === args.table);
      return {
        columns: ["mutation_id", "command", "create_time", "parts_to_do", "is_done", "latest_fail_reason", "latest_fail_time"],
        rows: rows.map((item) => [`mutation_${item.id}.txt`, item.command, time(item.createdAt), "0", "1", "", null]),
        note: null,
      };
    }
    case "clickhouse.processes":
      return {
        columns: ["query_id", "user", "elapsed_s", "read_rows", "memory", "query"],
        rows: processes.map((item) => [item.id, item.user, String(item.elapsed), String(item.readRows), item.memory, item.query]),
        note: null,
      };
    case "clickhouse.queryLog": {
      const failedOnly = args.failedOnly === true;
      const rows = log.filter((item) => !failedOnly || item.code !== null).slice(0, Number(args.limit ?? 100));
      return {
        columns: ["event_time", "type", "query_duration_ms", "memory", "read_rows", "written_rows", "user", "query", "exception_code", "query_id"],
        rows: rows.map((item) => [
          time(item.at),
          item.type,
          String(item.ms),
          human(item.memory),
          String(item.readRows),
          String(item.writtenRows),
          "studio",
          item.query,
          item.code,
          item.id,
        ]),
        note: rows.length === 0 ? "No finished queries in the last day yet. The server writes the log every few seconds." : null,
      };
    }
    case "clickhouse.kill": {
      if (typeof args.mutationId === "string") return { ok: true };
      const index = processes.findIndex((item) => item.id === args.queryId);
      if (index >= 0) processes.splice(index, 1);
      else if (typeof args.queryId !== "string") throw mockError("ERROR", "Say which query to stop (queryId).");
      return { ok: true };
    }
    default:
      return undefined;
  }
};

export const connectInfo = (): ConnectInfo => ({
  engine: ENGINE,
  server: { product: "ClickHouse", version: VERSION, label: `ClickHouse ${VERSION}`, flavor: null },
  capabilities: descriptor.capabilities,
  languages: descriptor.languages,
  connectionActions: [
    {
      id: "server",
      label: "Server: running queries and query log",
      icon: "server",
      target: { kind: "view", role: "server" },
      danger: false,
      separatorBefore: false,
    },
  ],
});

/** ClickHouse's own words on top of the shared classifier: SYSTEM and OPTIMIZE are administrative, DROP/DETACH PARTITION loses data, mutations write. */
export const classify = (text: string): StatementInfo[] =>
  classifySql(text).map((info) => {
    const sql = info.excerpt;
    if (/^(system|optimize)\b/i.test(sql)) return { ...info, category: "admin", reason: "A server or maintenance operation" };
    if (/^alter\b[\s\S]*\b(drop|detach)\b/i.test(sql)) return { ...info, category: "destructive", reason: "ALTER drops a column, index or partition" };
    if (/^alter\b[\s\S]*\b(update|delete)\b/i.test(sql)) return { ...info, category: "write", reason: "A mutation" };
    return info;
  });

export const run: NonNullable<EngineMock["run"]> = async (request: RunRequest, emit, context) => {
  const started = performance.now();
  try {
    const count = await runSql(catalog, dialect, request, emit, context);
    note(request.text.trim(), Math.round(performance.now() - started), null);
    return count;
  } catch (error) {
    note(request.text.trim(), Math.round(performance.now() - started), String((error as { vendorCode?: unknown }).vendorCode ?? "1"));
    throw error;
  }
};

export const completion = (database: string | null) => catalog.completion(database);
export const createDatabase = (name: string) => catalog.addDatabase(name);
export const touchRow = (table: string, key: unknown, remove: boolean, column?: string) => catalog.touch(table.replace(/^shop\./, ""), key, remove, column);
