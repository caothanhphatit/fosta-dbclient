import { describe, expect, it } from "vitest";
import { WorkspaceStore } from "./state";
import { boundViewState, MAX_SECTION_CONTEXT, MAX_VIEW_BYTES, MAX_WORKSPACE_CONTEXT, parseViewState, workspaceContextText } from "./workspaceState";

describe("boundViewState", () => {
  it("keeps small states as plain JSON", () => {
    const view = { section: "documents", state: { documents: { filter: "{}", when: new Date(0), skip: undefined }, nothing: undefined } };
    expect(boundViewState(view)).toEqual({ section: "documents", state: { documents: { filter: "{}", when: "1970-01-01T00:00:00.000Z" } } });
  });

  it("drops a state that is not JSON", () => {
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    expect(boundViewState({ section: "a", state: { a: cyclic, b: 1n, c: 3 } })).toEqual({ section: "a", state: { c: 3 } });
  });

  it("keeps only the shown section's state, then none, when too large", () => {
    const other = "o".repeat(MAX_VIEW_BYTES);
    expect(boundViewState({ section: "a", state: { a: { x: 1 }, b: other } })).toEqual({ section: "a", state: { a: { x: 1 } } });
    expect(boundViewState({ section: "a", state: { a: other, b: 1 } })).toEqual({ section: "a", state: {} });
    // Bytes, not characters.
    expect(boundViewState({ section: "a", state: { a: "é".repeat(600) } }, 1_000)).toEqual({ section: "a", state: {} });
  });
});

describe("parseViewState", () => {
  it("reads what was saved and rejects anything malformed", () => {
    expect(parseViewState({ section: "schema", state: { schema: { mode: "sample" } } })).toEqual({ section: "schema", state: { schema: { mode: "sample" } } });
    expect(parseViewState({ section: "schema", state: [1] })).toEqual({ section: "schema", state: {} });
    for (const bad of [undefined, null, "schema", { section: "" }, { section: 1 }, { section: "s".repeat(65) }]) expect(parseViewState(bad)).toBeUndefined();
  });
});

describe("workspaceContextText", () => {
  it("puts the shown section first and leaves out sections without context", () => {
    const text = workspaceContextText("MongoDB workspace of collection shop.orders", [
      { title: "Documents", text: "filter: {}", active: false },
      { title: "Schema", text: null, active: false },
      { title: "Aggregations", text: "3 stages", active: true },
    ]);
    expect(text).toBe("MongoDB workspace of collection shop.orders\n\n[Aggregations]\n3 stages\n\n[Documents]\nfilter: {}");
  });

  it("bounds each section and the whole", () => {
    const long = "x".repeat(50_000);
    const one = workspaceContextText("h", [{ title: "A", text: long, active: true }]);
    expect(one.length).toBeLessThanOrEqual(MAX_SECTION_CONTEXT + 20);
    expect(one).toContain("… (cut)");
    const many = workspaceContextText(
      "h",
      Array.from({ length: 10 }, (_, index) => ({ title: `S${index}`, text: long, active: false })),
    );
    expect(many.length).toBeLessThanOrEqual(MAX_WORKSPACE_CONTEXT);
  });
});

describe("WorkspaceStore", () => {
  it("shares values between sections and tells subscribers about changes", () => {
    const store = new WorkspaceStore();
    const seen: unknown[] = [];
    const stop = store.subscribe<string>("filter", (value) => seen.push(value));
    store.set("filter", "{}");
    store.set("filter", "{}");
    store.set("filter", '{ "a": 1 }');
    stop();
    store.set("filter", "later");
    expect(seen).toEqual(["{}", '{ "a": 1 }']);
    expect(store.get("filter")).toBe("later");
    store.clear();
    expect(store.get("filter")).toBeUndefined();
  });
});
