/**
 * Browser-only stand-in for the Tauri backend, used by `npm run dev` when the
 * page is opened in a plain browser (no `__TAURI_INTERNALS__`). It lets the UI
 * be reviewed without a MariaDB server. Never bundled into the desktop build.
 */
import type { Channel } from "@tauri-apps/api/core";
import { errorText } from "../core/api";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { baseUrlOf, connectionSettings, endpointProblem, providerPreset } from "../features/assistant/aiProviders";
import { effectiveRules, presetSettings, PRODUCTION_ICON, relaxations, safetyOf } from "../features/safety/safety";
import type {
  AiConnection,
  AiSettings,
  AppConfig,
  ChatEvent,
  ChatRequest,
  Confirmation,
  ConnectionProfile,
  GatewayProblem,
  HistoryEntry,
  HistoryQuery,
  QueryOutcome,
  SafetyAction,
  StatementCategory,
  StatementInfo,
  TablePage,
  TransactionState,
  Workspace,
} from "../core/types";

const DIMS = 4;

const workspaces: Workspace[] = [
  {
    id: "local",
    name: "Local",
    color: "blue",
    icon: "laptop",
    safety: presetSettings("open"),
    connections: [
      {
        id: "c-local",
        name: "Local MariaDB",
        driver: "mariadb",
        host: "127.0.0.1",
        port: 3306,
        user: "root",
        database: "shop",
        savePassword: true,
        options: {},
      },
      {
        id: "c-local-mongo",
        name: "Local Mongo",
        driver: "mongodb",
        host: "127.0.0.1",
        port: 27017,
        user: "",
        database: "admin",
        savePassword: false,
        options: {},
      },
    ],
    tasks: [],
  },
  {
    id: "client-a",
    name: "Client A staging",
    color: "orange",
    icon: "cloud",
    // Guarded: run an UPDATE or DELETE in a query tab to see the confirmation dialog.
    safety: presetSettings("guarded"),
    connections: [
      {
        id: "c-a-1",
        name: "Staging (port-forward)",
        driver: "mariadb",
        host: "127.0.0.1",
        port: 13306,
        user: "app",
        database: "shop",
        savePassword: true,
        options: {},
      },
      {
        id: "c-a-2",
        name: "Analytics replica",
        driver: "mysql",
        host: "127.0.0.1",
        port: 13307,
        user: "readonly",
        database: null,
        savePassword: false,
        options: {},
      },
      {
        id: "c-a-3",
        name: "ERP (SQL Server)",
        driver: "mssql",
        host: "erp.client-a.local",
        port: 1433,
        user: "sa",
        database: "ERP",
        savePassword: true,
        options: { encrypt: "true" },
      },
      {
        id: "c-a-4",
        name: "Events (Mongo Atlas)",
        driver: "mongodb",
        host: "cluster0.example.net",
        port: 27017,
        user: "app",
        database: "admin",
        savePassword: true,
        options: {},
      },
    ],
    tasks: [
      {
        id: "t-a-1",
        name: "MariaDB port-forward",
        program: "kubectl",
        args: ["port-forward", "-n", "shop", "svc/mariadb", "13306:3306"],
        autoStart: true,
        autoRestart: true,
        readyPattern: "Forwarding from",
      },
    ],
  },
  {
    id: "prod",
    name: "Production",
    color: "red",
    icon: "shield",
    safety: presetSettings("readOnly"),
    connections: [
      {
        id: "c-prod",
        name: "Shop primary",
        driver: "mariadb",
        host: "db.prod.internal",
        port: 3306,
        user: "app",
        database: "shop",
        savePassword: true,
        options: {},
      },
    ],
    tasks: [],
  },
];

let config: AppConfig = { workspaces, activeWorkspace: "client-a", embedding: null, aiConnections: [], aiActive: null, queryHistory: true };

/** Query history for this page load only (the desktop app keeps it in fosta-studio-history.jsonl). */
let history: HistoryEntry[] = [];
let historySequence = 0;

const columns = [
  { name: "id", typeName: "int(11)", numeric: true, binary: false, primary: true, vectorDims: null },
  { name: "sku", typeName: "varchar(32)", numeric: false, binary: false, primary: false, vectorDims: null },
  { name: "name", typeName: "varchar(120)", numeric: false, binary: false, primary: false, vectorDims: null },
  { name: "price", typeName: "decimal(10,2)", numeric: true, binary: false, primary: false, vectorDims: null },
  { name: "stock", typeName: "int(11)", numeric: true, binary: false, primary: false, vectorDims: null },
  { name: "category_id", typeName: "int(11)", numeric: true, binary: false, primary: false, vectorDims: null },
  { name: "updated_at", typeName: "datetime", numeric: false, binary: false, primary: false, vectorDims: null },
  { name: "attributes", typeName: "longtext", numeric: false, binary: false, primary: false, vectorDims: null },
  { name: "embedding", typeName: "vector(4)", numeric: false, binary: true, primary: false, vectorDims: DIMS },
];

