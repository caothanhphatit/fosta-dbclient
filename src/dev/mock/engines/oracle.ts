/**
 * Oracle 23ai mock: the schemas SHOP and HR are the tree's databases (as in the backend), each with folders for tables,
 * views, sequences, synonyms, packages, procedures and functions. AUDIT_LOG has no primary key and is edited by ROWID
 * (`_rowid_`); PL/SQL units open in the definition view, TOUCH_ORDER as INVALID with a compile error.
 *
 * Instant Client demo: the first connect fails with `ORACLE_CLIENT` (client not installed), which opens the Oracle prompt.
 * "Download" streams fake progress through `oracle_client_download` (Cancel stops it); "Use folder" saves the profile's
 * Instant Client folder, which the mock accepts when it is not empty. Reload the page to see the prompt again.
 */
import type { ConnectInfo, EngineDescriptor, ObjectDescription, ObjectNode, ObjectRef, TreeRequest } from "../../../core/contract";
import type { ConnectionProfile } from "../../../core/types";
import { builtin, column, MockCatalog, queryAction, v, viewAction, type CatalogStyle, type TableSpec } from "../catalog";
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mockError } from "../errors";
import { classifySql, runSql, type SqlDialectMock } from "../sqlRun";
import type { EngineMock } from "../types";

const base: EngineDescriptor = DESCRIPTORS.oracle;
export const descriptor: EngineDescriptor = {
  ...base,
  connection: {
    ...base.connection,
    fields: [
      ...base.connection.fields,
      {
        key: "options.clientDir",
        kind: { type: "text" },
        label: "Instant Client folder",
        placeholder: "C:\\oracle\\instantclient_23_26",
        hint: "A folder with Oracle Instant Client 19c or newer (oci.dll, or libclntsh.so on Linux). Leave it empty to use PATH, ORACLE_HOME or the client Fosta Studio downloads. The first client loaded stays in use until the app restarts.",
        section: "advanced",
        default: null,
        required: false,
      },
    ],
  },
  languages: base.languages.map((language) => ({
    ...language,
    hint: "Oracle SQL and PL/SQL blocks; end a PL/SQL block with '/' on its own line. Each statement commits unless a transaction is begun. Ctrl+Enter runs the query.",
  })),
};
export const order = ORDER.indexOf("oracle");

const ENGINE = "oracle";
const VERSION = "23.6.0.24.10";
const ROWID = "_rowid_";

const serverError = (code: number, message: string, extra: Record<string, unknown> = {}) =>
  mockError("ENGINE_ERROR", `ORA-${String(code).padStart(5, "0")}: ${message}`, {
    engine: ENGINE,
    vendorCode: `ORA-${String(code).padStart(5, "0")}`,
    kind: "server",
    retryable: false,
    ...extra,
  });

const errors = {
  syntax: () => serverError(900, "invalid SQL statement"),
  noTable: () => serverError(942, "table or view does not exist"),
  noColumn: (name: string) => serverError(904, `"${name.toUpperCase()}": invalid identifier`),
  cancelled: () =>
    mockError("CANCELLED", "ORA-01013: user requested cancel of current operation", { engine: ENGINE, vendorCode: "ORA-01013", kind: "cancelled" }),
};

const explain = "EXPLAIN PLAN FOR SELECT * FROM {qualified};\nSELECT plan_table_output FROM TABLE(DBMS_XPLAN.DISPLAY());";

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
    ...(table.kind === "view"
      ? [queryAction("explain", "Explain a select", explain)]
      : [
          queryAction("count", "Count rows", "SELECT COUNT(*) FROM {qualified}"),
          queryAction("explain", "Explain a select", explain),
          queryAction("truncate", "Truncate…", "TRUNCATE TABLE {qualified}", true),
        ]),
  ],
  errors,
};

const NAMES = ["An Nguyen", "Binh Tran", "Chau Le", "Dung Pham", "Em Vo", "Giang Do", "Hoa Bui", "Khanh Dang"];
const STATUS = ["NEW", "PAID", "SHIPPED", "CANCELLED"];
const pad = (n: number, width = 2) => String(n).padStart(width, "0");

