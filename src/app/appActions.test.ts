import { describe, expect, it, vi } from "vitest";
import helpYaml from "../../docs/app-help.yaml?raw";

vi.mock("./diagnostics", () => ({ copyDiagnostics: vi.fn(), openLogFolder: vi.fn() }));
vi.mock("./update", () => ({ checkForUpdatesNow: vi.fn() }));
vi.mock("../features/mcp/mcpSettings", () => ({ openMcpSettings: vi.fn() }));

const { APP_ACTION_IDS, APP_ACTION_LABELS, appActionEntry, appActionRegistry, isAppActionId } = await import("./appActions");
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

describe("suggested action buttons", () => {
  it("are labelled by the registry with a short action label, never a help title", () => {
    const run = vi.fn();
    const registry = {
      ...appActionRegistry({
        newConnection: run,
        newWorkspace: run,
        editWorkspace: run,
        newCommand: run,
        newQuery: run,
        llmProviders: run,
        embeddingSettings: run,
      }),
    };
    expect(Object.keys(APP_ACTION_LABELS).sort()).toEqual([...APP_ACTION_IDS].sort());
    for (const label of Object.values(APP_ACTION_LABELS)) expect(label.length).toBeLessThanOrEqual(24);
    expect(appActionEntry(registry, "run:check_for_updates")?.label).toBe("Check for updates now");
    expect(appActionEntry(registry, "open_settings:llm_providers")?.label).toBe("LLM providers");
    // Still allowlisted: unknown ids get no button.
    expect(appActionEntry(registry, "run:drop_database")).toBeNull();
    expect(appActionEntry(registry, "constructor")).toBeNull();
    appActionEntry(registry, "open_dialog:new_connection")!.run();
    expect(run).toHaveBeenCalledTimes(1);
  });
});
