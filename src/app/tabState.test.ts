import { describe, expect, it } from "vitest";
import {
  fitToBytes,
  jsonBytes,
  MAX_TAB_TEXT,
  mergeTabs,
  parseSavedTabs,
  queryNumber,
  restorableTabs,
  serializeTabs,
  type SavedTab,
  type SavedWorkspaceTabs,
} from "./tabState";

const query = (text: string, connectionId = "c1", title = "Query 1", id = `q-${title}`): Extract<SavedTab, { kind: "query" }> => ({
  id,
  kind: "query",
  connectionId,
  title,
  database: "shop",
  language: "sql",
  text,
  saved: "",
});
const table: SavedTab = {
  id: "t-orders",
  kind: "object",
  connectionId: "c2",
  title: "orders",
  role: "data",
  object: { database: "shop", schema: null, name: "orders", kind: "table" },
  filter: "status = 'NEW'",
};
const logs: SavedTab = { id: "l-tunnel", kind: "logs", taskId: "t1", title: "Tunnel · logs" };

const workspace = (tabs: SavedTab[], active: number | null = null): SavedWorkspaceTabs => ({ version: 1, tabs, active });

describe("serializeTabs and parseSavedTabs", () => {
  it("round-trips every kind of tab, its id and the active one", () => {
    const state = serializeTabs([query("SELECT 1"), table, logs], 1, 10_000);
    const parsed = parseSavedTabs(JSON.parse(JSON.stringify(state)));
    expect(parsed).toEqual({ version: 1, tabs: [query("SELECT 1"), table, logs], active: 1 });
  });

  it("stores the saved text only when it differs from the text", () => {
    const ran = { ...query("SELECT 1"), saved: "SELECT 1" };
    const [stored] = serializeTabs([ran], null, 10_000).tabs;
    expect(stored).not.toHaveProperty("saved");
    // Read back, the text counts as saved again.
    const parsed = parseSavedTabs(JSON.parse(JSON.stringify({ tabs: [stored] })));
    expect(parsed?.tabs[0]).toEqual(stored);
    expect(serializeTabs([query("SELECT 2")], null, 10_000).tabs[0]).toMatchObject({ saved: "" });
  });

  it("gives tabs of a file without ids (or with a repeated id) an id of their own", () => {
    const parsed = parseSavedTabs({
      tabs: [
        { kind: "logs", taskId: "t1", title: "a" },
        { kind: "logs", taskId: "t2", title: "b", id: "x" },
        { kind: "logs", taskId: "t3", title: "c", id: "x" },
      ],
    });
    const ids = parsed?.tabs.map((tab) => tab.id) ?? [];
    expect(ids[1]).toBe("x");
    expect(new Set(ids).size).toBe(3);
    expect(ids.every((id) => id.length > 0)).toBe(true);
  });

  it("leaves out text over the per-tab limit or the workspace budget", () => {
    const big = "x".repeat(MAX_TAB_TEXT + 1);
    const state = serializeTabs([query(big), query("SELECT 2", "c1", "Query 2"), query("SELECT 333", "c1", "Query 3")], 0, 10);
    expect(state.tabs[0]).toMatchObject({ text: "", textOmitted: true });
    expect(state.tabs[1]).toMatchObject({ text: "SELECT 2" });
    // Over what is left of the budget (8 of 10 characters are taken).
    expect(state.tabs[2]).toMatchObject({ text: "", textOmitted: true });
  });

  it("skips malformed tabs and rejects what is not a tab list", () => {
    expect(
      parseSavedTabs({ tabs: [{ kind: "query" }, logs, { kind: "object", connectionId: "c", role: "data", title: "x", object: { name: 1 } }], active: 1 }),
    ).toEqual({
      version: 1,
      tabs: [logs],
      active: 0,
    });
    expect(parseSavedTabs(null)).toBeNull();
    expect(parseSavedTabs({ tabs: "x" })).toBeNull();
  });

  it("drops an out-of-range active index", () => {
    expect(serializeTabs([logs], 5, 0).active).toBeNull();
  });
});

describe("fitToBytes", () => {
  it("drops the text of the last query tabs until the UTF-8 JSON fits", () => {
    // Two bytes per character: 300 characters take 600 bytes, more than the character count suggests.
    const state = serializeTabs([query("é".repeat(300), "c1", "Query 1"), query("ü".repeat(300), "c1", "Query 2")], null, 10_000);
    const fitted = fitToBytes(state, 1_000);
    expect(jsonBytes(fitted)).toBeLessThanOrEqual(1_000);
    expect(fitted.tabs[0]).toMatchObject({ text: "é".repeat(300) });
    expect(fitted.tabs[1]).toMatchObject({ text: "", textOmitted: true });
    expect(fitted.tabs[1]).not.toHaveProperty("saved");
    expect(fitToBytes(state, 1_000_000)).toBe(state);
  });

  it("measures once and drops exactly what re-measuring the whole state each time would", () => {
    const tabs = [
      query('"\\'.repeat(100), "c1", "Query 1"),
      { ...query("a".repeat(100), "c1", "Query 2"), saved: "b".repeat(100) },
      table,
      query("ü".repeat(50), "c1", "Query 3"),
    ];
    const state = serializeTabs(tabs, null, 10_000);
    /** The plain way: serialise the whole state again after each dropped text. */
    const reference = (limit: number): SavedWorkspaceTabs => {
      let current = state;
      for (let index = current.tabs.length - 1; index >= 0 && jsonBytes(current) > limit; index -= 1) {
        const tab = current.tabs[index];
        if (tab.kind !== "query") continue;
        const next = [...current.tabs];
        next[index] = fitToBytes(workspace([tab]), 0).tabs[0];
        current = { ...current, tabs: next };
      }
      return current;
    };
    for (const limit of [100, 300, 500, 700, 900, 1_100]) expect(fitToBytes(state, limit)).toEqual(reference(limit));
  });
});