const tables: TableSpec[] = [
  {
    database: "SHOP",
    name: "CUSTOMERS",
    kind: "table",
    key: ["ID"],
    columns: [
      column("ID", "NUMBER(10)", "int", { primary: true, identity: "BY DEFAULT" }),
      column("EMAIL", "VARCHAR2(200 CHAR)", "text", { nullable: false }),
      column("FULL_NAME", "VARCHAR2(100 CHAR)", "text", { nullable: false }),
      column("CREATED_AT", "TIMESTAMP(6) WITH TIME ZONE", "timestampTz", { nullable: false, default: "SYSTIMESTAMP" }),
      column("VIP", "BOOLEAN", "bool", { nullable: false, default: "FALSE" }),
      column("PROFILE", "JSON", "json", {}),
    ],
    rows: () =>
      Array.from({ length: 60 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(n),
          v.text(`customer${n}@example.com`),
          v.text(`${NAMES[n % NAMES.length]} ${n}`),
          v.tstz(`2026-0${(n % 9) + 1}-${pad((n % 28) + 1)} 09:${pad(n % 60)}:00+07:00`),
          v.bool(n % 7 === 0),
          n % 5 === 0 ? v.null() : v.json({ plan: n % 3 === 0 ? "pro" : "free" }),
        ];
      }),
    comment: "People who ordered at least once.",
  },
  {
    database: "SHOP",
    name: "ORDERS",
    kind: "table",
    key: ["ID"],
    columns: [
      column("ID", "NUMBER(10)", "int", { primary: true }),
      column("CUSTOMER_ID", "NUMBER(10)", "int", { nullable: false }),
      column("STATUS", "VARCHAR2(20)", "text", { nullable: false, default: "'NEW'" }),
      column("TOTAL", "NUMBER(12,2)", "decimal", { nullable: false }),
      column("ORDERED_AT", "DATE", "timestamp", { nullable: false }),
      column("NOTE", "CLOB", "text", {}),
    ],
    rows: () =>
      Array.from({ length: 400 }, (_, index) => {
        const n = index + 1;
        return [
          v.int(n),
          v.int((n % 60) + 1),
          v.text(STATUS[n % STATUS.length]),
          v.dec((n * 3.75).toFixed(2)),
          v.ts(`2026-0${(n % 9) + 1}-${pad((n % 28) + 1)} 1${n % 10}:00:00`),
          n % 4 === 0 ? v.text(`Deliver after ${(n % 12) + 8}:00`) : v.null(),
        ];
      }),
    foreignKeys: [
      {
        name: "FK_ORDERS_CUSTOMER",
        columns: ["CUSTOMER_ID"],
        refDatabase: "SHOP",
        refSchema: null,
        refTable: "CUSTOMERS",
        refColumns: ["ID"],
        onUpdate: null,
        onDelete: "NO ACTION",
      },
    ],
  },
  {
    database: "SHOP",
    name: "AUDIT_LOG",
    kind: "table",
    key: [ROWID],
    columns: [
      column(ROWID, "ROWID", "text", { nullable: false, identity: "ROWID", hints: { primary: true, readOnly: true } }),
      column("EVENT", "VARCHAR2(50)", "text", {}),
      column("LOGGED_AT", "TIMESTAMP(6)", "timestamp", {}),
    ],
    rows: () =>
      Array.from({ length: 12 }, (_, index) => [
        v.text(`AAAR3sAAEAAAACX${String.fromCharCode(65 + index)}AA`),
        v.text(index % 2 ? "login" : "export"),
        v.ts(`2026-11-0${(index % 8) + 1} 08:${pad(index * 4)}:00`),
      ]),
    comment: "No primary key: rows are edited by ROWID.",
  },
  {
    database: "SHOP",
    name: "ORDER_TOTALS",
    kind: "view",
    key: [],
    columns: [column("CUSTOMER_ID", "NUMBER(10)", "int", {}), column("ORDERS", "NUMBER", "decimal", {}), column("TOTAL", "NUMBER", "decimal", {})],
    rows: () => Array.from({ length: 20 }, (_, index) => [v.int(index + 1), v.dec(String(index + 3)), v.dec(((index + 3) * 41.5).toFixed(2))]),
    ddl: 'CREATE OR REPLACE FORCE EDITIONABLE VIEW "SHOP"."ORDER_TOTALS" AS\n  SELECT customer_id, COUNT(*) orders, SUM(total) total FROM orders GROUP BY customer_id;',
  },
  {
    database: "HR",
    name: "EMPLOYEES",
    kind: "table",
    key: ["EMPLOYEE_ID"],
    columns: [
      column("EMPLOYEE_ID", "NUMBER(6)", "int", { primary: true }),
      column("FIRST_NAME", "VARCHAR2(20)", "text", {}),
      column("LAST_NAME", "VARCHAR2(25)", "text", { nullable: false }),
      column("HIRE_DATE", "DATE", "timestamp", { nullable: false }),
      column("SALARY", "NUMBER(8,2)", "decimal", {}),
    ],
    rows: () =>
      Array.from({ length: 30 }, (_, index) => [
        v.int(100 + index),
        v.text(NAMES[index % NAMES.length].split(" ")[0]),
        v.text(NAMES[index % NAMES.length].split(" ")[1]),
        v.ts(`20${pad(10 + (index % 15))}-0${(index % 9) + 1}-15 00:00:00`),
        v.dec(String(3000 + index * 250)),
      ]),
  },
];

