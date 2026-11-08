import { describe, expect, it } from "vitest";
import { collectObjects, databaseNodes } from "./connectionApi";
import type { ObjectNode, TreeChildren, TreeRequest } from "./contract";

function node(id: string, kind: string, label: string, expandable: boolean, database: string | null = null): ObjectNode {
  const object = { database, schema: null, name: label, kind };
  return { key: { id, kind, object }, label, kind, icon: kind, object, detail: null, badges: [], expandable, defaultView: null, actions: [] };
}

/** A catalog with a schema folder and paged children; records each request. */
function fixture() {
  const requests: TreeRequest[] = [];
  const children: Record<string, ObjectNode[]> = {
    "": [node("db:ERP", "database", "ERP", true, "ERP"), node("db:master", "database", "master", true, "master"), node("srv", "folder", "Server", true)],
    "db:ERP": [node("s:dbo", "schema", "dbo", true, "ERP"), node("t:log", "table", "log", false, "ERP")],
    "s:dbo": [node("t:a", "table", "Customers", false, "ERP"), node("v:b", "view", "Totals", false, "ERP"), node("p:c", "procedure", "Recalc", false, "ERP")],
    "db:master": [],
  };
  const source = {
    treeChildren: (request: TreeRequest): Promise<TreeChildren> => {
      requests.push(request);
      const all = children[request.parent?.id ?? ""] ?? [];
      const start = Number(request.cursor ?? 0);
      const end = start + 2;
      return Promise.resolve({ nodes: all.slice(start, end), nextCursor: end < all.length ? String(end) : null, totalHint: all.length });
    },
  };
  return { source, requests };
}

describe("tree walking helpers", () => {
  it("lists database nodes across pages", async () => {
    const { source } = fixture();
    expect((await databaseNodes(source)).map((item) => item.label)).toEqual(["ERP", "master"]);
  });

  it("collects relations of one database, looking inside schemas", async () => {
    const { source, requests } = fixture();
    const found = await collectObjects(source, "ERP");
    expect(found.map((item) => item.label)).toEqual(["Customers", "Totals", "log"]);
    expect(requests.some((request) => request.parent?.id === "db:master")).toBe(false);
    expect(await collectObjects(source, "missing")).toEqual([]);
  });
});