describe("mergeTabs", () => {
  const a = query("A", "c1", "Query 1", "a");
  const b = query("B", "c1", "Query 2", "b");
  const c = query("C", "c1", "Query 3", "c");

  it("is this window's tabs when the stored ones are its own", () => {
    const local = workspace([a, b], 1);
    expect(mergeTabs(local, workspace([a, b]), new Set(["a", "b"]))).toBe(local);
    expect(mergeTabs(local, null, new Set())).toBe(local);
  });

  it("keeps another window's tabs after this window's, with this window's text and active tab", () => {
    const local = workspace([{ ...a, text: "A changed" }], 0);
    const stored = workspace([a, c], 1);
    expect(mergeTabs(local, stored, new Set(["a"]))).toEqual(workspace([{ ...a, text: "A changed" }, c], 0));
  });

  it("does not bring back a tab this window closed", () => {
    const local = workspace([a]);
    const stored = workspace([a, b, c]);
    // `b` was shown here and closed; `c` is another window's.
    expect(mergeTabs(local, stored, new Set(["a", "b"])).tabs.map((tab) => tab.id)).toEqual(["a", "c"]);
  });
});

describe("restorableTabs", () => {
  const saved = workspace([query("a"), table, logs, query("b", "c1", "Query 2")], 1);

  it("drops tabs whose connection or command is gone and keeps the active one", () => {
    const result = restorableTabs(
      saved,
      (id) => id === "c2",
      () => true,
    );
    expect(result.tabs).toEqual([table, logs]);
    expect(result.dropped).toBe(2);
    expect(result.active).toBe(0);
  });

  it("moves the active tab to its neighbour when it was dropped", () => {
    const result = restorableTabs(
      saved,
      (id) => id === "c1",
      () => false,
    );
    expect(result.tabs.map((tab) => tab.title)).toEqual(["Query 1", "Query 2"]);
    expect(result.active).toBe(0);
  });

  it("has no active tab when nothing is left", () => {
    expect(
      restorableTabs(
        saved,
        () => false,
        () => false,
      ),
    ).toEqual({ tabs: [], active: null, dropped: 4 });
  });
});

describe("queryNumber", () => {
  it("reads the number of a query tab title", () => {
    expect(queryNumber("Query 12")).toBe(12);
    expect(queryNumber("orders")).toBe(0);
  });
});

describe("workspace tabs (saved view)", () => {
  const orders: Extract<SavedTab, { kind: "object" }> = {
    id: "w-orders",
    kind: "object",
    connectionId: "c3",
    title: "orders",
    role: "workspace",
    object: { database: "shop", schema: null, name: "orders", kind: "collection" },
    view: { section: "aggregations", state: { documents: { filter: '{ "status": "NEW" }' }, aggregations: { stages: 3 } } },
  };

  it("round-trips the section and the section states", () => {
    const state = serializeTabs([orders, table], 0, 10_000);
    expect(parseSavedTabs(JSON.parse(JSON.stringify(state)))).toEqual({ version: 1, tabs: [orders, table], active: 0 });
  });

  it("keeps a file from before workspace tabs as it was, and drops a malformed view but not its tab", () => {
    const parsed = parseSavedTabs({ tabs: [{ ...table }] });
    expect(parsed?.tabs[0]).toEqual(table);
    expect(parsed?.tabs[0]).not.toHaveProperty("view");
    const broken = parseSavedTabs({
      tabs: [
        { ...orders, view: { section: 42 } },
        { ...orders, id: "w2", view: "documents" },
      ],
    });
    expect(broken?.tabs).toHaveLength(2);
    for (const tab of broken?.tabs ?? []) expect(tab).not.toHaveProperty("view");
    const noState = parseSavedTabs({ tabs: [{ ...orders, view: { section: "schema" } }] });
    expect(noState?.tabs[0]).toMatchObject({ view: { section: "schema", state: {} } });
  });

  it("bounds the saved view of each tab", () => {
    const huge: SavedTab = { ...orders, view: { section: "documents", state: { documents: { filter: "x" }, aggregations: "p".repeat(40_000) } } };
    const [stored] = serializeTabs([huge], null, 10_000).tabs;
    expect(stored).toMatchObject({ view: { section: "documents", state: { documents: { filter: "x" } } } });
    expect(JSON.stringify(stored).length).toBeLessThan(1_000);
  });

  it("drops section states of the last workspace tabs after query texts when the state is still too large", () => {
    const big = (id: string): SavedTab => ({ ...orders, id, view: { section: "documents", state: { documents: "d".repeat(20_000) } } });
    const state = serializeTabs([big("w1"), query("q".repeat(5_000)), big("w2")], null, 1_000_000);
    const fitted = fitToBytes(state, 26_000);
    expect(jsonBytes(fitted)).toBeLessThanOrEqual(26_000);
    expect(fitted.tabs[1]).toMatchObject({ text: "", textOmitted: true });
    expect(fitted.tabs[2]).toMatchObject({ view: { section: "documents", state: {} } });
    expect(fitted.tabs[0]).toMatchObject({ view: { state: { documents: "d".repeat(20_000) } } });
  });
});
