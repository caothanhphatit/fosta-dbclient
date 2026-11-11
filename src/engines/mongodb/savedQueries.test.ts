import { describe, expect, it } from "vitest";
import type { HistoryEntry, SavedQuery } from "../../core/types";
import { commandText, EMPTY_QUERY, type QueryState } from "./queryModel";
import { groupSaved, historyTarget, readQueryRequest, sameName, savedQueryOf, savedSummary, stateOfSaved, suggestName } from "./savedQueries";

const target = { connectionId: "c1", database: "shop", collection: "orders" };

function saved(id: string, name: string, collection = "orders", extra: Partial<SavedQuery> = {}): SavedQuery {
  return { ...savedQueryOf(id, name, { ...EMPTY_QUERY, filter: `{ n: '${id}' }` }, { ...target, collection }), ...extra };
}

function entry(sql: string, extra: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    id: "h1",
    sql,
    connectionId: "c1",
    connectionName: "Mongo",
    driver: "mongodb",
    database: "shop",
    startedAt: 0,
    durationMs: 1,
    results: [],
    error: null,
    cancelled: false,
    ...extra,
  };
}

describe("saved queries", () => {
  it("keep the bar's fields as typed and read them back", () => {
    const state: QueryState = { filter: "status: 'paid'", project: "{ total: 1 }", sort: "{ total: -1 }", collation: "{ locale: 'en' }", skip: 3, limit: 50 };
    const query = savedQueryOf("q1", "  Paid  ", state, target);
    expect(query).toMatchObject({ id: "q1", name: "Paid", engine: "mongodb", connectionId: "c1", database: "shop", collection: "orders", pipeline: null });
    expect(stateOfSaved(query)).toEqual(state);
  });

  it("read malformed entries with defaults", () => {
    const broken = { ...saved("a", "A"), query: { filter: 5, limit: 99999, skip: -2 } } as unknown as SavedQuery;
    expect(stateOfSaved(broken)).toEqual({ ...EMPTY_QUERY, limit: 1000 });
    expect(readQueryRequest({ filter: "a: 1", limit: 5 })).toEqual({ ...EMPTY_QUERY, filter: "a: 1", limit: 5 });
    expect(readQueryRequest({ limit: 5 })).toBeNull();
    expect(readQueryRequest("x")).toBeNull();
  });

  it("list this collection's first, then the database's, filtered by every search word", () => {
    const list = [
      saved("1", "zeta", "orders"),
      saved("2", "Alpha", "orders"),
      saved("3", "big", "customers"),
      saved("4", "other db", "orders", { database: "crm" }),
      saved("5", "redis", "orders", { engine: "redis" }),
      saved("6", "abc", "audit"),
    ];
    const groups = groupSaved(list, "shop", "orders");
    expect(groups.here.map((item) => item.name)).toEqual(["Alpha", "zeta"]);
    expect(groups.others.map((item) => item.name)).toEqual(["abc", "big"]);
    expect(groupSaved(list, "shop", "orders", "CUST").others.map((item) => item.id)).toEqual(["3"]);
    expect(groupSaved(list, "shop", "orders", "n: '2'").here.map((item) => item.id)).toEqual(["2"]);
    expect(groupSaved(list, "shop", "orders", "alpha zzz").here).toEqual([]);
  });

  it("summarise what is set", () => {
    const query = savedQueryOf("q", "Q", { ...EMPTY_QUERY, filter: "a: 1", sort: "{ b: -1 }", limit: 5 }, target);
    expect(savedSummary(query)).toBe("a: 1 · sort { b: -1 } · limit 5");
    expect(savedSummary(savedQueryOf("q", "Q", EMPTY_QUERY, target))).toBe("{}");
  });

  it("suggest a free name and find a name the collection has", () => {
    const list = [saved("1", "orders query"), saved("2", "Orders Query 2"), saved("3", "orders query 3", "customers")];
    expect(suggestName(list, "shop", "orders")).toBe("orders query 3");
    expect(suggestName([], "shop", "orders")).toBe("orders query");
    expect(sameName(list, { database: "shop", collection: "orders", name: " ORDERS QUERY " })?.id).toBe("1");
    expect(sameName(list, { database: "shop", collection: "orders", name: "orders query" }, "1")).toBeNull();
    expect(sameName(list, { database: "shop", collection: "customers", name: "orders query" })).toBeNull();
  });
});

describe("historyTarget", () => {
  it("reopens a query-bar run in its collection workspace with the bar filled in", () => {
    const state: QueryState = { ...EMPTY_QUERY, filter: "{ status: 'paid' }", sort: "{ total: -1 }", skip: 5, limit: 50 };
    const found = historyTarget(entry(commandText(state, 'my "odd" coll')));
    expect(found).toMatchObject({
      connectionId: "c1",
      section: "documents",
      object: { database: "shop", schema: null, name: 'my "odd" coll', kind: "collection" },
      request: { query: state },
    });
  });

  it("leaves other entries to the query tab", () => {
    expect(historyTarget(entry("db.orders.find({})"))).toBeNull();
    expect(historyTarget(entry('db.getCollection("orders").aggregate([])'))).toBeNull();
    expect(historyTarget(entry(commandText(EMPTY_QUERY, "orders"), { driver: "postgres" }))).toBeNull();
    expect(historyTarget(entry(commandText(EMPTY_QUERY, "orders"), { database: null }))).toBeNull();
    expect(historyTarget(entry(`${commandText(EMPTY_QUERY, "orders")}.pretty()`))).toBeNull();
  });
});
