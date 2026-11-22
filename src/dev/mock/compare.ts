/**
 * Schema compare in the browser mock: two PostgreSQL snapshots of a small shop schema (a newer "dev" copy on Local
 * PostgreSQL `shop.public`, an older "staging" copy everywhere else), a simplified diff and a sync script in the shape the
 * backend returns (`compare/diff.rs`, `compare/plan.rs`, `engines/postgres/schema_sync.rs`). Enough for `npm run dev`
 * to show every part of the compare tab; the real rules live in Rust.
 */
import type {
  Change,
  ColumnDef,
  CompareOptions,
  DiffItem,
  ForeignKeyDef,
  IndexDef,
  Risk,
  SchemaDiff,
  SchemaSnapshot,
  ScriptWarning,
  SkippedObject,
  SnapshotScope,
  SyncScript,
  TableDef,
  ViewDef,
} from "../../core/contract";
import { mockError } from "./errors";

// --- Snapshots -------------------------------------------------------------------------------------------------------------

function column(name: string, typeName: string, nullable: boolean, extra: Partial<ColumnDef> = {}): ColumnDef {
  return { name, typeName, nullable, default: null, defaultName: null, identity: null, generated: null, collation: null, comment: null, ...extra };
}

const identity = (): Partial<ColumnDef> => ({ identity: { kind: "always", start: 1, increment: 1 } });

function table(name: string, columns: ColumnDef[], extra: Partial<TableDef> = {}): TableDef {
  return {
    name,
    columns,
    primaryKey: { name: `${name}_pkey`, columns: [columns[0].name], systemNamed: false },
    uniques: [],
    checks: [],
    indexes: [],
    foreignKeys: [],
    comment: null,
    options: [],
    nativeDdl: null,
    ...extra,
  };
}

function index(name: string, columns: string[], extra: Partial<IndexDef> = {}): IndexDef {
  return {
    name,
    parts: columns.map((entry) => ({ column: entry.replace(/ DESC$/, ""), expression: null, descending: entry.endsWith(" DESC") })),
    unique: false,
    method: "btree",
    predicate: null,
    include: [],
    systemNamed: false,
    ...extra,
  };
}

function fk(name: string, columns: string[], refTable: string, refColumns: string[], onDelete: string | null): ForeignKeyDef {
  return { name, columns, refSchema: null, refTable, refColumns, onUpdate: null, onDelete, systemNamed: false };
}

function view(name: string, definition: string, dependsOn: string[]): ViewDef {
  return { name, definition, materialized: false, dependsOn, comment: null, options: [], unreadable: false };
}

const ACTIVE_PRODUCTS = "SELECT p.id, p.sku, p.name, p.price FROM products p WHERE p.is_active AND p.stock > 0";

function devTables(): TableDef[] {
  return [
    table(
      "customers",
      [
        column("id", "integer", false, identity()),
        column("email", "character varying(255)", false),
        column("full_name", "character varying(120)", false),
        column("phone", "character varying(32)", true),
        column("created_at", "timestamp with time zone", false, { default: "now()" }),
      ],
      { uniques: [{ name: "customers_email_key", columns: ["email"], systemNamed: false }], comment: "People who place orders" },
    ),
    table(
      "products",
      [
        column("id", "integer", false, identity()),
        column("sku", "character varying(32)", false),
        column("name", "character varying(200)", false),
        column("price", "numeric(10,2)", false),
        column("stock", "integer", false, { default: "0" }),
        column("is_active", "boolean", false, { default: "true" }),
      ],
      {
        uniques: [{ name: "products_sku_key", columns: ["sku"], systemNamed: false }],
        checks: [{ name: "products_price_check", expression: "CHECK (price >= 0::numeric)", systemNamed: false }],
        indexes: [index("products_name_idx", ["name"])],
      },
    ),
    table(
      "orders",
      [
        column("id", "bigint", false, identity()),
        column("customer_id", "integer", false),
        column("status", "character varying(20)", false, { default: "'pending'::character varying" }),
        column("total", "numeric(12,2)", false, { default: "0" }),
        column("placed_at", "timestamp with time zone", false, { default: "now()" }),
        column("shipped_at", "timestamp with time zone", true),
      ],
      {
        checks: [
          {
            name: "orders_status_check",
            expression: "CHECK (status::text = ANY (ARRAY['pending', 'paid', 'shipped', 'cancelled']))",
            systemNamed: false,
          },
        ],
        indexes: [
          index("orders_customer_id_idx", ["customer_id"]),
          index("orders_placed_at_idx", ["placed_at DESC"], { predicate: "status::text <> 'cancelled'::text" }),
        ],
        foreignKeys: [fk("orders_customer_id_fkey", ["customer_id"], "customers", ["id"], "RESTRICT")],
      },
    ),
    table(
      "order_items",
      [
        column("order_id", "bigint", false),
        column("product_id", "integer", false),
        column("quantity", "integer", false),
        column("unit_price", "numeric(10,2)", false),
      ],
      {
        primaryKey: { name: "order_items_pkey", columns: ["order_id", "product_id"], systemNamed: false },
        foreignKeys: [
          fk("order_items_order_id_fkey", ["order_id"], "orders", ["id"], "CASCADE"),
          fk("order_items_product_id_fkey", ["product_id"], "products", ["id"], null),
        ],
      },
    ),
    table("coupons", [column("code", "character varying(32)", false), column("percent_off", "integer", false), column("expires_at", "date", true)], {
      checks: [{ name: "coupons_percent_off_check", expression: "CHECK (percent_off >= 1 AND percent_off <= 90)", systemNamed: false }],
    }),
    table("flyway_schema_history", [column("installed_rank", "integer", false), column("version", "character varying(50)", true)]),
  ];
}

