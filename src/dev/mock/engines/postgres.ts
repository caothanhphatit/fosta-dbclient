/**
 * PostgreSQL 17 mock: the `shop` database with `public` and `sales` schemas. The tree is database → schema → folders
 * (tables, views, materialized views, functions, sequences, types) with an extensions folder per database, like the
 * backend. Tables carry typed values (arrays, jsonb, uuid, numeric, timestamptz) and `xmin`-style row versions, so the
 * conflict demo (`__fostaMock.touchRow`) works here too.
 */
import type { ConnectInfo, ForeignKeyInfo, ObjectDescription, ObjectNode, ObjectRef, TreeRequest } from "../../../core/contract";
import { builtin, column, MockCatalog, queryAction, v, viewAction, type CatalogStyle, type TableSpec } from "../catalog";
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mockError } from "../errors";
import { classifySql, runSql, type SqlDialectMock } from "../sqlRun";
import type { EngineMock } from "../types";
import { resultColumn, vectorOps } from "../vectors";

const base = DESCRIPTORS.postgres;
export const descriptor = {
  ...base,
  connection: {
    ...base.connection,
    tls: { modes: ["disable", "require", "verifyCa", "verifyFull"], ca: true, clientCert: true, serverName: true, pin: true },
  },
} satisfies typeof base;
export const order = ORDER.indexOf("postgres");

const ENGINE = "postgres";
const VERSION = "17.2";

const serverError = (sqlstate: string, message: string, extra: Record<string, unknown> = {}) =>
  mockError("ENGINE_ERROR", message, { engine: ENGINE, sqlstate, vendorCode: sqlstate, kind: "server", retryable: false, ...extra });

const errors = {
  syntax: (near: string) => serverError("42601", `syntax error at or near "${near.split(/\s+/)[0]}"`),
  noTable: (name: string) => serverError("42P01", `relation "${name}" does not exist`),
  noColumn: (name: string, where: string) => serverError("42703", `column "${name}" does not exist`, { detail: `In ${where}` }),
  cancelled: () =>
    mockError("CANCELLED", "canceling statement due to user request", { engine: ENGINE, sqlstate: "57014", kind: "cancelled", retryable: false }),
};

const style: CatalogStyle = {
  engine: ENGINE,
  quote: (name) => `"${name.replace(/"/g, '""')}"`,
  label: (table) => table.name,
  rawFilter: "sql",
  countUnit: "rows",
  databaseActions: [
    builtin("newQuery", "New query", "newQuery"),
    builtin("diagram", "ER diagram", "diagram"),
    builtin("copyName", "Copy name", "copyName"),
    builtin("refresh", "Refresh", "refresh", true),
  ],
  tableActions: (table) => [
    viewAction("design", "Design", "design"),
    builtin("copyName", "Copy name", "copyName"),
    queryAction("explain", "Explain a select", "EXPLAIN SELECT * FROM {qualified}"),
    ...(table.kind === "view"
      ? []
      : [queryAction("analyze", "Analyze", "ANALYZE {qualified}"), queryAction("truncate", "Truncate…", "TRUNCATE TABLE {qualified}", true)]),
  ],
  errors,
};

const NAMES = ["An Nguyen", "Binh Tran", "Chau Le", "Dung Pham", "Em Vo", "Giang Do", "Hoa Bui", "Khanh Dang"];
const COUNTRIES = ["VN", "US", "DE", "JP", "FR"];
const STATUS = ["new", "paid", "shipped", "cancelled"];
const pad = (n: number, width = 2) => String(n).padStart(width, "0");
const uuidOf = (n: number) => `6f9619ff-8b86-d011-b42d-${(0xc04fc964ff00 + n).toString(16).padStart(12, "0")}`;
const fk = (name: string, columns: string[], refSchema: string, refTable: string, refColumns: string[]): ForeignKeyInfo => ({
  name,
  columns,
  refDatabase: null,
  refSchema,
  refTable,
  refColumns,
  onUpdate: "NO ACTION",
  onDelete: "NO ACTION",
});

