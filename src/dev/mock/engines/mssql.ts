/**
 * SQL Server 2022 mock: an ERP database with `dbo` and `sales` schemas. Tables are versioned like `rowversion` columns:
 * pages carry `rowVersions` and edits are checked by version (the conflict demo works here too).
 */
import type { ConnectInfo, ForeignKeyInfo } from "../../../core/contract";
import { builtin, column, MockCatalog, queryAction, v, viewAction, type CatalogStyle, type TableSpec } from "../catalog";
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mockError } from "../errors";
import { classifySql, runSql, type SqlDialectMock } from "../sqlRun";
import type { EngineMock } from "../types";
import type { ConnectionProfile } from "../../../core/types";

export const descriptor = DESCRIPTORS.mssql;
export const order = ORDER.indexOf("mssql");

const ENGINE = "mssql";
const VERSION = "16.0.1000.6";

const serverError = (vendorCode: number, message: string, extra: Record<string, unknown> = {}) =>
  mockError("ENGINE_ERROR", message, { engine: ENGINE, vendorCode, kind: "server", retryable: false, ...extra });

const errors = {
  syntax: (near: string) => serverError(102, `Incorrect syntax near '${near.split(/\s+/)[0]}'.`, { detail: "Msg 102, Level 15, State 1" }),
  noTable: (name: string) => serverError(208, `Invalid object name '${name}'.`, { detail: "Msg 208, Level 16, State 1" }),
  noColumn: (name: string) => serverError(207, `Invalid column name '${name}'.`, { detail: "Msg 207, Level 16, State 1" }),
  cancelled: () => mockError("CANCELLED", "The query was cancelled.", { engine: ENGINE, sqlstate: "HY008", kind: "cancelled", retryable: false }),
};