function stagingTables(): TableDef[] {
  return [
    table(
      "customers",
      [
        column("id", "integer", false, identity()),
        column("email", "character varying(255)", false),
        column("full_name", "character varying(100)", false),
        column("created_at", "timestamp with time zone", false, { default: "now()" }),
      ],
      { uniques: [{ name: "customers_email_key", columns: ["email"], systemNamed: false }], comment: "Customers" },
    ),
    table(
      "products",
      [
        column("id", "integer", false, identity()),
        column("sku", "character varying(32)", false),
        column("name", "character varying(255)", false),
        column("price", "numeric(10,2)", false),
        column("stock", "integer", false, { default: "0" }),
        column("is_active", "boolean", false, { default: "true" }),
        column("legacy_code", "character varying(20)", true),
      ],
      { uniques: [{ name: "products_sku_key", columns: ["sku"], systemNamed: false }], indexes: [index("products_name_idx", ["name"])] },
    ),
    table(
      "orders",
      [
        column("id", "bigint", false, identity()),
        column("customer_id", "integer", false),
        column("status", "character varying(20)", false, { default: "'new'::character varying" }),
        column("total", "numeric(12,2)", true, { default: "0" }),
        column("placed_at", "timestamp with time zone", false, { default: "now()" }),
        column("shipped_on", "timestamp with time zone", true),
      ],
      {
        checks: [
          {
            name: "orders_status_check",
            expression: "CHECK (status::text = ANY (ARRAY['new', 'paid', 'shipped', 'cancelled']))",
            systemNamed: false,
          },
        ],
        indexes: [index("orders_customer_id_idx", ["customer_id"])],
        foreignKeys: [fk("orders_customer_id_fkey", ["customer_id"], "customers", ["id"], "CASCADE")],
      },
    ),
    table(
      "order_items",
      [
        column("order_id", "bigint", false),
        column("product_id", "integer", false),
        column("quantity", "integer", false),
        column("unit_price", "numeric(10,2)", false),
      ],
      {
        primaryKey: { name: "order_items_pkey", columns: ["order_id", "product_id"], systemNamed: false },
        foreignKeys: [
          fk("order_items_order_id_fkey", ["order_id"], "orders", ["id"], "CASCADE"),
          fk("order_items_product_id_fkey", ["product_id"], "products", ["id"], null),
        ],
      },
    ),
    table("audit_log", [
      column("id", "bigint", false, { identity: { kind: "serial", start: null, increment: null } }),
      column("table_name", "text", false),
      column("changed_at", "timestamp with time zone", false, { default: "now()" }),
      column("payload", "jsonb", true),
    ]),
  ];
}

