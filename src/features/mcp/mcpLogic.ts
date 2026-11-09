/** Pure helpers of the MCP server UI: setup snippets, access levels, labels. */
import type { McpAccess, McpStatus, SafetySettings } from "../../core/types";
import { effectiveRules, PRODUCTION_ICON } from "../safety/safety";

export const MCP_ACCESS_LABELS: Record<McpAccess, string> = {
  off: "Off",
  readOnly: "Read-only",
  askBeforeWrites: "Ask before writes",
};

/** Production and Read-only rules allow at most read-only access (the backend applies the same cap). */
export function isReadOnlyWorkspace(icon: string, safety: SafetySettings): boolean {
  if (icon === PRODUCTION_ICON || safety.preset === "readOnly") return true;
  const { rules } = effectiveRules(safety);
  return [rules.write, rules.ddl, rules.destructive, rules.admin].every((action) => action === "block");
}

export function cappedAccess(level: McpAccess, icon: string, safety: SafetySettings): McpAccess {
  return level === "askBeforeWrites" && isReadOnlyWorkspace(icon, safety) ? "readOnly" : level;
}

/** The choices the workspace dialog offers. */
export function accessChoices(icon: string, safety: SafetySettings): McpAccess[] {
  return isReadOnlyWorkspace(icon, safety) ? ["off", "readOnly"] : ["off", "readOnly", "askBeforeWrites"];
}

/** Quotes one argument for a POSIX shell or PowerShell when it needs it. */
function shellQuote(value: string): string {
  return /^[\w./:@=-]+$/.test(value) ? value : `"${value.replace(/(["\\$`])/g, "\\$1")}"`;
}

/** The `claude mcp add` command for the HTTP endpoint. */
export function claudeCodeCommand(url: string, token: string): string {
  return `claude mcp add --transport http fosta ${url} --header ${shellQuote(`Authorization: Bearer ${token}`)}`;
}

/** Cursor's `mcp.json` (also VS Code's `servers` shape with `type`). */
export function cursorConfig(url: string, token: string): string {
  return JSON.stringify({ mcpServers: { fosta: { url, headers: { Authorization: `Bearer ${token}` } } } }, null, 2);
}

/** Claude Desktop's `claude_desktop_config.json`: the stdio bridge, which reads the token itself. */
export function claudeDesktopConfig(command: string[]): string {
  const [program = "fosta-studio", ...args] = command;
  return JSON.stringify({ mcpServers: { fosta: { command: program, args } } }, null, 2);
}

/** The web edition's endpoint, absolute. */
export function webEndpoint(origin: string, path: string): string {
  return `${origin.replace(/\/$/, "")}${path}`;
}

/** The status bar text: "MCP" plus the number of tool calls, when the server is on. */
export function statusText(status: Pick<McpStatus, "enabled" | "running" | "calls">): string | null {
  if (!status.enabled) return null;
  if (!status.running) return "MCP: not running";
  return status.calls > 0 ? `MCP · ${status.calls} call${status.calls === 1 ? "" : "s"}` : "MCP";
}

/** The approval dialog's heading. The name is what the client reported about itself. */
export function approvalTitle(clientLabel: string): string {
  return `${clientLabel || "An MCP client"} wants to run a statement`;
}

/** Seconds left of an approval, for the countdown. */
export function secondsLeft(startedAt: number, timeoutMs: number, now: number): number {
  return Math.max(0, Math.ceil((startedAt + timeoutMs - now) / 1000));
}