const style: CatalogStyle = {
  engine: ENGINE,
  quote: (name) => `[${name.replace(/]/g, "]]")}]`,
  label: (table) => `${table.schema ?? "dbo"}.${table.name}`,
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

const COMPANIES = ["Northwind Traders", "Contoso Ltd", "Fabrikam Inc", "Adventure Works", "Tailspin Toys", "Wide World Importers", "Litware Inc", "Proseware"];
const CONTACTS = [
  "Maria Anders",
  "Thomas Hardy",
  "Hanna Moos",
  "Martin Sommer",
  "Laurence Lebihan",
  "Elizabeth Lincoln",
  "Victoria Ashworth",
  "Patricio Simpson",
];
const COUNTRIES = ["USA", "Germany", "UK", "France", "Canada", "Spain", "Sweden", "Japan"];
const PRODUCTS = ["Steel bolt M8", "Hex nut M8", "Washer 8mm", "Bearing 608", "Drive belt", "Hydraulic hose", "Gear pump", "Pressure valve"];
const STATUS = ["Open", "Released", "Shipped", "Closed"];

const pad = (n: number, width = 2) => String(n).padStart(width, "0");
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
const identity = "IDENTITY(1,1)";

const tables: TableSpec[] = [
  {
    database: "ERP",
    schema: "dbo",
    name: "Customers",
    kind: "table",
    key: ["CustomerID"],
    versioned: true,
    columns: [
      column("CustomerID", "int", "int", { primary: true, identity }),
      column("CompanyName", "nvarchar(100)", "text", { nullable: false }),
      column("ContactName", "nvarchar(80)", "text"),
      column("Country", "nvarchar(40)", "text"),
      column("CreditLimit", "money", "decimal", { nullable: false, default: "((0))" }),
      column("IsActive", "bit", "bool", { nullable: false, default: "((1))" }),
      column("CreatedAt", "datetime2(3)", "timestamp", { nullable: false, default: "(sysutcdatetime())" }),
    ],
    rows: () =>
      Array.from({ length: 180 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(n),
          v.text(`${COMPANIES[n % COMPANIES.length]}${n > COMPANIES.length ? ` ${Math.ceil(n / COMPANIES.length)}` : ""}`),
          n % 17 === 0 ? v.null() : v.text(CONTACTS[n % CONTACTS.length]),
          v.text(COUNTRIES[(n * 3) % COUNTRIES.length]),
          v.dec(((n % 12) * 2500).toFixed(4)),
          v.bool(n % 9 !== 0),
          v.ts(`2025-${pad((n % 12) + 1)}-${pad((n % 28) + 1)} 10:${pad(n % 60)}:00.${pad(n % 1000, 3)}`),
        ];
      }),
    extra: [["Filegroup", "PRIMARY"]],
  },
  {
    database: "ERP",
    schema: "dbo",
    name: "Products",
    kind: "table",
    key: ["ProductID"],
    versioned: true,
    columns: [
      column("ProductID", "int", "int", { primary: true, identity }),
      column("Name", "nvarchar(120)", "text", { nullable: false }),
      column("UnitPrice", "decimal(10,2)", "decimal", { nullable: false }),
      column("UnitsInStock", "int", "int"),
      column("Discontinued", "bit", "bool", { nullable: false }),
      column("RowGuid", "uniqueidentifier", "uuid", { nullable: false, default: "(newid())" }),
    ],
    rows: () =>
      Array.from({ length: 64 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(n),
          v.text(`${PRODUCTS[n % PRODUCTS.length]} (${n})`),
          v.dec((0.35 + (n % 40) * 3.1).toFixed(2)),
          n % 10 === 0 ? v.null() : v.int((n * 37) % 500),
          v.bool(n % 15 === 0),
          v.uuid(`6f9619ff-8b86-d011-b42d-${(0xc04fc964ff00 + n).toString(16).padStart(12, "0")}`),
        ];
      }),
  },
  {
    database: "ERP",
    schema: "sales",
    name: "Orders",
    kind: "table",
    key: ["OrderID"],
    versioned: true,
    columns: [
      column("OrderID", "int", "int", { primary: true, identity }),
      column("CustomerID", "int", "int", { nullable: false }),
      column("OrderDate", "date", "date", { nullable: false }),
      column("ShippedAt", "datetimeoffset(0)", "timestampTz"),
      column("Status", "nvarchar(20)", "text", { nullable: false }),
      column("Total", "decimal(12,2)", "decimal", { nullable: false }),
    ],
    rows: () =>
      Array.from({ length: 900 }, (_, index) => {
        const n = index + 1;
        const status = STATUS[n % STATUS.length];
        return [
          v.int(n),
          v.int(((n * 7) % 180) + 1),
          v.date(`2026-${pad((n % 10) + 1)}-${pad((n % 28) + 1)}`),
          status === "Shipped" || status === "Closed" ? v.tstz(`2026-${pad((n % 10) + 1)}-${pad((n % 28) + 1)} 16:30:00+01:00`) : v.null(),
          v.text(status),
          v.dec((120 + ((n * 53) % 9000)).toFixed(2)),
        ];
      }),
    foreignKeys: [fk("FK_Orders_Customers", ["CustomerID"], "dbo", "Customers", ["CustomerID"])],
  },
  {
    database: "ERP",
    schema: "sales",
    name: "OrderLines",
    kind: "table",
    key: ["OrderLineID"],
    versioned: true,
    columns: [
      column("OrderLineID", "int", "int", { primary: true, identity }),
      column("OrderID", "int", "int", { nullable: false }),
      column("ProductID", "int", "int", { nullable: false }),
      column("Quantity", "int", "int", { nullable: false }),
      column("UnitPrice", "decimal(10,2)", "decimal", { nullable: false }),
    ],
    rows: () =>
      Array.from({ length: 2400 }, (_, index) => {
        const n = index + 1;
        return [v.int(n), v.int(((n * 3) % 900) + 1), v.int(((n * 11) % 64) + 1), v.int((n % 12) + 1), v.dec((0.35 + (n % 40) * 3.1).toFixed(2))];
      }),
    foreignKeys: [
      fk("FK_OrderLines_Orders", ["OrderID"], "sales", "Orders", ["OrderID"]),
      fk("FK_OrderLines_Products", ["ProductID"], "dbo", "Products", ["ProductID"]),
    ],
  },
  {
    database: "ERP",
    schema: "sales",
    name: "v_OpenOrders",
    kind: "view",
    key: [],
    columns: [
      column("OrderID", "int", "int", { nullable: false }),
      column("CompanyName", "nvarchar(100)", "text", { nullable: false }),
      column("Total", "decimal(12,2)", "decimal"),
    ],
    rows: () =>
      Array.from({ length: 40 }, (_, index) => [v.int(index * 4 + 4), v.text(COMPANIES[index % COMPANIES.length]), v.dec((300 + index * 17.5).toFixed(2))]),
    ddl: "CREATE VIEW [sales].[v_OpenOrders] AS\nSELECT o.[OrderID], c.[CompanyName], o.[Total]\nFROM [sales].[Orders] o JOIN [dbo].[Customers] c ON c.[CustomerID] = o.[CustomerID]\nWHERE o.[Status] = N'Open'",
  },
];

