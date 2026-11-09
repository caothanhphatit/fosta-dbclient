import { describe, expect, it } from "vitest";
import type { SafetySettings } from "../../core/types";
import { newWorkspaceSafety } from "../safety/safety";
import {
  accessChoices,
  approvalTitle,
  cappedAccess,
  claudeCodeCommand,
  claudeDesktopConfig,
  cursorConfig,
  isReadOnlyWorkspace,
  secondsLeft,
  statusText,
  webEndpoint,
} from "./mcpLogic";

const open: SafetySettings = newWorkspaceSafety("laptop");
const readOnly: SafetySettings = { ...open, preset: "readOnly" };

describe("MCP access levels", () => {
  it("caps Production and Read-only workspaces at read-only", () => {
    expect(isReadOnlyWorkspace("laptop", open)).toBe(false);
    expect(isReadOnlyWorkspace("shield", open)).toBe(true);
    expect(isReadOnlyWorkspace("laptop", readOnly)).toBe(true);
    expect(cappedAccess("askBeforeWrites", "shield", open)).toBe("readOnly");
    expect(cappedAccess("askBeforeWrites", "laptop", open)).toBe("askBeforeWrites");
    expect(cappedAccess("off", "shield", readOnly)).toBe("off");
  });

  it("offers Ask before writes only where writes are possible", () => {
    expect(accessChoices("laptop", open)).toEqual(["off", "readOnly", "askBeforeWrites"]);
    expect(accessChoices("shield", open)).toEqual(["off", "readOnly"]);
  });

  it("treats custom rules that block every write as read-only", () => {
    const blocked: SafetySettings = {
      ...open,
      preset: "custom",
      rules: { read: "allow", write: "block", ddl: "block", destructive: "block", admin: "block" },
    };
    expect(isReadOnlyWorkspace("laptop", blocked)).toBe(true);
  });
});

describe("MCP setup snippets", () => {
  const url = "http://127.0.0.1:3910/mcp";

  it("builds the Claude Code command with a quoted header", () => {
    expect(claudeCodeCommand(url, "abc")).toBe(`claude mcp add --transport http fosta ${url} --header "Authorization: Bearer abc"`);
  });

  it("builds Cursor's mcp.json", () => {
    expect(JSON.parse(cursorConfig(url, "abc"))).toEqual({ mcpServers: { fosta: { url, headers: { Authorization: "Bearer abc" } } } });
  });

  it("builds Claude Desktop's stdio entry from the bridge command", () => {
    const config = JSON.parse(claudeDesktopConfig(["C:\\Apps\\fosta-studio.exe", "mcp"])) as { mcpServers: { fosta: { command: string; args: string[] } } };
    expect(config.mcpServers.fosta).toEqual({ command: "C:\\Apps\\fosta-studio.exe", args: ["mcp"] });
  });

  it("makes the web endpoint absolute", () => {
    expect(webEndpoint("https://studio.example/", "/mcp")).toBe("https://studio.example/mcp");
  });
});

describe("MCP labels", () => {
  it("shows the status only while on", () => {
    expect(statusText({ enabled: false, running: false, calls: 3 })).toBeNull();
    expect(statusText({ enabled: true, running: false, calls: 0 })).toBe("MCP: not running");
    expect(statusText({ enabled: true, running: true, calls: 0 })).toBe("MCP");
    expect(statusText({ enabled: true, running: true, calls: 1 })).toBe("MCP · 1 call");
    expect(statusText({ enabled: true, running: true, calls: 12 })).toBe("MCP · 12 calls");
  });

  it("names the client and counts down", () => {
    expect(approvalTitle("Claude Code")).toBe("Claude Code wants to run a statement");
    expect(approvalTitle("")).toBe("An MCP client wants to run a statement");
    expect(secondsLeft(1000, 120_000, 1000)).toBe(120);
    expect(secondsLeft(1000, 120_000, 200_000)).toBe(0);
  });
});