const COLORS = ["walnut", "oak", "steel", "linen", "glass", "brass", "wool", "pine"];

/** A JSON document per product, long enough to need the cell editor. */
function attributes(id: number): string {
  return JSON.stringify({
    color: COLORS[id % COLORS.length],
    dimensions: { width: 40 + (id % 9) * 10, depth: 30 + (id % 5) * 10, height: 45 + (id % 7) * 5, unit: "cm" },
    tags: ["furniture", id % 2 ? "indoor" : "outdoor", id % 3 ? "in-stock" : "backorder"],
    care: "Wipe with a dry cloth. Avoid direct sunlight and keep away from heat sources to prevent the finish from fading.",
  });
}

const names = ["Walnut desk", "Oak chair", "Steel lamp", "Linen sofa", "Glass table", "Brass hook", "Wool rug", "Pine shelf"];

function rows(offset: number, limit: number): (string | null)[][] {
  return Array.from({ length: limit }, (_, index) => {
    const id = offset + index + 1;
    return [
      String(id),
      `SKU-${String(id).padStart(5, "0")}`,
      `${names[id % names.length]} ${id}`,
      (19.5 + (id % 37) * 4.25).toFixed(2),
      id % 9 === 0 ? null : String((id * 7) % 120),
      String((id % 5) + 1),
      `2026-10-${String((id % 28) + 1).padStart(2, "0")} 09:${String(id % 60).padStart(2, "0")}:00`,
      id % 11 === 0 ? null : attributes(id),
      `[${(id % 10) / 10},${((id * 3) % 10) / 10},${((id * 7) % 10) / 10},${((id * 9) % 10) / 10}]`,
    ];
  });
}

function page(offset: number, limit: number, filter: string | null = null): TablePage {
  const all = rows(0, 1240);
  const predicate = compilePredicate(filter);
  const matching = predicate ? all.filter(predicate) : all;
  return { columns, rows: matching.slice(offset, offset + limit), total: matching.length, primaryKey: ["id"], elapsedMs: 12 };
}

const CONDITION = /^[`["]?(\w+)[`\]"]?\s*(IS NOT NULL|IS NULL|LIKE|<>|!=|>=|<=|=|>|<)\s*(.*)$/i;

/** Evaluates the SQL-ish WHERE text the UI builds, enough for the preview to feel real. */
function compilePredicate(filter: string | null): ((row: (string | null)[]) => boolean) | null {
  const text = filter?.trim();
  if (!text) return null;
  const parts = text.split(/\s+AND\s+/i).map((part) => part.replace(/^\(|\)$/g, "").trim());
  const checks = parts.flatMap((part) => {
    const match = CONDITION.exec(part);
    if (!match) return [];
    const index = columns.findIndex((column) => column.name === match[1]);
    if (index < 0) return [];
    const operator = match[2].toUpperCase();
    const operand = match[3].replace(/^N?'|'$/g, "").replace(/''/g, "'");
    return [(row: (string | null)[]) => compareCell(row[index], operator, operand)];
  });
  return (row) => checks.every((check) => check(row));
}

function compareCell(value: string | null, operator: string, operand: string): boolean {
  if (operator === "IS NULL") return value === null;
  if (operator === "IS NOT NULL") return value !== null;
  if (value === null) return false;
  if (operator === "LIKE") return value.toLowerCase().includes(operand.replace(/%/g, "").toLowerCase());
  const numeric = !Number.isNaN(Number(value)) && !Number.isNaN(Number(operand));
  const order = numeric ? Number(value) - Number(operand) : value.localeCompare(operand);
  switch (operator) {
    case "=":
      return order === 0;
    case "<>":
    case "!=":
      return order !== 0;
    case ">":
      return order > 0;
    case "<":
      return order < 0;
    case ">=":
      return order >= 0;
    default:
      return order <= 0;
  }
}