interface Other {
  database: string;
  name: string;
  kind: "sequence" | "synonym" | "package" | "procedure" | "function";
  detail: string | null;
  invalid?: boolean;
  ddl: string;
  extra: [string, string][];
}

const others: Other[] = [
  {
    database: "SHOP",
    name: "INVOICE_NO",
    kind: "sequence",
    detail: null,
    ddl: 'CREATE SEQUENCE "SHOP"."INVOICE_NO" MINVALUE 1 MAXVALUE 9999999999999999999999999999 INCREMENT BY 10 START WITH 1000 CACHE 20 NOORDER NOCYCLE;',
    extra: [
      ["minValue", "1"],
      ["maxValue", "9999999999999999999999999999"],
      ["incrementBy", "10"],
      ["cycle", "N"],
      ["cacheSize", "20"],
      ["lastNumber", "1200"],
    ],
  },
  {
    database: "SHOP",
    name: "CUST",
    kind: "synonym",
    detail: "SHOP.CUSTOMERS",
    ddl: 'CREATE OR REPLACE EDITIONABLE SYNONYM "SHOP"."CUST" FOR "SHOP"."CUSTOMERS";',
    extra: [["target", "SHOP.CUSTOMERS"]],
  },
  {
    database: "SHOP",
    name: "SHOP_PKG",
    kind: "package",
    detail: null,
    ddl: 'CREATE OR REPLACE EDITIONABLE PACKAGE "SHOP"."SHOP_PKG" AS\n  FUNCTION order_count RETURN NUMBER;\nEND shop_pkg;\n/\n\nCREATE OR REPLACE EDITIONABLE PACKAGE BODY "SHOP"."SHOP_PKG" AS\n  FUNCTION order_count RETURN NUMBER IS\n    n NUMBER;\n  BEGIN\n    SELECT COUNT(*) INTO n FROM orders;\n    RETURN n;\n  END;\nEND shop_pkg;\n/',
    extra: [
      ["status", "VALID"],
      ["created", "2026-11-01 09:00:00"],
      ["lastDdl", "2026-11-07 16:20:11"],
      ["bodyStatus", "VALID"],
    ],
  },
  {
    database: "SHOP",
    name: "TOUCH_ORDER",
    kind: "procedure",
    detail: null,
    invalid: true,
    ddl: 'CREATE OR REPLACE EDITIONABLE PROCEDURE "SHOP"."TOUCH_ORDER" (oid NUMBER) IS\nBEGIN\n  UPDATE orders SET note = note || \'.\' WHERE id = oid\nEND;\n/',
    extra: [
      ["status", "INVALID"],
      ["created", "2026-11-01 09:00:00"],
      ["lastDdl", "2026-11-08 10:02:45"],
      ["error1", 'PROCEDURE line 4, column 1: PLS-00103: Encountered the symbol "END" when expecting one of the following: ;'],
    ],
  },
  {
    database: "SHOP",
    name: "CUSTOMER_ORDER_COUNT",
    kind: "function",
    detail: null,
    ddl: 'CREATE OR REPLACE EDITIONABLE FUNCTION "SHOP"."CUSTOMER_ORDER_COUNT" (cid NUMBER) RETURN NUMBER IS\n  n NUMBER;\nBEGIN\n  SELECT COUNT(*) INTO n FROM orders WHERE customer_id = cid;\n  RETURN n;\nEND;\n/',
    extra: [
      ["status", "VALID"],
      ["created", "2026-11-01 09:00:00"],
      ["lastDdl", "2026-11-01 09:00:00"],
    ],
  },
];