const SKIPPED: SkippedObject[] = [
  { kind: "function", name: "place_order(integer, jsonb)", reason: "Routines are not compared in this version" },
  { kind: "trigger", name: "orders_touch_updated", reason: "Triggers are not compared in this version" },
  { kind: "type", name: "order_status", reason: "Types are not compared in this version" },
];

/** `shop.public` on Local PostgreSQL is the newer copy; any other scope is the older one. */
export function compareSnapshot(connectionId: string, scope: SnapshotScope, engine: string): SchemaSnapshot {
  if (engine !== "postgres") throw mockError("UNSUPPORTED_CAPABILITY", "The mock compares PostgreSQL schemas only.", { engine });
  if (!scope.database.trim()) throw mockError("ERROR", "Choose a database to compare");
  const dev = connectionId === "c-local-pg" && (scope.schema ?? "public") === "public";
  return {
    engine: "postgres",
    serverVersion: dev ? "17.2" : "16.4",
    scope,
    caseInsensitiveNames: false,
    tables: dev ? devTables() : stagingTables(),
    views: dev
      ? [
          view(
            "v_order_totals",
            "SELECT o.id, o.customer_id, sum(i.quantity::numeric * i.unit_price) AS total FROM orders o JOIN order_items i ON i.order_id = o.id GROUP BY o.id, o.customer_id",
            ["orders", "order_items"],
          ),
          view("v_active_products", ACTIVE_PRODUCTS, ["products"]),
        ]
      : [
          view(
            "v_order_totals",
            "SELECT o.id, sum(i.quantity::numeric * i.unit_price) AS total FROM orders o JOIN order_items i ON i.order_id = o.id GROUP BY o.id",
            ["orders", "order_items"],
          ),
          view("v_active_products", ACTIVE_PRODUCTS, ["products"]),
          view("v_legacy_stock", "SELECT p.legacy_code, p.stock FROM products p WHERE p.legacy_code IS NOT NULL", ["products"]),
        ],
    skipped: SKIPPED,
    readAt: new Date().toISOString(),
  };
}

// --- Diff ------------------------------------------------------------------------------------------------------------------

const quote = (name: string) => (/^[a-z_][a-z0-9_]*$/.test(name) ? name : `"${name.replace(/"/g, '""')}"`);
const qualified = (schema: string | null, name: string) => (schema ? `${quote(schema)}.${quote(name)}` : quote(name));
const globToRegex = (glob: string) =>
  new RegExp(
    `^${glob
      .replace(/[.+^${}()|[\]\\]/g, "\\$&")
      .replace(/\*/g, ".*")
      .replace(/\?/g, ".")}$`,
    "i",
  );
const excluded = (options: CompareOptions, name: string) => options.exclude.some((pattern) => globToRegex(pattern).test(name));
const maxRisk = (risks: Risk[]): Risk => (risks.includes("dataLoss") ? "dataLoss" : risks.includes("review") ? "review" : "none");

function columnSql(entry: ColumnDef, options: CompareOptions): string {
  let text = `${quote(entry.name)} ${entry.typeName}`;
  if (entry.identity?.kind === "always") text += " GENERATED ALWAYS AS IDENTITY";
  else if (entry.identity?.kind === "byDefault") text += " GENERATED BY DEFAULT AS IDENTITY";
  if (!entry.nullable) text += " NOT NULL";
  if (entry.default !== null && !options.ignoreDefaults && entry.identity?.kind !== "serial") text += ` DEFAULT ${entry.default}`;
  return text;
}

function indexSql(schema: string, tableName: string, entry: IndexDef): string {
  const parts = entry.parts.map((part) => `${part.column ?? part.expression}${part.descending ? " DESC" : ""}`).join(", ");
  const where = entry.predicate ? ` WHERE (${entry.predicate})` : "";
  return `CREATE ${entry.unique ? "UNIQUE " : ""}INDEX ${quote(entry.name)} ON ${qualified(schema, tableName)} USING ${entry.method ?? "btree"} (${parts})${where};`;
}

function fkSql(schema: string, tableName: string, entry: ForeignKeyDef): string {
  const actions = [
    entry.onDelete && entry.onDelete !== "NO ACTION" ? ` ON DELETE ${entry.onDelete}` : "",
    entry.onUpdate ? ` ON UPDATE ${entry.onUpdate}` : "",
  ].join("");
  return `ALTER TABLE ${qualified(schema, tableName)} ADD CONSTRAINT ${quote(entry.name ?? `${tableName}_fkey`)} FOREIGN KEY (${entry.columns.join(", ")}) REFERENCES ${qualified(schema, entry.refTable)} (${entry.refColumns.join(", ")})${actions};`;
}