const tables: TableSpec[] = [
  {
    database: "shop",
    schema: "public",
    name: "customers",
    kind: "table",
    key: ["id"],
    versioned: true,
    columns: [
      column("id", "bigint", "int", { primary: true, identity: "GENERATED ALWAYS AS IDENTITY", default: "nextval('customers_id_seq'::regclass)" }),
      column("email", "character varying(200)", "text", { nullable: false }),
      column("full_name", "text", "text", { nullable: false }),
      column("country", "character(2)", "text", { nullable: false, default: "'VN'::bpchar" }),
      column("created_at", "timestamp with time zone", "timestampTz", { nullable: false, default: "now()" }),
      column("vip", "boolean", "bool", { nullable: false, default: "false" }),
      column("tags", "text[]", { kind: "array", of: { kind: "text" } }, {}),
      column("profile", "jsonb", "json", {}),
      column("external_id", "uuid", "uuid", {}),
      column("embedding", "vector(4)", "other", { hints: { vectorDims: 4 } }),
    ],
    rows: () =>
      Array.from({ length: 80 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(n),
          v.text(`customer${n}@example.com`),
          v.text(`${NAMES[n % NAMES.length]} ${n}`),
          v.text(COUNTRIES[n % COUNTRIES.length]),
          v.tstz(`2026-0${(n % 9) + 1}-${pad((n % 28) + 1)} 09:${pad(n % 60)}:00+07:00`),
          v.bool(n % 7 === 0),
          v.arr([v.text("web"), v.text(n % 2 ? "mobile" : "store")]),
          n % 5 === 0 ? v.null() : v.json({ plan: n % 3 === 0 ? "pro" : "free", seats: (n % 4) + 1 }),
          v.uuid(uuidOf(n)),
          v.other("vector", `[${[n % 10, (n * 3) % 11, (n * 7) % 13, (n * 9) % 17].map((x) => ((x + 1) / 10).toFixed(1)).join(",")}]`),
        ];
      }),
    indexes: [
      {
        name: "customers_email_key",
        columns: ["email"],
        unique: true,
        primary: false,
        method: "btree",
        definition: "CREATE UNIQUE INDEX customers_email_key ON public.customers USING btree (email)",
      },
      {
        name: "customers_embedding_idx",
        columns: ["embedding"],
        unique: false,
        primary: false,
        method: "hnsw",
        definition: "CREATE INDEX customers_embedding_idx ON public.customers USING hnsw (embedding vector_cosine_ops)",
      },
    ],
    extra: [["Relation kind", "table"]],
  },
  {
    database: "shop",
    schema: "public",
    name: "orders",
    kind: "table",
    key: ["id"],
    versioned: true,
    columns: [
      column("id", "bigint", "int", { primary: true }),
      column("customer_id", "bigint", "int", { nullable: false }),
      column("status", "order_status", "text", { nullable: false, default: "'new'::order_status" }),
      column("total", "numeric(12,2)", "decimal", { nullable: false }),
      column("ordered_at", "timestamp without time zone", "timestamp", { nullable: false }),
      column("note", "text", "text", {}),
    ],
    rows: () =>
      Array.from({ length: 600 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(n),
          v.int(((n * 7) % 80) + 1),
          v.text(STATUS[n % STATUS.length]),
          v.dec((15 + ((n * 37) % 900) + 0.5).toFixed(2)),
          v.ts(`2026-${pad((n % 10) + 1)}-${pad((n % 28) + 1)} ${pad(n % 24)}:${pad(n % 60)}:00`),
          n % 6 === 0 ? v.text("gift wrap") : v.null(),
        ];
      }),
    foreignKeys: [fk("orders_customer_id_fkey", ["customer_id"], "public", "customers", ["id"])],
    extra: [["Relation kind", "table"]],
  },
  {
    database: "shop",
    schema: "public",
    name: "customer_orders",
    kind: "view",
    key: [],
    columns: [column("full_name", "text", "text", {}), column("orders", "bigint", "int", {}), column("spent", "numeric", "decimal", {})],
    rows: () =>
      Array.from({ length: 40 }, (_, index) => [
        v.text(`${NAMES[index % NAMES.length]} ${index + 1}`),
        v.int((index % 9) + 1),
        v.dec(((index + 1) * 41.25).toFixed(2)),
      ]),
    ddl: "CREATE VIEW public.customer_orders AS\n SELECT c.full_name, count(o.id) AS orders, sum(o.total) AS spent\n FROM customers c JOIN orders o ON o.customer_id = c.id\n GROUP BY c.full_name;",
  },
  {
    database: "shop",
    schema: "public",
    name: "daily_sales",
    kind: "view",
    key: [],
    columns: [column("day", "date", "date", {}), column("revenue", "numeric", "decimal", {})],
    rows: () => Array.from({ length: 30 }, (_, index) => [v.date(`2026-10-${pad(index + 1)}`), v.dec((1200 + index * 83.4).toFixed(2))]),
    ddl: "CREATE MATERIALIZED VIEW public.daily_sales AS\n SELECT ordered_at::date AS day, sum(total) AS revenue\n FROM orders GROUP BY 1;",
  },
  {
    database: "shop",
    schema: "sales",
    name: "invoices",
    kind: "table",
    key: ["number"],
    versioned: true,
    columns: [
      column("number", "text", "text", { primary: true }),
      column("order_id", "bigint", "int", { nullable: false }),
      column("issued", "date", "date", { nullable: false }),
      column("amount", "numeric(12,2)", "decimal", { nullable: false }),
      column("paid", "boolean", "bool", { nullable: false, default: "false" }),
    ],
    rows: () =>
      Array.from({ length: 120 }, (_, index) => [
        v.text(`INV-${pad(index + 1, 5)}`),
        v.int(index * 5 + 1),
        v.date(`2026-${pad((index % 10) + 1)}-${pad((index % 28) + 1)}`),
        v.dec((20 + index * 3.1).toFixed(2)),
        v.bool(index % 3 !== 0),
      ]),
    foreignKeys: [fk("invoices_order_id_fkey", ["order_id"], "public", "orders", ["id"])],
    extra: [["Relation kind", "table"]],
  },
];