const col = (name: string, columnType: string, primary = false, nullable = false) => ({ name, columnType, primary, nullable });
const SHOP_GRAPH = {
  totalTables: 6,
  tables: [
    { name: "categories", kind: "table", rows: 8, comment: "", columns: [col("id", "int(11)", true), col("name", "varchar(60)"), col("slug", "varchar(60)")] },
    {
      name: "customers",
      kind: "table",
      rows: 2000,
      comment: "",
      columns: [
        col("id", "int(11)", true),
        col("full_name", "varchar(120)"),
        col("email", "varchar(160)"),
        col("city", "varchar(60)"),
        col("tier", "enum('bronze','silver','gold')"),
        col("created_at", "datetime"),
      ],
    },
    {
      name: "products",
      kind: "table",
      rows: 1240,
      comment: "",
      columns: [
        col("id", "int(11)", true),
        col("sku", "varchar(32)"),
        col("name", "varchar(120)"),
        col("category_id", "int(11)"),
        col("price", "decimal(10,2)"),
        col("stock", "int(11)", false, true),
        col("updated_at", "datetime"),
        col("attributes", "longtext", false, true),
        col("embedding", "vector(4)"),
      ],
    },
    {
      name: "orders",
      kind: "table",
      rows: 8000,
      comment: "",
      columns: [
        col("id", "int(11)", true),
        col("customer_id", "int(11)"),
        col("status", "enum('pending','paid','shipped','cancelled')"),
        col("ordered_at", "datetime"),
        col("total", "decimal(12,2)"),
      ],
    },
    {
      name: "order_items",
      kind: "table",
      rows: 16001,
      comment: "",
      columns: [
        col("id", "int(11)", true),
        col("order_id", "int(11)"),
        col("product_id", "int(11)"),
        col("quantity", "int(11)"),
        col("unit_price", "decimal(10,2)"),
      ],
    },
    {
      name: "v_daily_sales",
      kind: "view",
      rows: null,
      comment: "",
      columns: [col("day", "date", false, true), col("orders", "bigint(21)"), col("revenue", "decimal(34,2)", false, true)],
    },
  ],
  relations: [
    ["fk_orders_customer", "orders", "customer_id", "customers"],
    ["fk_items_order", "order_items", "order_id", "orders"],
    ["fk_items_product", "order_items", "product_id", "products"],
    ["fk_products_category", "products", "category_id", "categories"],
  ].map(([constraint, table, column, target]) => ({
    constraint,
    database: "shop",
    table,
    columns: [column],
    referencedDatabase: "shop",
    referencedTable: target,
    referencedColumns: ["id"],
  })),
};