/** Canonical DDL as the target dialect writes it, with ignored attributes left out. */
function tableDdl(schema: string, entry: TableDef, options: CompareOptions, order: string[] | null): string {
  const columns = [...entry.columns];
  if (order && options.ignoreColumnOrder) columns.sort((a, b) => rank(order, a.name) - rank(order, b.name));
  const lines = columns.map((item) => `    ${columnSql(item, options)}`);
  if (entry.primaryKey)
    lines.push(`    CONSTRAINT ${quote(entry.primaryKey.name ?? `${entry.name}_pkey`)} PRIMARY KEY (${entry.primaryKey.columns.join(", ")})`);
  for (const unique of entry.uniques) lines.push(`    CONSTRAINT ${quote(unique.name ?? "unique")} UNIQUE (${unique.columns.join(", ")})`);
  for (const check of entry.checks) lines.push(`    CONSTRAINT ${quote(check.name ?? "check")} ${check.expression}`);
  const statements = [`CREATE TABLE ${qualified(schema, entry.name)} (\n${lines.join(",\n")}\n);`];
  for (const item of entry.indexes) statements.push(indexSql(schema, entry.name, item));
  for (const item of entry.foreignKeys) statements.push(fkSql(schema, entry.name, item));
  if (entry.comment && !options.ignoreComments) statements.push(`COMMENT ON TABLE ${qualified(schema, entry.name)} IS '${entry.comment.replace(/'/g, "''")}';`);
  return statements.join("\n");
}

function rank(order: string[], name: string): number {
  const found = order.indexOf(name);
  return found < 0 ? order.length : found;
}

function viewDdl(schema: string, entry: ViewDef): string {
  const body = entry.definition.replace(/ (FROM|JOIN|WHERE|GROUP BY) /g, "\n  $1 ");
  return `CREATE VIEW ${qualified(schema, entry.name)} AS\n${body};`;
}

function varcharLength(type: string): number | null {
  const match = /^character varying\((\d+)\)$/.exec(type);
  return match ? Number(match[1]) : null;
}

function typeRisk(from: string, to: string): Risk {
  const before = varcharLength(from);
  const after = varcharLength(to);
  if (before !== null && after !== null) return after < before ? "dataLoss" : "none";
  return "review";
}

function change(part: string, name: string, what: Change["what"], extra: Partial<Change> = {}): Change {
  return { part, name, what, fields: [], risk: "none", note: null, possibleRename: null, ...extra };
}

