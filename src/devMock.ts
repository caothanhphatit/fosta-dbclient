/**
 * Browser-only stand-in for the Tauri backend, used by `npm run dev` when the
 * page is opened in a plain browser (no `__TAURI_INTERNALS__`). It lets the UI
 * be reviewed without a MariaDB server. Never bundled into the desktop build.
 */
import type { Channel } from "@tauri-apps/api/core";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { baseUrlOf, providerPreset } from "./aiProviders";
import type { AiSettings, AppConfig, ChatEvent, ChatRequest, ConnectionProfile, TablePage, Workspace } from "./types";

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

let config: AppConfig = { workspaces, activeWorkspace: "client-a", embedding: null, ai: null };

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
      if (typeof payload.apiKey === "string" && payload.apiKey) browserApiKey = payload.apiKey;
      if (!config.ai) browserApiKey = "";
      return config;
    case "test_ai":
      return probeChat();
    case "list_ai_models":
      return fetchModels(payload.settings as AiSettings, (payload.apiKey as string | null) ?? null);
    case "ai_chat":
      return streamChat(payload.request as ChatRequest, payload.onEvent as Channel<ChatEvent>);
    case "ai_cancel":
      chatRuns.get(String(payload.id))?.abort();
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

// --- Assistant: real provider calls from the browser ------------------------
// The preview has no Rust backend, so the dialog's key is kept in memory for
// this page load and requests go straight to the provider from the browser.
// Schema lookups (tools) are desktop-only; here the model gets the table list.

let browserApiKey = "";
const chatRuns = new Map<string, AbortController>();

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
    const message = typeof parsed.error === "string" ? parsed.error : parsed.error?.message ?? parsed.message;
    if (message) return `${response.status}: ${message}`;
  } catch {
    // Not JSON; fall through to the raw preview.
  }
  return `${response.status}: ${body.slice(0, 300)}`;
}

async function fetchModels(settings: AiSettings, apiKey: string | null): Promise<string[]> {
  const key = apiKey || browserApiKey;
  const anthropic = providerPreset(settings.provider).wire === "anthropic";
  const url = `${baseUrlOf(settings)}${anthropic ? "/v1/models?limit=1000" : "/models"}`;
  const response = await fetch(url, { headers: providerHeaders(settings, key) });
  if (!response.ok) throw new Error(`${providerPreset(settings.provider).label} API returned ${await failureText(response)}`);
  const payload = (await response.json()) as { data?: { id?: string; name?: string }[] };
  const ids = (payload.data ?? []).map((item) => (item.id ?? item.name ?? "").replace(/^models\//, "")).filter(Boolean);
  return [...new Set(ids)].sort();
}

function systemPrompt(context: ChatRequest["context"]): string {
  const lines = [
    "You are the assistant built into DB Studio, a desktop database client. You cannot run anything: put every runnable query in a fenced code block (```sql, or ```json for MongoDB commands) for the user to run from the editor. Answer in the user's language and keep prose short.",
    "Context:",
    context.connectionId ? `- Connection: ${context.connectionName ?? "connection"} (${context.driverLabel ?? "unknown"} ${context.serverVersion ?? ""})` : "- No connection is open; write generic SQL and say which engine you assumed.",
  ];
  if (context.database) lines.push(`- Database: ${context.database}`, "- Tables: products (~1240 rows), categories (12), orders (48210), order_items (153900), customers (9031), v_daily_sales (view)");
  if (context.table) lines.push(`- Table the user is looking at: ${context.table}`);
  if (context.editorText) lines.push("- Current editor text:", "```", context.editorText, "```");
  if (context.lastError) lines.push(`- Last error from the server: ${context.lastError}`);
  if (context.extra) lines.push(`- ${context.extra}`);
  return lines.join("\n");
}

/** Streams the provider's reply through the channel the UI passed, like the backend does. */
async function streamChat(request: ChatRequest, channel: Channel<ChatEvent>): Promise<string> {
  const settings = config.ai;
  if (!settings) throw new Error("Set up the assistant first (toolbar → Assistant → settings)");
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
    const response = await fetch(`${baseUrlOf(settings)}${anthropic ? "/v1/messages" : "/chat/completions"}`, {
      method: "POST",
      headers: providerHeaders(settings, browserApiKey),
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
        const event = JSON.parse(payload) as { type?: string; delta?: { type?: string; text?: string }; choices?: { delta?: { content?: string } }[]; error?: { message?: string } };
        if (event.error?.message) throw new Error(`${preset.label} API: ${event.error.message}`);
        const piece = anthropic ? (event.type === "content_block_delta" && event.delta?.type === "text_delta" ? event.delta.text : undefined) : event.choices?.[0]?.delta?.content;
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
async function probeChat(): Promise<string> {
  const settings = config.ai;
  if (!settings) throw new Error("Set up the assistant first");
  const preset = providerPreset(settings.provider);
  const anthropic = preset.wire === "anthropic";
  const prompt = { role: "user", content: "Reply with the single word OK." };
  const body = anthropic ? { model: settings.model, max_tokens: 64, messages: [prompt] } : { model: settings.model, messages: [prompt] };
  const response = await fetch(`${baseUrlOf(settings)}${anthropic ? "/v1/messages" : "/chat/completions"}`, { method: "POST", headers: providerHeaders(settings, browserApiKey), body: JSON.stringify(body) });
  if (!response.ok) throw new Error(`${preset.label} API returned ${await failureText(response)}`);
  const payload = (await response.json()) as { content?: { text?: string }[]; choices?: { message?: { content?: string } }[] };
  return (anthropic ? payload.content?.map((block) => block.text ?? "").join("") : payload.choices?.[0]?.message?.content)?.trim() || "(empty reply)";
}

export function installDevMock(): void {
  mockWindows("main");
  mockIPC((command, payload) => handle(command, payload as Record<string, unknown>));
}