function handle(command: string, payload: Record<string, unknown> = {}): unknown {
  switch (command) {
    case "plugin:event|listen":
      return 1;
    case "plugin:event|unlisten":
    case "plugin:window|show":
    case "plugin:window|set_title":
      return null;
    case "get_config":
      return config;
    case "set_active_workspace":
      config = { ...config, activeWorkspace: String(payload.id) };
      return config;
    case "save_workspace": {
      const meta = payload.workspace as Workspace;
      const existing = config.workspaces.find((item) => item.id === meta.id);
      const before = existing ? safetyOf(existing) : presetSettings(meta.icon === PRODUCTION_ICON ? "readOnly" : "guarded");
      const after = meta.safety ?? before;
      const production = existing?.icon === PRODUCTION_ICON || meta.icon === PRODUCTION_ICON;
      const name = existing?.name ?? meta.name;
      if (production && relaxations(before, after).length > 0 && String(payload.relaxConfirmation ?? "").trim() !== name.trim()) {
        throw Object.assign(new Error(`Relaxing the rules of the Production workspace needs its name typed: "${name}".`), {
          code: "RELAX_CONFIRMATION_REQUIRED",
          expected: name,
        });
      }
      config = existing
        ? {
            ...config,
            workspaces: config.workspaces.map((item) =>
              item.id === meta.id ? { ...item, name: meta.name, color: meta.color, icon: meta.icon, safety: after } : item,
            ),
          }
        : { ...config, workspaces: [...config.workspaces, { ...meta, safety: after, connections: [], tasks: [] }] };
      return config;
    }
    case "delete_workspace":
      config = { ...config, workspaces: config.workspaces.filter((item) => item.id !== payload.id) };
      if (config.activeWorkspace === payload.id) config.activeWorkspace = config.workspaces[0].id;
      return config;
    case "save_connection":
    case "delete_connection":
    case "save_task":
    case "delete_task":
    case "save_embedding_settings":
      return config;
    case "save_ai_connection":
      return saveAiConnection(payload.connection as AiConnection, (payload.apiKey as string | null) ?? null);
    case "delete_ai_connection":
      return deleteAiConnection(String(payload.id));
    case "set_ai_active":
      return setAiActive(String(payload.connectionId), String(payload.model ?? ""));
    case "test_ai":
      return probeChat(payload.settings as AiSettings, draftKey(payload));
    case "list_ai_models":
      return fetchModels(payload.settings as AiSettings, draftKey(payload));
    case "ai_chat":
      return streamChat(payload.request as ChatRequest, payload.onEvent as Channel<ChatEvent>);
    case "ai_cancel":
      chatRuns.get(String(payload.id))?.abort();
      return null;
    case "test_connection":
    case "connect": {
      const profile =
        (payload.profile as ConnectionProfile | undefined) ?? config.workspaces.flatMap((item) => item.connections).find((item) => item.id === payload.id);
      const versions: Record<string, string> = { mariadb: "11.8.2-MariaDB", mysql: "8.4.3", mssql: "16.0.1000.6 (Developer Edition)", mongodb: "8.0.4" };
      return versions[profile?.driver ?? "mariadb"];
    }
    case "open_session":
      sessions.set(String(payload.sessionId), { connectionId: String(payload.connectionId), open: false, autoCommit: true });
      return null;
    case "sql_transaction":
      return transactionControl(String(payload.sessionId), String(payload.op));
    case "disconnect":
    case "close_session":
    case "cancel_sql":
    case "start_task":
    case "stop_task":
    case "restart_task":
      return null;
    case "save_download":
      return ["C:", "Users", "you", "Downloads", String(payload.name)].join("\\");
    case "reveal_file":
      return null;
    case "capabilities":
      return {
        edition: "desktop",
        tasks: true,
        revealFile: true,
        browserDownloads: false,
        secretStore: "Windows Credential Manager",
        inContainer: false,
      };
    case "create_database":
      gate(String(payload.connectionId), "createDatabase", [statement("ddl", "CREATE DATABASE", [], `CREATE DATABASE ${String(payload.name)}`)], payload);
      return null;
    case "schema_graph":
      return SHOP_GRAPH;
    case "list_databases":
      return ["shop", "analytics", "information_schema", "mysql", "performance_schema", "sys"];
    case "list_tables":
      return [
        { name: "products", kind: "table", rows: 1240, comment: "" },
        { name: "categories", kind: "table", rows: 12, comment: "" },
        { name: "orders", kind: "table", rows: 48210, comment: "" },
        { name: "order_items", kind: "table", rows: 153900, comment: "" },
        { name: "customers", kind: "table", rows: 9031, comment: "" },
        { name: "v_daily_sales", kind: "view", rows: null, comment: "" },
      ];
    case "browse_table": {
      const request = payload.request as { offset: number; limit: number; filter: string | null };
      return page(request.offset, request.limit, request.filter);
    }
    case "describe_table":
      return {
        columns: columns.map((column) => ({
          name: column.name,
          columnType: column.typeName,
          nullable: !column.primary,
          key: column.primary ? "PRI" : "",
          defaultValue: null,
          extra: column.primary ? "auto_increment" : "",
          comment: "",
        })),
        ddl: "CREATE TABLE `products` (\n  `id` int(11) NOT NULL AUTO_INCREMENT,\n  `sku` varchar(32) NOT NULL,\n  `name` varchar(120) NOT NULL,\n  `price` decimal(10,2) NOT NULL,\n  `stock` int(11) DEFAULT NULL,\n  `category_id` int(11) NOT NULL,\n  `updated_at` datetime NOT NULL,\n  `attributes` longtext DEFAULT NULL CHECK (json_valid(`attributes`)),\n  `embedding` vector(4) NOT NULL,\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB",
      };
    case "run_sql":
      return runSql(String(payload.sessionId ?? ""), String(payload.sql ?? ""), payload);
    case "history_append":
      return appendHistory(payload.entry as HistoryEntry);
    case "history_list":
      return listHistory(payload.query as HistoryQuery);
    case "history_delete": {
      const ids = new Set(payload.ids as string[]);
      const before = history.length;
      history = history.filter((entry) => !ids.has(entry.id));
      return before - history.length;
    }
    case "history_clear":
      history = [];
      return null;
    case "set_query_history":
      config = { ...config, queryHistory: Boolean(payload.enabled) };
      return config;
    case "apply_changes": {
      const changes = payload.changes as { kind: string }[];
      const table = String(payload.table);
      gate(
        String(payload.connectionId),
        payload.caller === "import" ? "import" : "gridSave",
        [statement("write", "CHANGES", [table], `${changes.length} change(s) to ${table}, each matched by its key`)],
        payload,
      );
      return changes.length;
    }
    case "task_statuses":
      return [{ id: "t-a-1", state: "running", pid: 4242, restarts: 0, message: null }];
    case "task_logs":
      return ["▶ kubectl port-forward -n shop svc/mariadb 13306:3306", "Forwarding from 127.0.0.1:13306 -> 3306", "Handling connection for 13306"];
    case "table_relations":
      return {
        outgoing: [
          {
            constraint: "fk_products_category",
            database: "shop",
            table: "products",
            columns: ["category_id"],
            referencedDatabase: "shop",
            referencedTable: "categories",
            referencedColumns: ["id"],
          },
        ],
        incoming: [
          {
            constraint: "fk_items_product",
            database: "shop",
            table: "order_items",
            columns: ["product_id"],
            referencedDatabase: "shop",
            referencedTable: "products",
            referencedColumns: ["id"],
          },
        ],
      };
    case "vector_info":
      return {
        supported: true,
        columns: [{ name: "embedding", dims: DIMS }],
        indexes: [{ name: "idx_embedding", column: "embedding", metric: "cosine", m: 6 }],
      };
    case "fetch_vector":
      return [0.12, -0.4, 0.33, 0.9];
    case "vector_search": {
      const request = payload.request as { k: number; filter: string | null };
      const result = page(0, request.k, request.filter);
      const distance = { name: "distance", typeName: "double", numeric: true, binary: false, primary: false, vectorDims: null };
      const sql = "SELECT *, VEC_DISTANCE_COSINE(`embedding`, ?) AS distance FROM `products` ORDER BY distance LIMIT ?";
      return {
        result: {
          columns: [...result.columns, distance],
          rows: result.rows.map((row, index) => [...row, (0.05 + index * 0.031).toFixed(4)]),
          affectedRows: 0,
          truncated: false,
        },
        plan: {
          columns: [
            { ...distance, name: "type", typeName: "varchar", numeric: false },
            { ...distance, name: "key", typeName: "varchar", numeric: false },
          ],
          rows: [["range", "idx_embedding"]],
          affectedRows: 0,
          truncated: false,
        },
        indexUsed: "idx_embedding",
        sql,
        runnableSql: sql.replace("?", "VEC_FromText('[0.12,-0.4,0.33,0.9]')").replace("?", String(request.k)),
        elapsedMs: 4,
        queryDims: DIMS,
        primaryKey: ["id"],
      };
    }
    case "vector_health":
      return [
        { label: "Dimensions", status: "ok", value: String(DIMS), detail: "Every sampled row has 4 values" },
        { label: "NULL vectors", status: "ok", value: "0", detail: "No row is missing a vector" },
        { label: "Zero vectors", status: "warn", value: "3", detail: "Cosine distance is undefined for all-zero vectors" },
        { label: "Vector index", status: "info", value: "idx_embedding", detail: "cosine, M=6" },
      ];
    case "index_recall":
      return {
        index: "idx_embedding",
        metric: "cosine",
        k: Number(payload.k),
        queries: Number(payload.queries),
        recall: 0.97,
        worstRecall: 0.8,
        annMs: 1.4,
        exactMs: 9.8,
      };
    case "test_embedding":
      return DIMS;
    default:
      throw new Error(`devMock: no handler for ${command}`);
  }
}