function tableChanges(source: TableDef, target: TableDef, options: CompareOptions): Change[] {
  const changes: Change[] = [];
  const targetColumns = new Map(target.columns.map((item) => [item.name, item]));
  const sourceNames = new Set(source.columns.map((item) => item.name));
  const added = source.columns.filter((item) => !targetColumns.has(item.name));
  const removed = target.columns.filter((item) => !sourceNames.has(item.name));
  const rename =
    options.detectRenames &&
    added.length === 1 &&
    removed.length === 1 &&
    added[0].typeName === removed[0].typeName &&
    added[0].nullable === removed[0].nullable;
  for (const item of source.columns) {
    const other = targetColumns.get(item.name);
    if (!other) {
      const fields = [{ field: "type", source: item.typeName, target: null }];
      const needsValue = !item.nullable && item.default === null && !item.identity;
      changes.push(
        rename
          ? change("column", item.name, "added", {
              fields,
              risk: "review",
              note: `Possible rename of ${removed[0].name} to ${item.name}; the script drops and adds, so the data is lost`,
              possibleRename: removed[0].name,
            })
          : change("column", item.name, "added", {
              fields,
              risk: needsValue ? "review" : "none",
              note: needsValue ? "NOT NULL without a default fails on a table with rows" : null,
            }),
      );
      continue;
    }
    const fields: Change["fields"] = [];
    const risks: Risk[] = [];
    const notes: string[] = [];
    if (item.typeName !== other.typeName) {
      fields.push({ field: "type", source: item.typeName, target: other.typeName });
      const risk = typeRisk(other.typeName, item.typeName);
      risks.push(risk);
      if (risk === "dataLoss") notes.push("The type change may lose data");
    }
    if (item.nullable !== other.nullable) {
      fields.push({ field: "nullable", source: item.nullable ? "YES" : "NO", target: other.nullable ? "YES" : "NO" });
      if (!item.nullable) {
        risks.push("review");
        notes.push("Fails while the target has NULLs in this column");
      }
    }
    if (!options.ignoreDefaults && item.default !== other.default) fields.push({ field: "default", source: item.default, target: other.default });
    if (fields.length > 0) changes.push(change("column", item.name, "modified", { fields, risk: maxRisk(risks), note: notes.join("; ") || null }));
  }
  for (const item of removed) {
    changes.push(
      change("column", item.name, "removed", {
        fields: [{ field: "type", source: null, target: item.typeName }],
        risk: "dataLoss",
        note: rename ? `Possible rename to ${added[0].name}` : "Dropping the column loses its data",
        possibleRename: rename ? added[0].name : null,
      }),
    );
  }
  const named = <T extends { name: string | null }>(part: string, a: T[], b: T[], signature: (item: T) => string, risk: Risk, note: string | null) => {
    const others = new Map(b.map((item) => [item.name ?? signature(item), item]));
    for (const item of a) {
      const other = others.get(item.name ?? signature(item));
      if (!other)
        changes.push(change(part, item.name ?? "", "added", { fields: [{ field: "definition", source: signature(item), target: null }], risk, note }));
      else if (signature(other) !== signature(item))
        changes.push(
          change(part, item.name ?? "", "modified", { fields: [{ field: "definition", source: signature(item), target: signature(other) }], risk, note }),
        );
      others.delete(item.name ?? signature(item));
    }
    for (const [name, item] of others)
      changes.push(change(part, name, "removed", { fields: [{ field: "definition", source: null, target: signature(item) }] }));
  };
  named("unique", source.uniques, target.uniques, (item) => `UNIQUE (${item.columns.join(", ")})`, "review", "Fails if the target has duplicates");
  named("check", source.checks, target.checks, (item) => item.expression, "review", "Fails if existing rows break the check");
  named(
    "index",
    source.indexes,
    target.indexes,
    (item) =>
      `${item.unique ? "UNIQUE " : ""}(${item.parts.map((part) => `${part.column}${part.descending ? " DESC" : ""}`).join(", ")})${item.predicate ? ` WHERE ${item.predicate}` : ""}`,
    "none",
    null,
  );
  named(
    "foreignKey",
    source.foreignKeys,
    target.foreignKeys,
    (item) => `(${item.columns.join(", ")}) → ${item.refTable} (${item.refColumns.join(", ")})${item.onDelete ? ` ON DELETE ${item.onDelete}` : ""}`,
    "review",
    "Fails if existing rows have no match",
  );
  if (!options.ignoreComments && (source.comment ?? null) !== (target.comment ?? null))
    changes.push(change("comment", source.name, "modified", { fields: [{ field: "comment", source: source.comment, target: target.comment }] }));
  return changes;
}

