/**
 * SQLite 3.50 mock: a `main` database (contacts, a table without a primary key that is edited by rowid, a view, an
 * index and a trigger) and an attached `audit` database. There are no row versions: edits are checked by the original
 * values, so the conflict demo (`__fostaMock.touchRow`) still works.
 */
import type { ConnectInfo, EngineDescriptor, ObjectDescription, ObjectNode, ObjectRef, TreeRequest } from "../../../core/contract";
import { builtin, column, MockCatalog, queryAction, v, viewAction, type CatalogStyle, type TableSpec } from "../catalog";
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mockError } from "../errors";
import { classifySql, runSql, type SqlDialectMock } from "../sqlRun";
import type { EngineMock } from "../types";

const base: EngineDescriptor = DESCRIPTORS.sqlite;
export const descriptor: EngineDescriptor = {
  ...base,
  connection: {
    ...base.connection,
    fields: [
      ...base.connection.fields.filter((field) => field.key !== "options.busyTimeout"),
      {
        key: "options.attach",
        kind: { type: "textarea" },
        label: "Attached databases",
        placeholder: "audit=C:\\data\\audit.db",
        hint: "One alias=path per line; they show up next to main in the tree and can be used in queries as alias.table.",
        section: "advanced",
        default: null,
        required: false,
      },
      ...base.connection.fields.filter((field) => field.key === "options.busyTimeout"),
    ],
  },
};
export const order = ORDER.indexOf("sqlite");

const ENGINE = "sqlite";
const VERSION = "3.50.4";

const serverError = (code: number, message: string, extra: Record<string, unknown> = {}) =>
  mockError("ENGINE_ERROR", message, { engine: ENGINE, vendorCode: String(code), kind: "server", retryable: false, ...extra });

const errors = {
  syntax: (near: string) => serverError(1, `near "${near.split(/\s+/)[0]}": syntax error`),
  noTable: (name: string) => serverError(1, `no such table: ${name}`),
  noColumn: (name: string) => serverError(1, `no such column: ${name}`),
  cancelled: () => mockError("CANCELLED", "interrupted", { engine: ENGINE, vendorCode: "9", kind: "cancelled", retryable: false }),
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
    queryAction("explain", "Explain a select", "EXPLAIN QUERY PLAN SELECT * FROM {qualified}"),
    ...(table.kind === "view"
      ? []
      : [queryAction("analyze", "Analyze", "ANALYZE {qualified}"), queryAction("clear", "Delete all rows…", "DELETE FROM {qualified}", true)]),
  ],
  errors,
};

const NAMES = ["Alice Martin", "Bao Chen", "Carla Ruiz", "Dmitri Volkov", "Emi Sato", "Farah Haddad"];
const pad = (n: number, width = 2) => String(n).padStart(width, "0");

const tables: TableSpec[] = [
  {
    database: "main",
    name: "contacts",
    kind: "table",
    key: ["id"],
    columns: [
      column("id", "INTEGER", "int", { primary: true, identity: "AUTOINCREMENT" }),
      column("name", "TEXT", "text", { nullable: false }),
      column("email", "VARCHAR(120)", "text", {}),
      column("born", "DATE", "date", {}),
      column("active", "BOOLEAN", "bool", { nullable: false, default: "1" }),
      column("score", "REAL", "float", {}),
      column("meta", "JSON", "json", {}),
    ],
    rows: () =>
      Array.from({ length: 120 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(n),
          v.text(`${NAMES[n % NAMES.length]} ${n}`),
          n % 8 === 0 ? v.null() : v.text(`contact${n}@example.org`),
          v.date(`19${70 + (n % 30)}-${pad((n % 12) + 1)}-${pad((n % 28) + 1)}`),
          v.bool(n % 5 !== 0),
          v.float(Math.round(((n * 37) % 100) * 10) / 10),
          v.json({ tags: n % 2 ? ["a"] : ["a", "b"] }),
        ];
      }),
    indexes: [
      {
        name: "contacts_email_idx",
        columns: ["email"],
        unique: false,
        primary: false,
        method: null,
        definition: "CREATE INDEX contacts_email_idx ON contacts (email)",
      },
    ],
    extra: [
      ["withoutRowid", "false"],
      ["strict", "false"],
    ],
  },
  {
    database: "main",
    name: "scratch",
    kind: "table",
    key: ["_rowid_"],
    columns: [
      column("_rowid_", "INTEGER", "int", { primary: true, identity: "ROWID" }),
      column("label", "TEXT", "text", {}),
      column("qty", "INTEGER", "int", {}),
    ],
    rows: () => Array.from({ length: 25 }, (_, index) => [v.int(index + 1), v.text(`item ${index + 1}`), v.int((index * 7) % 40)]),
    extra: [
      ["withoutRowid", "false"],
      ["strict", "false"],
    ],
  },
  {
    database: "main",
    name: "active_contacts",
    kind: "view",
    key: [],
    columns: [column("id", "INTEGER", "int", {}), column("name", "TEXT", "text", {})],
    rows: () => Array.from({ length: 40 }, (_, index) => [v.int(index + 1), v.text(`${NAMES[index % NAMES.length]} ${index + 1}`)]),
    ddl: "CREATE VIEW active_contacts AS SELECT id, name FROM contacts WHERE active = 1;",
  },
  {
    database: "audit",
    name: "log",
    kind: "table",
    key: ["id"],
    columns: [column("id", "INTEGER", "int", { primary: true }), column("what", "TEXT", "text", {}), column("at", "DATETIME", "timestamp", {})],
    rows: () =>
      Array.from({ length: 30 }, (_, index) => [
        v.int(index + 1),
        v.text(index % 2 ? "login" : "update"),
        v.ts(`2026-11-0${(index % 8) + 1} 10:${pad(index)}:00`),
      ]),
    extra: [
      ["withoutRowid", "false"],
      ["strict", "false"],
    ],
  },
];