// --- Query tab and history ---------------------------------------------------

// --- Statement gateway (a rough stand-in for gateway/ in the backend) ---------

interface MockSession {
  connectionId: string;
  open: boolean;
  autoCommit: boolean;
}

const sessions = new Map<string, MockSession>();
const STRICTNESS: Record<SafetyAction, number> = { allow: 0, confirm: 1, typed: 2, block: 3 };

function statement(category: StatementCategory, kind: string, tables: string[], excerpt: string, reason: string | null = null): StatementInfo {
  return { category, kind, tables, excerpt, reason };
}

/** Keyword classification, enough to show the dialogs in the preview (the backend parses SQL properly). */
function classify(text: string): StatementInfo {
  const sql = text.trim();
  const table = /(?:from|into|update|table)\s+[`"[]?([\w.]+)/i.exec(sql)?.[1] ?? "";
  const tables = table ? [table] : [];
  if (/^(begin|start|commit|rollback|savepoint)\b/i.test(sql)) return statement("transaction", sql.split(/\s+/)[0].toUpperCase(), [], sql);
  if (/^(grant|revoke|kill|create\s+user|set\s+global)\b/i.test(sql)) return statement("admin", sql.split(/\s+/)[0].toUpperCase(), tables, sql);
  if (/^(drop|truncate)\b/i.test(sql)) return statement("destructive", sql.split(/\s+/)[0].toUpperCase(), tables, sql, "Removes the object and its data");
  if (/^(delete|update)\b/i.test(sql) && !/\bwhere\b/i.test(sql))
    return statement("destructive", sql.split(/\s+/)[0].toUpperCase(), tables, sql, `${sql.split(/\s+/)[0].toUpperCase()} without WHERE affects every row`);
  if (/^(insert|update|delete|replace|merge)\b/i.test(sql)) return statement("write", sql.split(/\s+/)[0].toUpperCase(), tables, sql);
  if (/^(create|alter|rename)\b/i.test(sql)) return statement("ddl", sql.split(/\s+/)[0].toUpperCase(), tables, sql);
  return statement("read", sql.split(/\s+/)[0].toUpperCase(), tables, sql);
}

/** Applies the workspace's rules like the backend: throws BLOCKED or CONFIRMATION_REQUIRED (any token is accepted back). */
function gate(connectionId: string, caller: GatewayProblem["caller"], statements: StatementInfo[], payload: Record<string, unknown>): void {
  const workspace = config.workspaces.find((item) => item.connections.some((profile) => profile.id === connectionId));
  if (!workspace) return;
  const settings = safetyOf(workspace);
  const rules = effectiveRules(settings).rules;
  const actionOf = (category: StatementCategory): SafetyAction =>
    category === "transaction" ? "allow" : category === "unknown" ? rules.write : rules[category as keyof typeof rules];
  const level = statements
    .map((item) => actionOf(item.category))
    .reduce<SafetyAction>((worst, next) => (STRICTNESS[next] > STRICTNESS[worst] ? next : worst), "allow");
  if (level === "allow") return;
  const confirmation = payload.confirmation as Confirmation | null | undefined;
  if (level !== "block" && confirmation) return;
  const tables = [...new Set(statements.flatMap((item) => item.tables))];
  const categories = [...new Set(statements.map((item) => item.category))];
  const expected = level === "typed" ? (tables.length === 1 ? (tables[0].split(".").pop() ?? workspace.name) : workspace.name) : null;
  const message =
    level === "block"
      ? `Blocked: these statements are not allowed by the rules of workspace ${workspace.name}.`
      : `These statements need a ${level === "typed" ? "typed " : ""}confirmation in workspace ${workspace.name}.`;
  const problem: GatewayProblem = {
    code: level === "block" ? "BLOCKED" : "CONFIRMATION_REQUIRED",
    message,
    caller,
    workspace: workspace.name,
    connection: workspace.connections.find((profile) => profile.id === connectionId)?.name ?? connectionId,
    preset: settings.preset,
    level,
    token: level === "block" ? null : `mock-${Date.now().toString(16)}`,
    expected,
    categories,
    tables,
    statements,
  };
  throw Object.assign(new Error(message), problem);
}

function transactionState(session: MockSession | undefined): TransactionState {
  return { supported: true, open: session?.open ?? false, autoCommit: session?.autoCommit ?? true };
}

function transactionControl(sessionId: string, op: string): TransactionState {
  const session = sessions.get(sessionId);
  if (!session) throw new Error("This query tab lost its session. Close it and open a new query.");
  if (op === "begin" && session.open) throw new Error("A transaction is already open in this tab. Commit or roll it back before starting a new one.");
  if (op === "begin") session.open = true;
  if (op === "commit" || op === "rollback" || op === "autoCommitOn") session.open = false;
  if (op === "autoCommitOn") session.autoCommit = true;
  if (op === "autoCommitOff") session.autoCommit = false;
  return transactionState(session);
}

/** Statements the preview pretends to understand; anything else fails like a syntax error. */
async function runSql(sessionId: string, sql: string, payload: Record<string, unknown>): Promise<QueryOutcome> {
  const statements = sql
    .split(";")
    .map((statement) => statement.trim())
    .filter(Boolean);
  const session = sessions.get(sessionId);
  if (session && !statements[0]?.startsWith("{")) gate(session.connectionId, "queryTab", statements.map(classify), payload);
  await new Promise((resolve) => setTimeout(resolve, 120));
  const sets = statements.map((statement) => {
    if (/^(update|delete|insert|replace|create|drop|alter|set|begin|commit|rollback|use)\b/i.test(statement)) {
      return { columns: [], rows: [], affectedRows: /^(update|delete|insert|replace)/i.test(statement) ? 3 : 0, truncated: false };
    }
    if (/^(select|show|with|describe|desc|explain)\b/i.test(statement) || statement.startsWith("{")) {
      const result = page(0, 25);
      return { columns: result.columns, rows: result.rows, affectedRows: 0, truncated: false };
    }
    const near = statement.slice(0, 30);
    throw new Error(
      `You have an error in your SQL syntax; check the manual that corresponds to your MariaDB server version for the right syntax to use near '${near}' at line 1`,
    );
  });
  if (session) {
    for (const text of statements) {
      if (/^(begin|start)\b/i.test(text)) session.open = true;
      else if (/^(commit|rollback)\b/i.test(text)) session.open = false;
      else if (!session.autoCommit && !/^(select|show)\b/i.test(text)) session.open = true;
    }
  }
  const mongo = session && config.workspaces.flatMap((item) => item.connections).find((profile) => profile.id === session.connectionId)?.driver === "mongodb";
  return { sets, elapsedMs: 8, transaction: mongo ? { supported: false, open: false, autoCommit: true } : transactionState(session) };
}

function appendHistory(entry: HistoryEntry): HistoryEntry | null {
  if (!config.queryHistory) return null;
  historySequence += 1;
  const stored = { ...entry, id: `mock-${Date.now().toString(16)}-${historySequence}` };
  history = [...history, stored].slice(-5000);
  return stored;
}

function listHistory(query: HistoryQuery): HistoryEntry[] {
  const search = query.search?.trim().toLowerCase();
  return history
    .filter((entry) => !query.connectionId || entry.connectionId === query.connectionId)
    .filter((entry) => query.before === undefined || entry.startedAt < query.before)
    .filter((entry) => !search || [entry.sql, entry.connectionName, entry.database ?? ""].some((text) => text.toLowerCase().includes(search)))
    .reverse()
    .sort((a, b) => b.startedAt - a.startedAt)
    .slice(0, query.limit || 200);
}

// --- Assistant: real provider calls from the browser ------------------------
// The preview has no Rust backend, so the dialog's key is kept in memory for
// this page load and requests go straight to the provider from the browser.
// Schema lookups (tools) are desktop-only; here the model gets the table list.

/** Keys per LLM connection id, for this page load only. */
const browserKeys = new Map<string, string>();
const chatRuns = new Map<string, AbortController>();

/** Mirrors the backend: the active connection must exist; else the first one with its default model. */
function repairActive(next: AppConfig): AppConfig {
  const valid = next.aiConnections.some((item) => item.id === next.aiActive?.connectionId);
  if (valid) return next;
  const first = next.aiConnections[0];
  return { ...next, aiActive: first ? { connectionId: first.id, model: first.defaultModel } : null };
}

function saveAiConnection(connection: AiConnection, apiKey: string | null): AppConfig {
  const item = { ...connection, name: connection.name.trim(), defaultModel: connection.defaultModel.trim(), baseUrl: connection.baseUrl.trim() };
  if (!item.name) throw new Error("Name is required");
  if (!item.defaultModel) throw new Error("Default model is required");
  const problem = endpointProblem(item.baseUrl, item.provider);
  if (problem) throw new Error(problem);
  if (apiKey) browserKeys.set(item.id, apiKey);
  const previous = config.aiConnections.find((existing) => existing.id === item.id);
  const aiConnections = previous ? config.aiConnections.map((existing) => (existing.id === item.id ? item : existing)) : [...config.aiConnections, item];
  let aiActive = config.aiActive;
  const moved = previous && (previous.provider !== item.provider || previous.baseUrl !== item.baseUrl);
  if (previous && aiActive?.connectionId === item.id && (moved || aiActive.model === previous.defaultModel))
    aiActive = { ...aiActive, model: item.defaultModel };
  config = repairActive({ ...config, aiConnections, aiActive });
  return config;
}

function deleteAiConnection(id: string): AppConfig {
  browserKeys.delete(id);
  config = repairActive({ ...config, aiConnections: config.aiConnections.filter((item) => item.id !== id) });
  return config;
}

function setAiActive(connectionId: string, model: string): AppConfig {
  if (!model.trim()) throw new Error("Model is required");
  if (!config.aiConnections.some((item) => item.id === connectionId)) throw new Error(`LLM provider '${connectionId}' no longer exists`);
  config = { ...config, aiActive: { connectionId, model: model.trim() } };
  return config;
}

/** The typed key, else the one kept for `connectionId`. */
function draftKey(payload: Record<string, unknown>): string {
  const typed = typeof payload.apiKey === "string" ? payload.apiKey : "";
  return typed || browserKeys.get(String(payload.connectionId ?? "")) || "";
}

/** Settings and key for a chat request, like the backend's `chat_settings`. */
function chatSettings(request: ChatRequest): { settings: AiSettings; apiKey: string } {
  const connectionId = request.connectionId ?? config.aiActive?.connectionId;
  if (!connectionId) throw new Error("Add an LLM provider first (Assistant → Set up…)");
  const connection = config.aiConnections.find((item) => item.id === connectionId);
  if (!connection) throw new Error(`LLM provider '${connectionId}' no longer exists`);
  const model = request.model?.trim() || (config.aiActive?.connectionId === connection.id ? config.aiActive.model : connection.defaultModel);
  return { settings: connectionSettings(connection, model), apiKey: browserKeys.get(connection.id) ?? "" };
}

function providerHeaders(settings: AiSettings, apiKey: string): Record<string, string> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (providerPreset(settings.provider).wire === "anthropic") {
    headers["x-api-key"] = apiKey;
    headers["anthropic-version"] = "2026-10-21";
    headers["anthropic-dangerous-direct-browser-access"] = "true";
  } else if (apiKey) {
    headers.Authorization = `Bearer ${apiKey}`;
  }
  return headers;
}

async function failureText(response: Response): Promise<string> {
  const body = await response.text();
  try {
    const parsed = JSON.parse(body) as { error?: { message?: string } | string; message?: string };
    const message = typeof parsed.error === "string" ? parsed.error : (parsed.error?.message ?? parsed.message);
    if (message) return `${response.status}: ${message}`;
  } catch {
    // Not JSON; fall through to the raw preview.
  }
  return `${response.status}: ${body.slice(0, 300)}`;
}

/** Parses JSON, naming the URL when the server answered with something else (a login page, HTML). */
async function jsonBody(response: Response, url: string): Promise<unknown> {
  const text = await response.text();
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new Error(`${url} did not return JSON (got "${text.slice(0, 40).replace(/\s+/g, " ")}…"). Check the endpoint URL.`);
  }
}

/** fetch() from a page fails without detail when the provider blocks cross-origin calls; say so. */
async function providerFetch(url: string, init: RequestInit): Promise<Response> {
  try {
    return await fetch(url, init);
  } catch (error) {
    throw new Error(
      `The browser preview could not reach ${new URL(url).host} (${errorText(error)}). The provider probably blocks cross-origin requests; the desktop app calls it from Rust and is not affected.`,
    );
  }
}

async function fetchModels(settings: AiSettings, key: string): Promise<string[]> {
  const problem = endpointProblem(settings.baseUrl, settings.provider);
  if (problem) throw new Error(problem);
  const anthropic = providerPreset(settings.provider).wire === "anthropic";
  const url = `${baseUrlOf(settings)}${anthropic ? "/v1/models?limit=1000" : "/models"}`;
  const response = await providerFetch(url, { headers: providerHeaders(settings, key) });
  if (!response.ok) throw new Error(`${providerPreset(settings.provider).label} API returned ${await failureText(response)}`);
  const payload = (await jsonBody(response, url)) as { data?: unknown; models?: unknown } | unknown[];
  // Same shapes the backend accepts: { data: [...] } (OpenAI, Anthropic), { models: [...] } (Ollama native), or a bare array.
  const items = Array.isArray(payload) ? payload : ((payload as { data?: unknown }).data ?? (payload as { models?: unknown }).models);
  if (!Array.isArray(items)) throw new Error(`${url} returned no model list`);
  const ids = items
    .map((item) => (typeof item === "string" ? item : String((item as { id?: string; name?: string }).id ?? (item as { name?: string }).name ?? "")))
    .map((id) => id.replace(/^models\//, ""))
    .filter(Boolean);
  return [...new Set(ids)].sort();
}

function systemPrompt(context: ChatRequest["context"]): string {
  const lines = [
    "You are the assistant built into Fosta Studio, a desktop database client. You cannot run anything: put every runnable query in a fenced code block (```sql, or ```json for MongoDB commands) for the user to run from the editor. Answer in the user's language and keep prose short.",
    "Context:",
    context.connectionId
      ? `- Connection: ${context.connectionName ?? "connection"} (${context.driverLabel ?? "unknown"} ${context.serverVersion ?? ""})`
      : "- No connection is open; write generic SQL and say which engine you assumed.",
  ];
  if (context.database)
    lines.push(
      `- Database: ${context.database}`,
      "- Tables: products (~1240 rows), categories (12), orders (48210), order_items (153900), customers (9031), v_daily_sales (view)",
    );
  if (context.table) lines.push(`- Table the user is looking at: ${context.table}`);
  if (context.editorText) lines.push("- Current editor text:", "```", context.editorText, "```");
  if (context.lastError) lines.push(`- Last error from the server: ${context.lastError}`);
  if (context.extra) lines.push(`- ${context.extra}`);
  return lines.join("\n");
}

/** Streams the provider's reply through the channel the UI passed, like the backend does. */
async function streamChat(request: ChatRequest, channel: Channel<ChatEvent>): Promise<string> {
  const { settings, apiKey } = chatSettings(request);
  const internals = (window as unknown as { __TAURI_INTERNALS__: { runCallback(id: number, data: unknown): void } }).__TAURI_INTERNALS__;
  let index = 0;
  const emit = (message: ChatEvent) => internals.runCallback(channel.id, { index: index++, message });
  const preset = providerPreset(settings.provider);
  const anthropic = preset.wire === "anthropic";
  const system = systemPrompt(request.context);
  const messages = request.messages.map((message) => ({ role: message.role, content: message.content }));
  const body = anthropic
    ? { model: settings.model, max_tokens: 8192, stream: true, system, messages }
    : { model: settings.model, stream: true, messages: [{ role: "system", content: system }, ...messages] };
  const controller = new AbortController();
  chatRuns.set(request.id, controller);
  let text = "";
  try {
    const response = await providerFetch(`${baseUrlOf(settings)}${anthropic ? "/v1/messages" : "/chat/completions"}`, {
      method: "POST",
      headers: providerHeaders(settings, apiKey),
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (!response.ok || !response.body) throw new Error(`${preset.label} API returned ${await failureText(response)}`);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        const payload = line.replace(/\r$/, "").replace(/^data:\s*/, "");
        if (!line.startsWith("data:") || !payload || payload === "[DONE]") continue;
        const event = JSON.parse(payload) as {
          type?: string;
          delta?: { type?: string; text?: string };
          choices?: { delta?: { content?: string } }[];
          error?: { message?: string };
        };
        if (event.error?.message) throw new Error(`${preset.label} API: ${event.error.message}`);
        const piece = anthropic
          ? event.type === "content_block_delta" && event.delta?.type === "text_delta"
            ? event.delta.text
            : undefined
          : event.choices?.[0]?.delta?.content;
        if (piece) {
          text += piece;
          emit({ type: "text", text: piece });
        }
      }
    }
    return text;
  } catch (error) {
    if (controller.signal.aborted) throw new Error("Stopped");
    throw error;
  } finally {
    chatRuns.delete(request.id);
    emit({ type: "done" });
  }
}

/** "Save & test": one short exchange without streaming. */
async function probeChat(settings: AiSettings, apiKey: string): Promise<string> {
  const problem = endpointProblem(settings.baseUrl, settings.provider);
  if (problem) throw new Error(problem);
  const preset = providerPreset(settings.provider);
  const anthropic = preset.wire === "anthropic";
  const prompt = { role: "user", content: "Reply with the single word OK." };
  const body = anthropic ? { model: settings.model, max_tokens: 64, messages: [prompt] } : { model: settings.model, messages: [prompt] };
  const response = await providerFetch(`${baseUrlOf(settings)}${anthropic ? "/v1/messages" : "/chat/completions"}`, {
    method: "POST",
    headers: providerHeaders(settings, apiKey),
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error(`${preset.label} API returned ${await failureText(response)}`);
  const payload = (await jsonBody(response, baseUrlOf(settings))) as { content?: { text?: string }[]; choices?: { message?: { content?: string } }[] };
  return (anthropic ? payload.content?.map((block) => block.text ?? "").join("") : payload.choices?.[0]?.message?.content)?.trim() || "(empty reply)";
}

export function installDevMock(): void {
  mockWindows("main");
  mockIPC((command, payload) => handle(command, payload as Record<string, unknown>));
}