export function compareDiff(source: SchemaSnapshot, target: SchemaSnapshot, options: CompareOptions): SchemaDiff {
  if (source.engine !== target.engine) throw mockError("ERROR", "Compare needs two connections of the same engine");
  const schema = target.scope.schema ?? target.scope.database;
  const skipped: SkippedObject[] = [...source.skipped];
  const items: DiffItem[] = [];
  const tableNames = [...new Set([...source.tables, ...target.tables].map((item) => item.name))].sort();
  for (const name of tableNames) {
    if (excluded(options, name)) {
      skipped.push({ kind: "table", name, reason: "Excluded by the compare options" });
      continue;
    }
    const a = source.tables.find((item) => item.name === name) ?? null;
    const b = target.tables.find((item) => item.name === name) ?? null;
    const order = a ? a.columns.map((item) => item.name) : null;
    const sourceDdl = a ? tableDdl(schema, a, options, null) : null;
    const targetDdl = b ? tableDdl(schema, b, options, order) : null;
    if (a && b) {
      const changes = tableChanges(a, b, options);
      items.push({
        id: `table:${name}`,
        kind: "table",
        name,
        status: changes.length ? "different" : "identical",
        changes,
        risk: maxRisk(changes.map((item) => item.risk)),
        sourceDdl,
        targetDdl,
      });
    } else
      items.push({ id: `table:${name}`, kind: "table", name, status: a ? "onlyInSource" : "onlyInTarget", changes: [], risk: "none", sourceDdl, targetDdl });
  }
  const viewNames = [...new Set([...source.views, ...target.views].map((item) => item.name))].sort();
  for (const name of viewNames) {
    const a = source.views.find((item) => item.name === name) ?? null;
    const b = target.views.find((item) => item.name === name) ?? null;
    const changes =
      a && b && a.definition !== b.definition
        ? [change("definition", name, "modified", { fields: [{ field: "definition", source: a.definition, target: b.definition }] })]
        : [];
    items.push({
      id: `view:${name}`,
      kind: "view",
      name,
      status: a && b ? (changes.length ? "different" : "identical") : a ? "onlyInSource" : "onlyInTarget",
      changes,
      risk: "none",
      sourceDdl: a ? viewDdl(schema, a) : null,
      targetDdl: b ? viewDdl(schema, b) : null,
    });
  }
  const summary = (snapshot: SchemaSnapshot) => ({
    engine: snapshot.engine,
    serverVersion: snapshot.serverVersion,
    database: snapshot.scope.database,
    schema: snapshot.scope.schema,
  });
  return { source: summary(source), target: summary(target), options, items, skipped };
}

// --- Script ----------------------------------------------------------------------------------------------------------------

interface Statement {
  sql: string;
  warnings: string[];
  itemId: string;
  risk: Risk;
}