const MATERIALIZED = new Set(["daily_sales"]);

interface Other {
  schema: string | null;
  name: string;
  kind: "function" | "sequence" | "type" | "extension";
  detail: string;
  ddl: string;
  extra: [string, string][];
  icon: string;
}

const others: Other[] = [
  {
    schema: "public",
    name: "order_total",
    kind: "function",
    detail: "(order_id bigint)",
    icon: "terminal",
    extra: [["signature", "(order_id bigint) RETURNS numeric"]],
    ddl: "CREATE OR REPLACE FUNCTION public.order_total(order_id bigint)\n RETURNS numeric\n LANGUAGE sql\nAS $function$ SELECT total FROM orders WHERE id = order_id $function$;",
  },
  {
    schema: "public",
    name: "customers_id_seq",
    kind: "sequence",
    detail: "bigint",
    icon: "engine:postgres/sequence",
    extra: [
      ["start", "1"],
      ["increment", "1"],
      ["last value", "80"],
    ],
    ddl: "CREATE SEQUENCE public.customers_id_seq\n AS bigint START WITH 1 INCREMENT BY 1;",
  },
  {
    schema: "public",
    name: "order_status",
    kind: "type",
    detail: "enum",
    icon: "engine:postgres/enum",
    extra: [
      ["kind", "enum"],
      ["labels", "new, paid, shipped, cancelled"],
    ],
    ddl: "CREATE TYPE public.order_status AS ENUM ('new', 'paid', 'shipped', 'cancelled');",
  },
  {
    schema: null,
    name: "pgcrypto",
    kind: "extension",
    detail: "1.3",
    icon: "engine:postgres/extension",
    extra: [
      ["version", "1.3"],
      ["schema", "public"],
    ],
    ddl: "CREATE EXTENSION pgcrypto WITH SCHEMA public VERSION '1.3';",
  },
];

