/**
 * Browser-only stand-in for the Tauri backend, used by `npm run dev` when the
 * page is opened in a plain browser (no `__TAURI_INTERNALS__`). It lets the UI
 * be reviewed without a MariaDB server. Never bundled into the desktop build.
 */
import type { Channel } from "@tauri-apps/api/core";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { AppConfig, ChatEvent, ChatRequest, ConnectionProfile, TablePage, Workspace } from "./types";

const DIMS = 4;

const workspaces: Workspace[] = [
  {
    id: "local",
    name: "Local",
    color: "blue",
    icon: "laptop",
    connections: [
      { id: "c-local", name: "Local MariaDB", driver: "mariadb", host: "127.0.0.1", port: 3306, user: "root", database: "shop", savePassword: true, options: {} },
      { id: "c-local-mongo", name: "Local Mongo", driver: "mongodb", host: "127.0.0.1", port: 27017, user: "", database: "admin", savePassword: false, options: {} },
    ],
    tasks: [],
  },
  {
    id: "client-a",
    name: "Client A staging",
    color: "orange",
    icon: "cloud",
    connections: [
      { id: "c-a-1", name: "Staging (port-forward)", driver: "mariadb", host: "127.0.0.1", port: 13306, user: "app", database: "shop", savePassword: true, options: {} },
      { id: "c-a-2", name: "Analytics replica", driver: "mysql", host: "127.0.0.1", port: 13307, user: "readonly", database: null, savePassword: false, options: {} },
      { id: "c-a-3", name: "ERP (SQL Server)", driver: "mssql", host: "erp.client-a.local", port: 1433, user: "sa", database: "ERP", savePassword: true, options: { encrypt: "true" } },
      { id: "c-a-4", name: "Events (Mongo Atlas)", driver: "mongodb", host: "cluster0.example.net", port: 27017, user: "app", database: "admin", savePassword: true, options: {} },
    ],
    tasks: [
      { id: "t-a-1", name: "MariaDB port-forward", program: "kubectl", args: ["port-forward", "-n", "shop", "svc/mariadb", "13306:3306"], autoStart: true, autoRestart: true, readyPattern: "Forwarding from" },
    ],
  },
  { id: "prod", name: "Production", color: "red", icon: "shield", connections: [], tasks: [] },
];

let config: AppConfig = { workspaces, activeWorkspace: "client-a", embedding: null, ai: { provider: "anthropic", baseUrl: "", model: "claude-opus-5-5" } };

const columns = [
  { name: "id", typeName: "int(11)", numeric: true, binary: false, primary: true, vectorDims: null },
  { name: "sku", typeName: "varchar(32)", numeric: false, binary: false, primary: false, vectorDims: null },
  { name: "name", typeName: "varchar(120)", numeric: false, binary: false, primary: false, vectorDims: null },
  { name: "price", typeName: "decimal(10,2)", numeric: true, binary: false, primary: false, vectorDims: null },
  { name: "stock", typeName: "int(11)", numeric: true, binary: false, primary: false, vectorDims: null },
  { name: "category_id", typeName: "int(11)", numeric: true, binary: false, primary: false, vectorDims: null },
  { name: "updated_at", typeName: "datetime", numeric: false, binary: false, primary: false, vectorDims: null },
  { name: "embedding", typeName: "vector(4)", numeric: false, binary: true, primary: false, vectorDims: DIMS },
];

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

