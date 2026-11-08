import { describe, expect, it } from "vitest";
import type { ObjectNode, TreeChildren } from "../../core/contract";
import { fillTemplate, plainName, ROOT, TreeModel, type VisibleRow } from "./treeModel";

function database(name: string): ObjectNode {
  const object = { database: name, schema: null, name, kind: "database" };
  return {
    key: { id: `db:${name}`, kind: "database", object },
    label: name,
    kind: "database",
    icon: "database",
    object,
    detail: null,
    badges: [],
    expandable: true,
    defaultView: null,
    actions: [],
  };
}

function table(db: string, name: string, schema: string | null = null): ObjectNode {
  const object = { database: db, schema, name, kind: "table" };
  const label = schema ? `${schema}.${name}` : name;
  return {
    key: { id: `db:${db}/table:${label}`, kind: "table", object },
    label,
    kind: "table",
    icon: "table",
    object,
    detail: null,
    badges: [],
    expandable: false,
    defaultView: "data",
    actions: [],
  };
}

function page(nodes: ObjectNode[], nextCursor: string | null = null, totalHint: number | null = null): TreeChildren {
  return { nodes, nextCursor, totalHint };
}

/** Rows as short strings: "  label" indented by depth, placeholders in brackets. */
function outline(rows: VisibleRow[]): string[] {
  return rows.map((row) => {
    const pad = "  ".repeat(row.depth);
    switch (row.type) {
      case "node":
        return `${pad}${row.node.label}${row.expanded ? " v" : ""}${row.count ? ` (${row.count})` : ""}`;
      case "more":
        return `${pad}[more ${row.loaded}${row.total === null ? "" : `/${row.total}`}]`;
      case "error":
        return `${pad}[error ${row.message}]`;
      default:
        return `${pad}[${row.type}]`;
    }
  });
}

function shopModel(): TreeModel {
  const model = new TreeModel();
  model.receive(ROOT, page([database("shop"), database("analytics")]), false);
  model.receive("db:shop", page([table("shop", "customers"), table("shop", "orders")]), false);
  model.setExpanded("db:shop", true);
  return model;
}

