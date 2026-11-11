import { describe, expect, it, vi } from "vitest";
import helpYaml from "../../docs/app-help.yaml?raw";

vi.mock("./diagnostics", () => ({ copyDiagnostics: vi.fn(), openLogFolder: vi.fn() }));
vi.mock("./update", () => ({ checkForUpdatesNow: vi.fn() }));
vi.mock("../features/mcp/mcpSettings", () => ({ openMcpSettings: vi.fn() }));

const { APP_ACTION_IDS, appActionRegistry, isAppActionId } = await import("./appActions");
const { openMcpSettings } = await import("../features/mcp/mcpSettings");

/** The distinct `action:` ids of `docs/app-help.yaml`. */
function helpActionIds(): string[] {
  const ids = [...helpYaml.matchAll(/^ {2}action:\s*(\S+)\s*$/gm)].map((match) => match[1]);
  return [...new Set(ids)].sort();
}

describe("the app action registry", () => {
  it("maps exactly the actions the help pack names", () => {
    const help = helpActionIds();
    expect(help.length).toBeGreaterThanOrEqual(10);
    expect([...APP_ACTION_IDS].sort()).toEqual(help);
  });

  it("runs the mapped function only when called, and knows no other ids", () => {
    const host = {
      newConnection: vi.fn(),
      newWorkspace: vi.fn(),
      editWorkspace: vi.fn(),
      newCommand: vi.fn(),
      newQuery: vi.fn(),
      llmProviders: vi.fn(),
      embeddingSettings: vi.fn(),
    };
    const registry = appActionRegistry(host);
    expect(Object.keys(registry).sort()).toEqual([...APP_ACTION_IDS].sort());
    expect(host.newConnection).not.toHaveBeenCalled();
    registry["open_dialog:new_connection"]();
    expect(host.newConnection).toHaveBeenCalledTimes(1);
    registry["open_settings:mcp"]();
    expect(openMcpSettings).toHaveBeenCalledTimes(1);
    expect(isAppActionId("open_view:new_query")).toBe(true);
    expect(isAppActionId("run:drop_database")).toBe(false);
    expect(isAppActionId("toString")).toBe(false);
  });
});