const CONDITION = /^`?(\w+)`?\s*(IS NOT NULL|IS NULL|LIKE|<>|!=|>=|<=|=|>|<)\s*(.*)$/i;

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
    const operand = match[3].replace(/^'|'$/g, "").replace(/''/g, "'");
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
    case "=": return order === 0;
    case "<>": case "!=": return order !== 0;
    case ">": return order > 0;
    case "<": return order < 0;
    case ">=": return order >= 0;
    default: return order <= 0;
  }
}

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
      config = existing
        ? { ...config, workspaces: config.workspaces.map((item) => (item.id === meta.id ? { ...item, name: meta.name, color: meta.color, icon: meta.icon } : item)) }
        : { ...config, workspaces: [...config.workspaces, { ...meta, connections: [], tasks: [] }] };
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
    case "save_ai_settings":
      config = { ...config, ai: (payload.settings as AppConfig["ai"]) ?? null };
      return config;
    case "test_ai":
      return "OK";
    case "ai_chat":
      return mockChat(payload.request as ChatRequest, payload.onEvent as Channel<ChatEvent>);
    case "ai_cancel":
      return null;
    case "test_connection":
    case "connect": {
      const profile = (payload.profile as ConnectionProfile | undefined) ?? config.workspaces.flatMap((item) => item.connections).find((item) => item.id === payload.id);
      const versions: Record<string, string> = { mariadb: "11.8.2-MariaDB", mysql: "8.4.3", mssql: "16.0.1000.6 (Developer Edition)", mongodb: "8.0.4" };
      return versions[profile?.driver ?? "mariadb"];
    }
    case "disconnect":
    case "open_session":
    case "close_session":
    case "cancel_sql":
    case "start_task":
    case "stop_task":
    case "restart_task":
      return null;
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
        columns: columns.map((column) => ({ name: column.name, columnType: column.typeName, nullable: !column.primary, key: column.primary ? "PRI" : "", defaultValue: null, extra: column.primary ? "auto_increment" : "", comment: "" })),
        ddl: "CREATE TABLE `products` (\n  `id` int(11) NOT NULL AUTO_INCREMENT,\n  `sku` varchar(32) NOT NULL,\n  `name` varchar(120) NOT NULL,\n  `price` decimal(10,2) NOT NULL,\n  `stock` int(11) DEFAULT NULL,\n  `category_id` int(11) NOT NULL,\n  `updated_at` datetime NOT NULL,\n  `embedding` vector(4) NOT NULL,\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB",
      };
    case "run_sql": {
      const result = page(0, 25);
      return { sets: [{ columns: result.columns, rows: result.rows, affectedRows: 0, truncated: false }], elapsedMs: 8 };
    }
    case "apply_changes":
      return 1;
    case "task_statuses":
      return [{ id: "t-a-1", state: "running", pid: 4242, restarts: 0, message: null }];
    case "task_logs":
      return ["▶ kubectl port-forward -n shop svc/mariadb 13306:3306", "Forwarding from 127.0.0.1:13306 -> 3306", "Handling connection for 13306"];
    case "table_relations":
      return {
        outgoing: [{ constraint: "fk_products_category", database: "shop", table: "products", columns: ["category_id"], referencedDatabase: "shop", referencedTable: "categories", referencedColumns: ["id"] }],
        incoming: [{ constraint: "fk_items_product", database: "shop", table: "order_items", columns: ["product_id"], referencedDatabase: "shop", referencedTable: "products", referencedColumns: ["id"] }],
      };
    case "vector_info":
      return { supported: true, columns: [{ name: "embedding", dims: DIMS }], indexes: [{ name: "idx_embedding", column: "embedding", metric: "cosine", m: 6 }] };
    case "fetch_vector":
      return [0.12, -0.4, 0.33, 0.9];
    default:
      throw new Error(`devMock: no handler for ${command}`);
  }
}

const SQL_REPLY = `Dựa trên bảng \`products\` (đã xem cấu trúc), đây là query:

\`\`\`sql
SELECT p.id, p.name, p.price, p.stock, c.name AS category
FROM products p
JOIN categories c ON c.id = p.category_id
WHERE p.stock < 20
ORDER BY p.stock ASC
LIMIT 50;
\`\`\`

- \`stock\` có thể NULL với vài dòng; nếu muốn tính cả những dòng đó, thêm \`OR p.stock IS NULL\`.
- Có index trên \`category_id\` nên JOIN rẻ.`;

const MONGO_REPLY = `Pipeline theo category, tính tồn kho và giá trung bình:

\`\`\`json
{
  "aggregate": "products",
  "pipeline": [
    { "$match": { "stock": { "$gt": 0 } } },
    { "$group": { "_id": "$category_id", "items": { "$sum": 1 }, "avgPrice": { "$avg": "$price" } } },
    { "$sort": { "items": -1 } }
  ],
  "cursor": {}
}
\`\`\`

Chạy trực tiếp trong tab Query, hoặc chèn vào editor rồi sửa \`$match\`.`;

/** Streams a canned reply through the channel the UI passed, like the backend would. */
function mockChat(request: ChatRequest, channel: Channel<ChatEvent>): Promise<string> {
  const internals = (window as unknown as { __TAURI_INTERNALS__: { runCallback(id: number, data: unknown): void } }).__TAURI_INTERNALS__;
  let index = 0;
  const emit = (message: ChatEvent) => internals.runCallback(channel.id, { index: index++, message });
  const reply = request.context.driverLabel === "MongoDB" ? MONGO_REPLY : SQL_REPLY;
  const pieces = reply.match(/[\s\S]{1,14}/g) ?? [];
  return new Promise((resolve) => {
    emit({ type: "tool", label: `Looked at ${request.context.table ?? "products"}` });
    let position = 0;
    const tick = () => {
      if (position >= pieces.length) {
        emit({ type: "done" });
        resolve(reply);
        return;
      }
      emit({ type: "text", text: pieces[position] });
      position += 1;
      setTimeout(tick, 18);
    };
    setTimeout(tick, 350);
  });
}

export function installDevMock(): void {
  mockWindows("main");
  mockIPC((command, payload) => handle(command, payload as Record<string, unknown>));
}