const catalog = new MockCatalog(style, [
  { name: "shop", tables },
  { name: "postgres", system: true, tables: [] },
]);

const dialect: SqlDialectMock = {
  split: {},
  errors,
  version: `PostgreSQL ${VERSION} (Debian ${VERSION}-1.pgdg120+1) on x86_64-pc-linux-gnu, compiled by gcc, 64-bit`,
};

const folderLabels: [string, string[]][] = [
  ["Tables", ["table"]],
  ["Views", ["view"]],
  ["Materialized Views", ["materializedView"]],
  ["Functions", ["function"]],
  ["Sequences", ["sequence"]],
  ["Types", ["type"]],
];

const copyName = builtin("copyName", "Copy name", "copyName");

function containerNode(id: string, kind: "schema" | "folder", label: string, database: string, schema: string | null): ObjectNode {
  const object: ObjectRef | null = kind === "schema" ? { database, schema, name: label, kind: "schema" } : null;
  return {
    key: { id, kind, object },
    label,
    kind,
    icon: kind === "schema" ? "database" : "layers",
    object,
    detail: null,
    badges: [],
    expandable: true,
    defaultView: null,
    actions: kind === "schema" ? [builtin("newQuery", "New query", "newQuery"), copyName, builtin("refresh", "Refresh", "refresh", true)] : [],
  };
}

function otherNode(prefix: string, database: string, item: Other): ObjectNode {
  const object: ObjectRef = { database, schema: item.schema, name: item.name, kind: item.kind };
  return {
    key: { id: `${prefix}/${item.kind}:${item.name}`, kind: item.kind, object },
    label: item.name,
    kind: item.kind,
    icon: item.icon,
    object,
    detail: item.detail,
    badges: [],
    expandable: false,
    defaultView: "definition",
    actions: [copyName],
  };
}

const kindOf = (node: ObjectNode) => (node.object && MATERIALIZED.has(node.object.name) ? "materializedView" : node.kind);

function tableNodes(prefix: string, database: string, schema: string, kinds: string[]): ObjectNode[] {
  return catalog
    .tree({ id: `db:${database}`, kind: "database", object: { database, schema: null, name: database, kind: "database" } })
    .filter((node) => node.object?.schema === schema && kinds.includes(kindOf(node)))
    .map((node) => {
      const kind = kindOf(node);
      const object = { ...node.object!, kind };
      return { ...node, kind, object, key: { id: `${prefix}/${kind}:${node.label}`, kind, object }, icon: node.icon, defaultView: "data" };
    });
}

export const tree = (parent: TreeRequest["parent"]): ObjectNode[] => {
  if (!parent) return catalog.tree(null);
  const [db, schemaPart] = parent.id.split("/");
  const database = db.slice(3);
  if (parent.kind === "database") {
    const schemas = [
      ...new Set([...tables.filter((t) => t.database === database).map((t) => t.schema ?? "public"), ...others.flatMap((o) => (o.schema ? [o.schema] : []))]),
    ].sort();
    const nodes = database === "shop" ? schemas.map((name) => containerNode(`${db}/schema:${name}`, "schema", name, database, name)) : [];
    return database === "shop" ? [...nodes, containerNode(`${db}/folder:Extensions`, "folder", "Extensions", database, null)] : nodes;
  }
  if (parent.kind === "schema") {
    const schema = schemaPart.slice(7);
    return folderLabels
      .filter(([, kinds]) => tableNodes("", database, schema, kinds).length > 0 || others.some((o) => o.schema === schema && kinds.includes(o.kind)))
      .map(([label]) => containerNode(`${db}/${schemaPart}/folder:${label}`, "folder", label, database, schema));
  }
  if (parent.kind === "folder") {
    const label = parent.id.split("/").pop()!.slice(7);
    if (label === "Extensions") return others.filter((o) => o.kind === "extension").map((item) => otherNode(parent.id, database, item));
    const kinds = folderLabels.find(([name]) => name === label)?.[1] ?? [];
    const schema = schemaPart.slice(7);
    return [
      ...tableNodes(parent.id, database, schema, kinds),
      ...others.filter((o) => o.schema === schema && kinds.includes(o.kind)).map((item) => otherNode(parent.id, database, item)),
    ];
  }
  return [];
};