const catalog = new MockCatalog(style, [
  { name: "SHOP", tables: tables.filter((table) => table.database === "SHOP") },
  { name: "HR", tables: tables.filter((table) => table.database === "HR") },
]);

const dialect: SqlDialectMock = {
  split: {},
  errors,
  version: `Oracle Database 23ai Free Release 23.0.0.0.0 - Version ${VERSION}`,
};

const FOLDERS: [string, string, string[]][] = [
  ["Tables", "TABLE", ["table"]],
  ["Views", "VIEW", ["view"]],
  ["Sequences", "SEQUENCE", ["sequence"]],
  ["Synonyms", "SYNONYM", ["synonym"]],
  ["Packages", "PACKAGE", ["package"]],
  ["Procedures", "PROCEDURE", ["procedure"]],
  ["Functions", "FUNCTION", ["function"]],
];

const copyName = builtin("copyName", "Copy name", "copyName");
const ICONS: Record<Other["kind"], string> = {
  sequence: "engine:oracle/sequence",
  synonym: "engine:oracle/synonym",
  package: "engine:oracle/package",
  procedure: "terminal",
  function: "terminal",
};

function otherNode(prefix: string, item: Other): ObjectNode {
  const object: ObjectRef = { database: item.database, schema: null, name: item.name, kind: item.kind };
  const plsql = item.kind === "package" || item.kind === "procedure" || item.kind === "function";
  return {
    key: { id: `${prefix}/${item.kind}:${item.name}`, kind: item.kind, object },
    label: item.name,
    kind: item.kind,
    icon: ICONS[item.kind],
    object,
    detail: item.detail,
    badges: item.invalid ? [{ text: "invalid", tone: "danger" }] : [],
    expandable: false,
    defaultView: "definition",
    actions: [
      viewAction(plsql ? "source" : "definition", plsql ? "Source" : "Definition", "definition"),
      copyName,
      ...(plsql ? [queryAction("compile", "Compile", `ALTER ${item.kind.toUpperCase()} {qualified} COMPILE`)] : []),
    ],
  };
}

function tableNodes(prefix: string, database: string, kinds: string[]): ObjectNode[] {
  return catalog
    .tree({ id: `db:${database}`, kind: "database", object: { database, schema: null, name: database, kind: "database" } })
    .filter((node) => kinds.includes(node.kind))
    .map((node) => ({ ...node, key: { ...node.key, id: `${prefix}/${node.kind}:${node.label}` } }));
}

export const tree = (parent: TreeRequest["parent"]): ObjectNode[] => {
  if (!parent) {
    return catalog.tree(null).map((node) => ({ ...node, icon: "engine:oracle/schema", detail: node.label === "SHOP" ? "login schema" : null }));
  }
  const [schemaPart, folderPart] = parent.id.split("/");
  const database = schemaPart.slice(3);
  if (parent.kind === "database") {
    return FOLDERS.flatMap(([label, type, kinds]) => {
      const count = tableNodes("", database, kinds).length + others.filter((o) => o.database === database && kinds.includes(o.kind)).length;
      if (count === 0) return [];
      const node: ObjectNode = {
        key: { id: `${schemaPart}/folder:${type}`, kind: "folder", object: null },
        label,
        kind: "folder",
        icon: "layers",
        object: null,
        detail: String(count),
        badges: [],
        expandable: true,
        defaultView: null,
        actions: [builtin("refresh", "Refresh", "refresh")],
      };
      return [node];
    });
  }
  if (parent.kind === "folder") {
    const kinds = FOLDERS.find(([, type]) => `folder:${type}` === folderPart)?.[2] ?? [];
    return [
      ...tableNodes(parent.id, database, kinds),
      ...others.filter((o) => o.database === database && kinds.includes(o.kind)).map((item) => otherNode(parent.id, item)),
    ];
  }
  return [];
};