export function compareScript(source: SchemaSnapshot, target: SchemaSnapshot, options: CompareOptions, include: string[]): SyncScript {
  const diff = compareDiff(source, target, options);
  const schema = target.scope.schema ?? target.scope.database;
  const chosen = diff.items.filter((item) => include.includes(item.id) && item.status !== "identical");
  const phases: Statement[][] = Array.from({ length: 9 }, () => []);
  const push = (phase: number, itemId: string, sql: string, risk: Risk = "none", warning: string | null = null) =>
    phases[phase].push({ sql, itemId, risk, warnings: warning ? [warning] : [] });
  const tableOf = (snapshot: SchemaSnapshot, name: string) => snapshot.tables.find((item) => item.name === name);
  for (const item of chosen) {
    const at = qualified(schema, item.name);
    if (item.kind === "view") {
      if (item.status !== "onlyInSource") push(1, item.id, `DROP VIEW ${at};`, item.status === "onlyInTarget" ? "review" : "none");
      const view = source.views.find((entry) => entry.name === item.name);
      if (view) push(7, item.id, viewDdl(schema, view));
      continue;
    }
    if (item.status === "onlyInTarget") {
      push(3, item.id, `DROP TABLE ${at};`, "dataLoss", `Drops table ${item.name} and all its rows`);
      continue;
    }
    const a = tableOf(source, item.name);
    if (!a) continue;
    if (item.status === "onlyInSource") {
      const create = tableDdl(schema, { ...a, indexes: [], foreignKeys: [], comment: null }, options, null);
      push(4, item.id, create);
      for (const entry of a.indexes) push(6, item.id, indexSql(schema, a.name, entry));
      for (const entry of a.foreignKeys) push(8, item.id, fkSql(schema, a.name, entry));
      continue;
    }
    for (const entry of item.changes) {
      const field = (name: string) => entry.fields.find((candidate) => candidate.field === name);
      if (entry.part === "column") {
        const def = a.columns.find((candidate) => candidate.name === entry.name);
        if (entry.what === "added" && def) push(5, item.id, `ALTER TABLE ${at} ADD COLUMN ${columnSql(def, options)};`, entry.risk, entry.note);
        if (entry.what === "removed")
          push(5, item.id, `ALTER TABLE ${at} DROP COLUMN ${quote(entry.name)};`, "dataLoss", entry.note ?? `Drops column ${entry.name} and its data`);
        if (entry.what === "modified" && def) {
          const type = field("type");
          if (type)
            push(
              5,
              item.id,
              `ALTER TABLE ${at} ALTER COLUMN ${quote(entry.name)} TYPE ${def.typeName};`,
              typeRisk(type.target ?? "", def.typeName),
              typeRisk(type.target ?? "", def.typeName) === "dataLoss"
                ? `Narrows ${entry.name} from ${type.target} to ${def.typeName}; longer values fail or are cut`
                : null,
            );
          const nullable = field("nullable");
          if (nullable)
            push(
              5,
              item.id,
              def.nullable
                ? `ALTER TABLE ${at} ALTER COLUMN ${quote(entry.name)} DROP NOT NULL;`
                : `-- UPDATE ${at} SET ${quote(entry.name)} = … WHERE ${quote(entry.name)} IS NULL;\nALTER TABLE ${at} ALTER COLUMN ${quote(entry.name)} SET NOT NULL;`,
              def.nullable ? "none" : "review",
              def.nullable ? null : `Fails while ${item.name}.${entry.name} has NULLs`,
            );
          const fallback = field("default");
          if (fallback)
            push(
              5,
              item.id,
              def.default === null
                ? `ALTER TABLE ${at} ALTER COLUMN ${quote(entry.name)} DROP DEFAULT;`
                : `ALTER TABLE ${at} ALTER COLUMN ${quote(entry.name)} SET DEFAULT ${def.default};`,
            );
        }
      } else if (entry.part === "index") {
        if (entry.what !== "added") push(2, item.id, `DROP INDEX ${qualified(schema, entry.name)};`);
        const def = a.indexes.find((candidate) => candidate.name === entry.name);
        if (def) push(6, item.id, indexSql(schema, a.name, def));
      } else if (entry.part === "foreignKey") {
        if (entry.what !== "added") push(0, item.id, `ALTER TABLE ${at} DROP CONSTRAINT ${quote(entry.name)};`);
        const def = a.foreignKeys.find((candidate) => candidate.name === entry.name);
        if (def) push(8, item.id, fkSql(schema, a.name, def), "review", entry.note);
      } else if (entry.part === "check" || entry.part === "unique") {
        if (entry.what !== "added") push(2, item.id, `ALTER TABLE ${at} DROP CONSTRAINT ${quote(entry.name)};`);
        const definition = field("definition")?.source;
        if (definition) push(6, item.id, `ALTER TABLE ${at} ADD CONSTRAINT ${quote(entry.name)} ${definition};`, "review", entry.note);
      } else if (entry.part === "comment") {
        const text = field("comment")?.source ?? null;
        push(8, item.id, `COMMENT ON TABLE ${at} IS ${text === null ? "NULL" : `'${text.replace(/'/g, "''")}'`};`);
      }
    }
  }
  const statements = phases.flat();
  const transactional = options.transactional ?? true;
  const warnings: ScriptWarning[] = statements.flatMap((statement) =>
    statement.warnings.map((message) => ({ itemId: statement.itemId, risk: statement.risk === "none" ? "review" : statement.risk, message })),
  );
  const counts = (risk: Risk) => warnings.filter((warning) => warning.risk === risk).length;
  const describe = (snapshot: SchemaSnapshot) => `${snapshot.scope.database}.${snapshot.scope.schema ?? ""} (PostgreSQL ${snapshot.serverVersion})`;
  const header = [
    `-- Generated by Fosta Studio (dev mock) at ${new Date().toISOString().slice(0, 16).replace("T", " ")} UTC`,
    `-- Source: ${describe(source)} -> Target: ${describe(target)}`,
    `-- Makes the target match the source. ${chosen.length} object(s), ${counts("dataLoss")} data-loss and ${counts("review")} review warning(s).`,
    "-- Review before running. Nothing has been run.",
  ];
  const body = statements.map((statement) => [...statement.warnings.map((message) => `-- WARNING: ${message}`), statement.sql].join("\n"));
  const lines = [...header, "", ...(transactional ? ["BEGIN;", ""] : []), body.join("\n\n"), ...(transactional ? ["", "COMMIT;"] : [])];
  return {
    sql: `${statements.length === 0 ? [...header, "", "-- Nothing to do."].join("\n") : lines.join("\n")}\n`,
    warnings,
    statements: statements.length + (transactional && statements.length ? 2 : 0),
    transactional,
    dialect: target.engine,
  };
}