export const describe = (object: ObjectRef): ObjectDescription => {
  const item = others.find((other) => other.kind === object.kind && other.name === object.name);
  if (!item) return catalog.describe(object);
  return {
    object,
    comment: null,
    columns: [],
    primaryKey: null,
    uniqueKeys: [],
    foreignKeys: [],
    referencedBy: [],
    indexes: [],
    rowEstimate: null,
    ddl: item.ddl,
    extra: item.extra,
  };
};

export const connectInfo = (): ConnectInfo => ({
  engine: ENGINE,
  server: { product: "PostgreSQL", version: `${VERSION} (Debian ${VERSION}-1.pgdg120+1)`, label: `PostgreSQL ${VERSION}`, flavor: null },
  capabilities: descriptor.capabilities,
  languages: descriptor.languages,
  connectionActions: [builtin("createDatabase", "New database…", "createDatabase"), builtin("refresh", "Refresh", "refresh")],
});

export const browse: typeof catalog.browse = (request) => catalog.browse(request);
export const apply: typeof catalog.apply = (changes) => catalog.apply(changes);
/** `pg_sleep(n)` is the mock's `sleep(n)`, so Stop can be tried. */
export const run: NonNullable<EngineMock["run"]> = (request, emit, context) =>
  runSql(catalog, dialect, { ...request, text: request.text.replace(/pg_sleep\(/gi, "sleep(") }, emit, context);
export const classify = (text: string) => classifySql(text, dialect.split);

/** pgvector on `public.customers.embedding`. */
const vectors = vectorOps({
  catalog,
  object: { database: "shop", schema: "public", name: "customers", kind: "table" },
  column: "embedding",
  dims: 4,
  typeName: "vector(4)",
  index: { name: "customers_embedding_idx", kind: "hnsw", metric: "cosine", m: null, params: "defaults" },
  metrics: ["cosine", "euclidean", "dot"],
  quote: style.quote,
  distance: (vector) => `"embedding" <=> ${vector}`,
  placeholder: "$1::vector",
  literal: (text) => `'${text}'::vector`,
  plan: {
    columns: [resultColumn("QUERY PLAN", "text", "text")],
    rows: [
      [{ t: "text", v: "Limit  (cost=12.10..13.40 rows=10 width=96)" }],
      [{ t: "text", v: "  ->  Index Scan using customers_embedding_idx on customers  (cost=12.10..22.50 rows=80 width=96)" }],
    ],
  },
});

/** `postgres.explain`: a plan with one node per table in the statement; `vector.*` on the pgvector column. */
export const invoke = (name: string, args: unknown): unknown => {
  if (name.startsWith("vector.")) return vectors[name]?.((args ?? {}) as Record<string, unknown>);
  if (name !== "postgres.explain") return undefined;
  const sql = String((args as { sql?: string } | null)?.sql ?? "");
  const table = /\bfrom\s+"?([\w.]+)"?/i.exec(sql)?.[1] ?? "unknown";
  return {
    plan: [{ Plan: { "Node Type": "Seq Scan", "Relation Name": table, "Startup Cost": 0, "Total Cost": 12.4, "Plan Rows": 600, "Plan Width": 64 } }],
    analyze: false,
    sql,
  };
};
export const schemaGraph = (database: string) => catalog.schemaGraph(database);
export const completion = (database: string | null) => catalog.completion(database);
export const createDatabase = (name: string) => catalog.addDatabase(name);
export const touchRow = (table: string, key: unknown, remove: boolean, column?: string) => catalog.touch(table, key, remove, column);