export const describe = (object: ObjectRef): ObjectDescription => {
  const item = others.find((other) => other.kind === object.kind && other.name === object.name && other.database === object.database);
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

// --- Instant Client ---------------------------------------------------------------------------------------------------

const OFFER = {
  version: "23.26.4.0.0",
  size: 60_406_433,
  url: "https://download.oracle.com/otn_software/nt/instantclient/2326400/instantclient-basiclite-windows.x64-23.26.4.0.0.zip",
  licenseUrl: "https://www.oracle.com/downloads/licenses/instant-client-lic.html",
};
let clientInstalled = false;
let cancelDownload = false;

function clientStatus() {
  return {
    available: clientInstalled,
    source: clientInstalled ? "downloaded" : "none",
    version: clientInstalled ? OFFER.version : null,
    path: clientInstalled ? "C:\\Fosta Studio\\fosta-studio-oracle\\instantclient_23_26_4_0_0" : null,
    loaded: clientInstalled,
    download: OFFER,
  };
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

async function download(emit: (event: unknown) => void) {
  cancelDownload = false;
  const steps = 20;
  for (let step = 1; step <= steps; step += 1) {
    await sleep(120);
    if (cancelDownload) throw mockError("ORACLE_CLIENT", "The download was cancelled.", { engine: ENGINE, kind: "cancelled" });
    emit({ phase: "downloading", downloaded: Math.round((OFFER.size * step) / steps), total: OFFER.size });
  }
  emit({ phase: "verifying", downloaded: OFFER.size, total: OFFER.size });
  await sleep(300);
  emit({ phase: "extracting", downloaded: OFFER.size, total: OFFER.size });
  await sleep(300);
  emit({ phase: "done", downloaded: OFFER.size, total: OFFER.size });
  clientInstalled = true;
  return clientStatus();
}

/** The backend's `oracle_client_*` commands. */
export const commands: NonNullable<EngineMock["commands"]> = {
  oracle_client_status: () => clientStatus(),
  oracle_client_download: (_payload, emit) => download(emit),
  oracle_client_cancel: () => {
    cancelDownload = true;
    return null;
  },
};

export const connectInfo = (profile: ConnectionProfile): ConnectInfo => {
  if (!clientInstalled && !profile.options?.clientDir?.trim()) {
    throw mockError(
      "ORACLE_CLIENT",
      "Oracle Instant Client was not found. Let Fosta Studio download it, or install Instant Client 19c or newer (Basic or Basic Light) and set its folder in the connection's Advanced settings or on PATH. (DPI-1047: Cannot locate a 64-bit Oracle Client library)",
      { engine: ENGINE, kind: "notInstalled", download: OFFER },
    );
  }
  return {
    engine: ENGINE,
    server: { product: "Oracle", version: VERSION, label: `Oracle ${VERSION}`, flavor: "23ai" },
    capabilities: descriptor.capabilities,
    languages: descriptor.languages,
    connectionActions: [builtin("refresh", "Refresh", "refresh")],
  };
};

export const browse: typeof catalog.browse = (request) => catalog.browse(request);
export const apply: typeof catalog.apply = (changes) => catalog.apply(changes);
/** `DBMS_SESSION.SLEEP(n)` is the mock's `sleep(n)`, so Stop can be tried. */
export const run: NonNullable<EngineMock["run"]> = (request, emit, context) =>
  runSql(catalog, dialect, { ...request, text: request.text.replace(/DBMS_SESSION\.SLEEP\(/gi, "sleep(") }, emit, context);
export const classify = (text: string) => classifySql(text, dialect.split);
export const schemaGraph = (database: string) => catalog.schemaGraph(database);
export const completion = (database: string | null) => catalog.completion(database);
export const touchRow = (table: string, key: unknown, remove: boolean, column?: string) => catalog.touch(table, key, remove, column);