const catalog = new MockCatalog(style, [{ name: "ERP", tables }, ...["master", "model", "msdb", "tempdb"].map((name) => ({ name, system: true, tables: [] }))]);
const dialect: SqlDialectMock = {
  split: { bracketIdentifiers: true },
  errors,
  version: `Microsoft SQL Server 2022 (RTM) - ${VERSION} (X64) Developer Edition`,
};

/**
 * The dev server has a self-signed certificate, like a real SQL Server without one installed: an encrypted connection
 * that does not accept invalid certificates fails with the backend's message (the dialog then points at the option).
 */
function checkCertificate(profile: ConnectionProfile): void {
  const tls = profile.tls;
  const encrypted = !tls || tls.mode !== "disable";
  if (encrypted && !tls?.acceptInvalidCerts && !tls?.caFile)
    throw mockError(
      "CONNECTION_ERROR",
      "Could not set up TLS with SQL Server: A certificate chain processed, but terminated in a root certificate which is not trusted by the trust " +
        "provider. (os error -2146762487). The server's TLS certificate is not trusted by this machine (SQL Server uses a self-signed certificate " +
        'unless one is installed). For a local or test server, tick "Accept invalid certificates" in the TLS section of the connection dialog ' +
        '("Trust server certificate" in other SQL Server tools), or install a certificate from a trusted authority on the server.',
      { engine: ENGINE },
    );
}

export const connectInfo = (profile: ConnectionProfile): ConnectInfo => {
  checkCertificate(profile);
  return info();
};

const info = (): ConnectInfo => ({
  engine: ENGINE,
  server: { product: "Microsoft SQL Server", version: VERSION, label: `SQL Server 2022 (${VERSION})`, flavor: "Developer Edition" },
  capabilities: descriptor.capabilities,
  languages: descriptor.languages,
  connectionActions: [builtin("createDatabase", "New database…", "createDatabase"), builtin("refresh", "Refresh", "refresh")],
});
export const tree: typeof catalog.tree = (parent) => catalog.tree(parent);
export const describe: typeof catalog.describe = (object) => catalog.describe(object);
export const browse: typeof catalog.browse = (request) => catalog.browse(request);
export const apply: typeof catalog.apply = (changes) => catalog.apply(changes);
export const run: NonNullable<EngineMock["run"]> = (request, emit, context) => runSql(catalog, dialect, request, emit, context);
export const classify = (text: string) => classifySql(text, dialect.split);
export const invoke = (): unknown => undefined;
export const schemaGraph = (database: string) => catalog.schemaGraph(database);
export const completion = (database: string) => catalog.completion(database);
export const createDatabase = (name: string) => catalog.addDatabase(name);
export const touchRow = (table: string, key: unknown, remove: boolean, column?: string) => catalog.touch(table, key, remove, column);
