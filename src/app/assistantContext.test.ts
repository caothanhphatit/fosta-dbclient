import { describe, expect, it, vi } from "vitest";
import type { AppConfig } from "../core/types";
import type { ConnectionTree } from "../features/connections/tree";
import type { WorkspaceTab } from "../features/objectWorkspace/sectionWorkspace";
import { MAX_WORKSPACE_CONTEXT, workspaceContextText } from "../features/objectWorkspace/workspaceState";
import { assistantContext, composerTakesFocus, typingElsewhere } from "./assistantContext";

// The grid pulls in DOM modules at load time; this test only needs `instanceof` to fail.
vi.mock("../features/table/tableView", () => ({ TableView: class {} }));

const tree = { connectedIds: () => new Set(["c1"]), serverVersion: () => "8.0.4", activeContext: () => null } as unknown as ConnectionTree;
const config = {
  workspaces: [{ connections: [{ id: "c1", name: "Shop Mongo", driver: "mongodb" }] }],
} as unknown as AppConfig;

function workspaceTab(sections: { title: string; text: string; active: boolean }[], samples: string | null = null): WorkspaceTab {
  return {
    element: {} as HTMLElement,
    connectionId: "c1",
    object: { database: "shop", schema: null, name: "orders", kind: "collection" },
    activeSection: () => "documents",
    show: async () => undefined,
    deliver: async () => undefined,
    viewState: () => ({ section: "documents", state: {} }),
    workspaceContext: () => workspaceContextText("MongoDB workspace of collection shop.orders", sections),
    workspaceSamples: () => samples,
    onViewChanged: null,
  };
}

describe("assistantContext of an engine workspace tab", () => {
  it("names the connection and the object, and passes on what the sections say", () => {
    const context = assistantContext(workspaceTab([{ title: "Documents", text: 'filter: { "status": "NEW" }', active: true }]), config, tree);
    expect(context).toMatchObject({
      kind: "table",
      connectionId: "c1",
      connectionName: "Shop Mongo",
      database: "shop",
      table: "orders",
      serverVersion: "8.0.4",
    });
    expect(context.extra?.()).toContain('[Documents]\nfilter: { "status": "NEW" }');
  });

  it("keeps sampled documents apart from the context, for the backend's read gate", () => {
    const context = assistantContext(
      workspaceTab([{ title: "Aggregations", text: "Active stage: 2", active: true }], '{"email":"ann@example.com"}'),
      config,
      tree,
    );
    expect(context.object).toMatchObject({ name: "orders", kind: "collection" });
    expect(context.extra?.()).not.toContain("ann@example.com");
    expect(context.samples?.()).toBe('{"email":"ann@example.com"}');
  });

  it("keeps the sections' context within the limit", () => {
    const sections = Array.from({ length: 8 }, (_, index) => ({ title: `S${index}`, text: "x".repeat(20_000), active: index === 0 }));
    const extra = assistantContext(workspaceTab(sections), config, tree).extra?.() ?? "";
    expect(extra.length).toBeLessThanOrEqual(MAX_WORKSPACE_CONTEXT);
    expect(extra.startsWith("MongoDB workspace of collection shop.orders\n\n[S0]")).toBe(true);
  });
});

describe("focus on opening the assistant", () => {
  const panel = { contains: (node: unknown) => node === inPanel } as unknown as Node;
  const element = (editable: boolean) => ({ closest: (selector: string) => (editable && selector.includes(".cm-editor") ? {} : null) }) as unknown as Element;
  const inPanel = element(true);

  it("knows when the user is typing in an editor or field outside the panel", () => {
    expect(typingElsewhere(element(true), panel)).toBe(true);
    expect(typingElsewhere(element(false), panel)).toBe(false);
    expect(typingElsewhere(inPanel, panel)).toBe(false);
    expect(typingElsewhere(null, panel)).toBe(false);
  });

  it("takes the focus only from the keyboard shortcut, never from an editor", () => {
    expect(composerTakesFocus(true, false)).toBe(true);
    expect(composerTakesFocus(true, true)).toBe(false);
    expect(composerTakesFocus(false, false)).toBe(false);
    expect(composerTakesFocus(false, true)).toBe(false);
  });
});