interface Definition {
  database: string;
  name: string;
  kind: "index" | "trigger";
  detail: string;
  ddl: string;
}

const definitions: Definition[] = [
  { database: "main", name: "contacts_email_idx", kind: "index", detail: "on contacts", ddl: "CREATE INDEX contacts_email_idx ON contacts (email);" },
  {
    database: "main",
    name: "contacts_audit",
    kind: "trigger",
    detail: "on contacts",
    ddl: "CREATE TRIGGER contacts_audit AFTER UPDATE ON contacts\nBEGIN\n  INSERT INTO audit.log (what) VALUES ('update');\nEND;",
  },
];

const catalog = new MockCatalog(style, [
  { name: "main", tables: tables.filter((table) => table.database === "main") },
  { name: "audit", tables: tables.filter((table) => table.database === "audit") },
]);

const dialect: SqlDialectMock = { split: {}, errors, version: `SQLite ${VERSION}` };

const copyName = builtin("copyName", "Copy name", "copyName");

function folderNode(database: string, label: string): ObjectNode {
  return {
    key: { id: `db:${database}/folder:${label}`, kind: "folder", object: null },
    label,
    kind: "folder",
    icon: "layers",
    object: null,
    detail: null,
    badges: [],
    expandable: true,
    defaultView: null,
    actions: [],
  };
}

function definitionNode(prefix: string, item: Definition): ObjectNode {
  const object: ObjectRef = { database: item.database, schema: null, name: item.name, kind: item.kind };
  return {
    key: { id: `${prefix}/${item.kind}:${item.name}`, kind: item.kind, object },
    label: item.name,
    kind: item.kind,
    icon: item.kind === "trigger" ? "engine:sqlite/trigger" : "key",
    object,
    detail: item.detail,
    badges: [],
    expandable: false,
    defaultView: "definition",
    actions: [copyName],
  };
}

const folders: [string, string][] = [
  ["Tables", "table"],
  ["Views", "view"],
  ["Indexes", "index"],
  ["Triggers", "trigger"],
];

export const tree = (parent: TreeRequest["parent"]): ObjectNode[] => {
  if (!parent) return catalog.tree(null);
  const [db] = parent.id.split("/");
  const database = db.slice(3);
  if (parent.kind === "database")
    return folders
      .filter(
        ([, kind]) =>
          tables.some((table) => table.database === database && table.kind === kind) ||
          definitions.some((item) => item.database === database && item.kind === kind),
      )
      .map(([label]) => folderNode(database, label));
  if (parent.kind !== "folder") return [];
  const label = parent.id.split("/").pop()!.slice(7);
  const kind = folders.find(([name]) => name === label)?.[1];
  const items = definitions.filter((item) => item.database === database && item.kind === kind).map((item) => definitionNode(parent.id, item));
  const relations = catalog
    .tree({ id: db, kind: "database", object: { database, schema: null, name: database, kind: "database" } })
    .filter((node) => node.kind === kind)
    .map((node) => ({ ...node, key: { ...node.key, id: `${parent.id}/${node.kind}:${node.label}` } }));
  return [...relations, ...items];
};

export const describe = (object: ObjectRef): ObjectDescription => {
  const item = definitions.find((definition) => definition.kind === object.kind && definition.name === object.name);
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
    extra: [["on", item.detail.replace("on ", "")]],
  };
};

export const connectInfo = (): ConnectInfo => ({
  engine: ENGINE,
  server: { product: "SQLite", version: VERSION, label: `SQLite ${VERSION}`, flavor: null },
  capabilities: descriptor.capabilities,
  languages: descriptor.languages,
  connectionActions: [builtin("refresh", "Refresh", "refresh")],
});

export const browse: typeof catalog.browse = (request) => catalog.browse(request);
export const apply: typeof catalog.apply = (changes) => catalog.apply(changes);
export const run: NonNullable<EngineMock["run"]> = (request, emit, context) => runSql(catalog, dialect, request, emit, context);
export const classify = (text: string) => classifySql(text, dialect.split);

/** `sqlite.explain`: `EXPLAIN QUERY PLAN` rows for the first table of the statement. */
export const invoke = (name: string, args: unknown): unknown => {
  if (name !== "sqlite.explain") return undefined;
  const sql = String((args as { sql?: string } | null)?.sql ?? "");
  const table = /\bfrom\s+"?([\w.]+)"?/i.exec(sql)?.[1] ?? "t";
  return { plan: [{ id: 2, parent: 0, detail: `SCAN ${table}` }], sql };
};
export const schemaGraph = (database: string) => catalog.schemaGraph(database);
export const completion = (database: string | null) => catalog.completion(database);
export const touchRow = (table: string, key: unknown, remove: boolean, column?: string) => catalog.touch(table, key, remove, column);
