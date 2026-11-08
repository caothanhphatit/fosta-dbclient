/**
 * The MariaDB and MySQL mocks: the shop and analytics databases, backtick quoting, MySQL error numbers, and the vector
 * operations MariaDB offers through `invoke_capability`.
 */
import type { ConnectInfo, EngineDescriptor } from "../../core/contract";
import { builtin, MockCatalog, queryAction, viewAction, type CatalogStyle } from "./catalog";
import { mockError } from "./errors";
import { analyticsDatabase, mysqlSystemDatabases, shopDatabase, VECTOR_DIMS } from "./shop";
import { classifySql, runSql, type SqlDialectMock } from "./sqlRun";
import type { EngineMock } from "./types";

export function mysqlFamily(descriptor: EngineDescriptor, order: number, flavor: "MariaDB" | "MySQL", version: string, label: string): EngineMock {
  const engine = descriptor.id;
  const serverError = (vendorCode: number, sqlstate: string, message: string) =>
    mockError("ENGINE_ERROR", message, { engine, vendorCode, sqlstate, kind: "server", retryable: false });
  const errors = {
    syntax: (near: string, line = 1) =>
      serverError(
        1064,
        "42000",
        `You have an error in your SQL syntax; check the manual that corresponds to your ${flavor} server version for the right syntax to use near '${near}' at line ${line}`,
      ),
    noTable: (name: string) => serverError(1146, "42S02", `Table '${name}' doesn't exist`),
    noColumn: (name: string, where: string) => serverError(1054, "42S22", `Unknown column '${name}' in '${where}'`),
    cancelled: () =>
      mockError("CANCELLED", "Query execution was interrupted", { engine, vendorCode: 1317, sqlstate: "70100", kind: "cancelled", retryable: false }),
  };
  const style: CatalogStyle = {
    engine,
    quote: (name) => `\`${name.replace(/`/g, "``")}\``,
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
      ...(table.kind === "view" ? [] : [queryAction("truncate", "Truncate…", "TRUNCATE TABLE {qualified}", true)]),
    ],
    errors,
  };
  const catalog = new MockCatalog(style, [shopDatabase("shop", flavor), analyticsDatabase(), ...mysqlSystemDatabases()]);
  const dialect: SqlDialectMock = { split: { hashComments: true }, errors, version, ddlCommits: true };

  const products = () => catalog.table({ database: "shop", schema: null, name: "products" });
  const vectorOps: Record<string, (args: Record<string, unknown>) => unknown> = {
    "vector.info": () => ({
      supported: true,
      columns: [{ name: "embedding", dims: VECTOR_DIMS }],
      indexes: [{ name: "idx_embedding", column: "embedding", metric: "cosine", m: 6 }],
    }),
    "vector.fetch": () => [0.12, -0.4, 0.33, 0.9],
    "vector.search": (args) => {
      const k = Number(args.k ?? 10);
      const page = catalog.browse({
        object: products().object,
        filter: { kind: "none" },
        sort: [],
        offset: 0,
        limit: k,
        after: null,
        estimateTotal: false,
        exactTotal: false,
        columns: null,
      });
      return {
        columns: page.columns,
        rows: page.rows.map((row, index) => [...row, { t: "float", v: 0.05 + index * 0.031 }]),
        indexUsed: "idx_embedding",
        elapsedMs: 4,
      };
    },
    "vector.health": () => [
      { label: "Dimensions", status: "ok", value: String(VECTOR_DIMS), detail: "Every sampled row has 4 values" },
      { label: "Zero vectors", status: "warn", value: "3", detail: "Cosine distance is undefined for all-zero vectors" },
    ],
    "vector.recall": (args) => ({
      index: "idx_embedding",
      metric: "cosine",
      k: Number(args.k ?? 10),
      queries: Number(args.queries ?? 20),
      recall: 0.97,
      worstRecall: 0.8,
      annMs: 1.4,
      exactMs: 9.8,
    }),
  };

  return {
    descriptor,
    order,
    connectInfo: (): ConnectInfo => ({
      engine,
      server: { product: flavor, version, label, flavor: null },
      capabilities: descriptor.capabilities,
      languages: descriptor.languages,
      connectionActions: [builtin("createDatabase", "New database…", "createDatabase"), builtin("refresh", "Refresh", "refresh")],
    }),
    tree: (parent) => catalog.tree(parent),
    describe: (object) => catalog.describe(object),
    browse: (request) => catalog.browse(request),
    apply: (changes) => catalog.apply(changes),
    run: (request, emit, context) => runSql(catalog, dialect, request, emit, context),
    classify: (text) => classifySql(text, dialect.split),
    invoke: (name, args) => {
      const op = descriptor.capabilities.includes("vector") ? vectorOps[name] : undefined;
      return op ? op((args ?? {}) as Record<string, unknown>) : undefined;
    },
    schemaGraph: (database) => catalog.schemaGraph(database),
    completion: (database) => catalog.completion(database),
    createDatabase: (name) => catalog.addDatabase(name),
    touchRow: (table, key, remove, column) => catalog.touch(table, key, remove, column),
  };
}
