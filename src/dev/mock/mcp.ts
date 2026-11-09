/**
 * The MCP server in the dev mock: Settings → MCP server works (on/off, port, token, rotate), and
 * `__fostaMock.mcpApproval()` (or `mcpApproval("typed")`, or `?mcpApproval` in the address) simulates an incoming
 * `run_query` from Claude Code, so the approval dialog can be previewed. Answers are logged to the console.
 */
import { emit } from "@tauri-apps/api/event";
import type { AppConfig, McpApproval, McpStatus } from "../../core/types";
import { state } from "./state";

let token = randomToken();
let calls = 0;
const pending = new Map<string, McpApproval>();

function randomToken(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(32)), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function settings(): { enabled: boolean; port: number } {
  return state.config.mcp ?? { enabled: false, port: 3910 };
}

export function mockMcpStatus(): McpStatus {
  const { enabled, port } = settings();
  return {
    enabled,
    port,
    running: enabled,
    error: null,
    url: `http://127.0.0.1:${port}/mcp`,
    calls,
    bridgeCommand: ["C:\\Program Files\\Fosta Studio\\fosta-studio.exe", "mcp"],
    clients: enabled && calls > 0 ? ["Claude Code"] : [],
    pending: [...pending.values()],
  };
}

export function mockSetMcpSettings(enabled: boolean, port: number | null): AppConfig {
  if (port !== null && (port < 1024 || port > 65535)) throw "Pick a port from 1024 to 65535";
  state.config = { ...state.config, mcp: { enabled, port: port ?? settings().port } };
  return state.config;
}

export function mockMcpToken(): string {
  return token;
}

export function mockRotateMcpToken(): string {
  token = randomToken();
  return token;
}

export function mockMcpAnswer(id: string, approve: boolean, typed: string | null): null {
  const approval = pending.get(id);
  if (!approval) throw "This request is no longer waiting: it was answered elsewhere or timed out.";
  pending.delete(id);
  console.info(`[mock] MCP approval ${approve ? "approved" : "denied"}`, { statement: approval.statement, typed });
  void emit("mcp-approval-closed", { id });
  return null;
}

/** Simulates Claude Code calling `run_query` on the first connection. */
export function simulateMcpApproval(level: "confirm" | "typed" = "confirm"): string {
  const workspace = state.config.workspaces[0];
  const connection = workspace?.connections[0];
  const id = randomToken();
  const statement =
    level === "typed" ? "DELETE FROM orders WHERE created_at < '2020-01-01'" : "UPDATE products SET price = price * 1.1 WHERE category = 'books'";
  const category = level === "typed" ? "destructive" : "write";
  const approval: McpApproval = {
    id,
    client: "claude-code",
    clientLabel: "Claude Code",
    connectionId: connection?.id ?? "c-local",
    connection: connection?.name ?? "Local",
    workspace: workspace?.name ?? "Default",
    database: connection?.database ?? "shop",
    statement,
    problem: {
      code: "CONFIRMATION_REQUIRED",
      message: "These statements need a confirmation.",
      caller: "mcp",
      workspace: workspace?.name ?? "Default",
      connection: connection?.name ?? "Local",
      connectionId: connection?.id ?? "c-local",
      preset: "guarded",
      level,
      token: null,
      expected: level === "typed" ? "orders" : null,
      categories: [category],
      tables: [level === "typed" ? "orders" : "products"],
      statements: [{ category, kind: level === "typed" ? "DELETE" : "UPDATE", excerpt: statement, tables: [], reason: null }],
    },
    timeoutMs: 120_000,
  };
  pending.set(id, approval);
  calls += 1;
  void emit("mcp-activity", { calls, clients: ["Claude Code"], client: "Claude Code", tool: "run_query" });
  void emit("mcp-approval", approval);
  window.setTimeout(() => {
    if (pending.delete(id)) void emit("mcp-approval-closed", { id });
  }, approval.timeoutMs);
  return id;
}

if (new URLSearchParams(window.location.search).has("mcpApproval")) {
  window.setTimeout(() => simulateMcpApproval(), 1500);
}