describe("TreeModel", () => {
  it("lazily expands: a collapsed node shows no children, an expanded one shows a loading row until its page arrives", () => {
    const model = new TreeModel();
    model.receive(ROOT, page([database("shop")]), false);
    expect(outline(model.rows())).toEqual(["shop"]);
    expect(model.needsLoad("db:shop")).toBe(true);
    model.setExpanded("db:shop", true);
    model.beginLoad("db:shop");
    expect(model.needsLoad("db:shop")).toBe(false);
    expect(outline(model.rows())).toEqual(["shop v", "  [loading]"]);
    model.receive("db:shop", page([]), false);
    expect(outline(model.rows())).toEqual(["shop v (0)", "  [empty]"]);
  });

  it("shows an error row when the first page fails", () => {
    const model = new TreeModel();
    model.receive(ROOT, page([database("shop")]), false);
    model.setExpanded("db:shop", true);
    model.beginLoad("db:shop");
    model.fail("db:shop", "Access denied");
    expect(outline(model.rows())).toEqual(["shop v", "  [error Access denied]"]);
    expect(model.needsLoad("db:shop")).toBe(true);
  });

  it("merges Load more pages in order without duplicates", () => {
    const model = new TreeModel();
    model.receive(ROOT, page([database("shop")]), false);
    model.setExpanded("db:shop", true);
    model.receive("db:shop", page([table("shop", "a"), table("shop", "b")], "2", 5), false);
    expect(outline(model.rows())).toEqual(["shop v (5)", "  a", "  b", "  [more 2/5]"]);
    model.beginLoad("db:shop");
    const more = model.rows().find((row) => row.type === "more");
    expect(more).toMatchObject({ loading: true });
    model.receive("db:shop", page([table("shop", "b"), table("shop", "c")], null, 5), true);
    expect(outline(model.rows())).toEqual(["shop v (5)", "  a", "  b", "  c"]);
    expect(model.reloadLimit("db:shop", 2)).toBe(3);
  });

  it("counts loaded children with a + while more pages wait and no total is known", () => {
    const model = new TreeModel();
    model.receive(ROOT, page([database("shop")]), false);
    model.receive("db:shop", page([table("shop", "a")], "1"), false);
    expect(model.rows()[0]).toMatchObject({ count: "1+" });
  });

  it("keeps ids, expansion and grandchildren of nodes that survive a refresh; forgets the ones that are gone", () => {
    const model = shopModel();
    model.setExpanded("db:analytics", true);
    model.receive("db:analytics", page([table("analytics", "events")]), false);
    expect(model.reloadTargets(ROOT)).toEqual([ROOT, "db:shop", "db:analytics"]);
    model.receive(ROOT, page([database("shop"), database("staging")]), false);
    expect(outline(model.rows())).toEqual(["shop v (2)", "  customers", "  orders", "staging"]);
    expect(model.node("db:analytics")).toBeUndefined();
    expect(model.list("db:analytics")).toBeUndefined();
    expect(model.isExpanded("db:analytics")).toBe(false);
    expect(model.parentOf("db:shop/table:orders")).toBe("db:shop");
  });

  it("filters on labels: matches keep their subtree, parents of loaded matches open, unloaded parents stay", () => {
    const model = shopModel();
    model.setExpanded("db:shop", false);
    expect(outline(model.rows("ORD"))).toEqual(["shop v (2)", "  orders", "analytics"]);
    expect(outline(model.rows("shop"))).toEqual(["shop (2)", "analytics"]);
    model.receive("db:analytics", page([table("analytics", "events")]), false);
    expect(outline(model.rows("zzz"))).toEqual([]);
    expect(outline(model.rows("  "))).toEqual(["shop (2)", "analytics (1)"]);
  });

  it("clears everything on disconnect", () => {
    const model = shopModel();
    model.clear();
    expect(model.rows()).toEqual([]);
    expect(model.needsLoad(ROOT)).toBe(true);
  });
});

describe("fillTemplate", () => {
  const products = { database: "shop", schema: null, name: "products", kind: "table" };
  const customers = { database: "ERP", schema: "dbo", name: "Customers", kind: "table" };

  it("quotes every part with the engine's identifier quoting", () => {
    expect(fillTemplate("TRUNCATE TABLE {qualified}", products, "mysql")).toBe("TRUNCATE TABLE `shop`.`products`");
    expect(fillTemplate("TRUNCATE TABLE {qualified}", customers, "tsql")).toBe("TRUNCATE TABLE [dbo].[Customers]");
    expect(fillTemplate("SELECT * FROM {schema}.{name} -- {database}", customers, "ansi")).toBe('SELECT * FROM "dbo"."Customers" -- "ERP"');
    expect(fillTemplate("DROP TABLE {name}", { ...products, name: "we`ird" }, "mysql")).toBe("DROP TABLE `we``ird`");
  });

  it("inserts names unquoted for engines without a SQL dialect", () => {
    expect(fillTemplate('{ "drop": "{name}" }', { database: "app", schema: null, name: "events", kind: "collection" }, null)).toBe('{ "drop": "events" }');
  });

  it("qualifies a database by its name only and falls back to the given database", () => {
    expect(fillTemplate("USE {qualified}", { database: "shop", schema: null, name: "shop", kind: "database" }, "mysql")).toBe("USE `shop`");
    expect(fillTemplate("{database}|{qualified}", null, "mysql", "shop")).toBe("`shop`|`shop`");
  });
});

describe("plainName", () => {
  it("is schema.name for schema objects and the label without an object", () => {
    expect(plainName(table("ERP", "Customers", "dbo"))).toBe("dbo.Customers");
    expect(plainName(table("shop", "orders"))).toBe("orders");
    expect(plainName({ label: "Indexes", object: null })).toBe("Indexes");
  });
});
