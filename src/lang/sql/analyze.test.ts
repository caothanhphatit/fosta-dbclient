import { describe, expect, it } from "vitest";
import { cursor, SHOP } from "../testSchema";
import type { LangDialect } from "../types";
import { analyzeSql, resolveMember } from "./analyze";

const analyze = (text: string, dialect: LangDialect = "postgres") => {
  const { doc, pos } = cursor(text);
  return analyzeSql(dialect, SHOP, doc, pos);
};

const refs = (text: string) => analyze(text).scope.map((r) => `${r.kind}:${r.name}${r.alias ? ` as ${r.alias}` : ""}`);

describe("scope", () => {
  it("collects FROM and JOIN refs with aliases, in any clause order", () => {
    expect(refs("select | from orders o join customers as c on c.id = o.customer_id, products")).toEqual([
      "table:orders as o",
      "table:customers as c",
      "table:products",
    ]);
  });

  it("resolves schema-qualified and quoted names", () => {
    const ctx = analyze('select | from sales.invoices i, "Mixed Case" m');
    expect(ctx.scope.map((r) => [r.name, r.schema, r.alias])).toEqual([
      ["invoices", "sales", "i"],
      ["Mixed Case", "public", "m"],
    ]);
    expect(ctx.scope[1].columns.map((c) => c.name)).toEqual(["Value"]);
  });

  it("derives subquery columns from aliases, plain columns and stars", () => {
    const ctx = analyze("select x.| from (select o.id, o.total * 2 as doubled, c.* from orders o join customers c on c.id = o.customer_id) x");
    const sub = ctx.scope.find((r) => r.alias === "x");
    expect(sub?.kind).toBe("subquery");
    expect(sub?.columns.map((c) => c.name)).toEqual(["id", "doubled", "id", "name", "email"]);
    expect(sub?.columns[0].type).toBe("int");
  });

  it("resolves CTE names, explicit CTE columns and CTEs built on CTEs", () => {
    const ctx = analyze("with big(oid, amount) as (select id, total from orders), top as (select * from big) select | from top t");
    expect(ctx.ctes.map((c) => c.name)).toEqual(["big", "top"]);
    expect(ctx.ctes[0].columns.map((c) => c.name)).toEqual(["oid", "amount"]);
    const t = ctx.scope.find((r) => r.alias === "t");
    expect(t?.kind).toBe("cte");
    expect(t?.columns.map((c) => c.name)).toEqual(["oid", "amount"]);
  });

  it("sees outer refs from a correlated subquery, innermost first", () => {
    const ctx = analyze("select * from customers c where exists (select 1 from orders o where o.customer_id = c.| )");
    expect(ctx.scope.map((r) => r.alias)).toEqual(["o", "c"]);
    expect(ctx.kind).toBe("member");
    expect(ctx.qualifier).toEqual(["c"]);
  });

  it("keeps refs of UPDATE, INSERT INTO and DELETE", () => {
    expect(refs("update orders set total = | where id = 1")).toEqual(["table:orders"]);
    expect(refs("insert into order_items (|) values (1)")).toEqual(["table:order_items"]);
    expect(refs("delete from customers where |")).toEqual(["table:customers"]);
  });

  it("only looks at the statement under the cursor", () => {
    expect(refs("select * from products;\nselect | from orders;\nselect * from customers")).toEqual(["table:orders"]);
  });
});

describe("cursor context", () => {
  it("classifies positions", () => {
    expect(analyze("|").kind).toBe("statementStart");
    expect(analyze("sel|").kind).toBe("statementStart");
    expect(analyze("select * from |").kind).toBe("table");
    expect(analyze("select * from orders, |").kind).toBe("table");
    expect(analyze("select * from orders o join |").kind).toBe("join");
    expect(analyze("select * from orders o left outer join customers c on |").kind).toBe("joinCondition");
    expect(analyze("select * from orders o join customers c on c.id = o.customer_id and |").kind).toBe("joinCondition");
    expect(analyze("select * from orders o |").kind).toBe("afterTable");
    expect(analyze("select | from orders").kind).toBe("column");
    expect(analyze("select a, | from orders").kind).toBe("column");
    expect(analyze("select * from orders where total > | ").kind).toBe("column");
    expect(analyze("select total |").kind).toBe("afterValue");
    expect(analyze("select 'it|s' from orders").kind).toBe("none");
    expect(analyze("select 1 -- note |").kind).toBe("none");
    expect(analyze("insert into |").kind).toBe("table");
    expect(analyze("update |").kind).toBe("table");
    expect(analyze("select * from orders o join customers c using (|").kind).toBe("column");
  });

  it("reports the typed prefix, member qualifiers and quotes", () => {
    const ctx = analyze("select o.cust| from orders o");
    expect(ctx.kind).toBe("member");
    expect(ctx.prefix).toBe("cust");
    expect(ctx.qualifier).toEqual(["o"]);
    const quoted = analyze('select * from "Mix|');
    expect(quoted.kind).toBe("table");
    expect(quoted.quote).toBe('"');
    expect(quoted.prefix).toBe("Mix");
    const twoPart = analyze("select * from public.orders where public.orders.|");
    expect(twoPart.qualifier).toEqual(["public", "orders"]);
  });

  it("names the table just joined for ON conditions", () => {
    const ctx = analyze("select * from orders o join order_items oi on |");
    expect(ctx.joinTarget?.alias).toBe("oi");
  });
});

describe("resolveMember", () => {
  it("resolves aliases, table names, CTEs, schemas and schema.table", () => {
    const member = (text: string) => {
      const ctx = analyze(text);
      const target = resolveMember(SHOP, ctx);
      if (target.kind === "columns") return `columns:${target.table.name}`;
      if (target.kind === "tables") return `tables:${target.schema}`;
      if (target.kind === "schemas") return `schemas:${target.database}`;
      return target.kind;
    };
    expect(member("select o.| from orders o")).toBe("columns:orders");
    expect(member("select orders.| from orders")).toBe("columns:orders");
    expect(member("with t as (select 1 as one) select t.| from t")).toBe("columns:t");
    expect(member("select * from sales.|")).toBe("tables:sales");
    expect(member("select * from shop.|")).toBe("schemas:shop");
    expect(member("select sales.invoices.| from sales.invoices")).toBe("columns:invoices");
    expect(member("select nope.| from orders")).toBe("unknown");
  });
});
