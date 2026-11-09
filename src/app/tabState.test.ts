import { describe, expect, it } from "vitest";
import { fitToBytes, jsonBytes, MAX_TAB_TEXT, parseSavedTabs, queryNumber, restorableTabs, serializeTabs, type SavedTab } from "./tabState";

const query = (text: string, connectionId = "c1", title = "Query 1"): SavedTab => ({
  kind: "query",
  connectionId,
  title,
  database: "shop",
  language: "sql",
  text,
  saved: "",
});
const table: SavedTab = {
  kind: "object",
  connectionId: "c2",
  title: "orders",
  role: "data",
  object: { database: "shop", schema: null, name: "orders", kind: "table" },
  filter: "status = 'NEW'",
};
const logs: SavedTab = { kind: "logs", taskId: "t1", title: "Tunnel · logs" };

describe("serializeTabs and parseSavedTabs", () => {
  it("round-trips every kind of tab and the active one", () => {
    const state = serializeTabs([query("SELECT 1"), table, logs], 1, 10_000);
    const parsed = parseSavedTabs(JSON.parse(JSON.stringify(state)));
    expect(parsed).toEqual({ version: 1, tabs: [query("SELECT 1"), table, logs], active: 1 });
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
    expect(fitToBytes(state, 1_000_000)).toBe(state);
  });
});

describe("restorableTabs", () => {
  const saved = { version: 1 as const, tabs: [query("a"), table, logs, query("b", "c1", "Query 2")], active: 1 };

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
