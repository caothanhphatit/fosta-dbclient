import { CompletionContext, type Completion } from "@codemirror/autocomplete";
import { EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";
import { cursor, SHOP } from "../testSchema";
import type { LangDialect } from "../types";
import { aliasFor, sqlCompletions } from "./completion";
import { needsQuoting, quoteIfNeeded } from "./dialect";
import { SchemaCache } from "../schemaCache";
import { createSqlLanguage, currentSqlStatement } from "./support";

const complete = (text: string, dialect: LangDialect = "postgres", explicit = true) => {
  const { doc, pos } = cursor(text);
  return sqlCompletions(dialect, SHOP, doc, pos, explicit);
};
const labels = (text: string, dialect?: LangDialect) => complete(text, dialect)?.options.map((o) => o.label) ?? [];
const option = (text: string, label: string, dialect?: LangDialect): Completion | undefined => complete(text, dialect)?.options.find((o) => o.label === label);

describe("SQL completion", () => {
  it("offers statement keywords at the start, in the case typed", () => {
    expect(labels("|").slice(0, 3)).toEqual(["SELECT", "INSERT", "UPDATE"]);
    expect(labels("sel|")).toContain("select");
    expect(labels("|", "msSql")).toContain("EXEC");
  });

  it("offers tables, CTEs and other schemas after FROM", () => {
    const l = labels("with recent as (select 1) select * from |");
    expect(l).toContain("orders");
    expect(l).toContain("recent");
    expect(l).toContain("sales.invoices");
    expect(l).toContain("sales");
    expect(option("select * from |", "sales.invoices")?.apply).toBe("sales.invoices");
  });

  it("quotes identifiers per dialect when needed", () => {
    expect(option("select * from |", "Mixed Case")?.apply).toBe('"Mixed Case"');
    expect(option("select * from |", "Mixed Case", "mySql")?.apply).toBe("`Mixed Case`");
    expect(option("select * from |", "Mixed Case", "msSql")?.apply).toBe("[Mixed Case]");
    expect(quoteIfNeeded("Orders", "postgres")).toBe('"Orders"');
    expect(quoteIfNeeded("Orders", "mySql")).toBe("Orders");
    expect(quoteIfNeeded("orders", "oracle")).toBe('"orders"');
    expect(quoteIfNeeded("ORDERS", "oracle")).toBe("ORDERS");
    expect(quoteIfNeeded("select", "sqlite")).toBe('"select"');
    expect(quoteIfNeeded("#tmp", "msSql")).toBe("#tmp");
    expect(needsQuoting("a]b", "msSql")).toBe(true);
    expect(quoteIfNeeded("a]b", "msSql")).toBe("[a]]b]");
  });

  it("closes a quote the user opened instead of quoting again", () => {
    const result = complete('select * from "Mix|');
    const mixed = result?.options.find((o) => o.label === "Mixed Case");
    expect(mixed?.apply).toBe('Mixed Case"');
    expect(result?.from).toBe('select * from "'.length);
  });

  it("completes alias members with the columns of the aliased table", () => {
    const l = labels("select o.| from orders o");
    expect(l).toEqual(["id", "customer_id", "total", "created_at"]);
    expect(option("select o.| from orders o", "customer_id")?.detail).toContain("customers.id");
    expect(labels("select x.| from (select id, total as amount from orders) x")).toEqual(["id", "amount"]);
    expect(labels("with t as (select id as tid from orders) select t.| from t")).toEqual(["tid"]);
    expect(labels("select * from sales.|")).toEqual(["invoices"]);
  });

  it("completes columns in scope and qualifies ambiguous ones", () => {
    const l = labels("select | from orders o join customers c on c.id = o.customer_id");
    expect(l).toContain("total");
    expect(l).toContain("email");
    expect(l).toContain("o.id");
    expect(l).toContain("c.id");
    expect(l).not.toContain("id");
    expect(l).toContain("o");
    expect(l).toContain("count".toUpperCase());
    expect(option("select | from orders o join customers c on true", "o.id")?.apply).toBe("o.id");
  });

  it("ranks the innermost query's columns above outer ones", () => {
    const result = complete("select * from customers c where exists (select 1 from order_items i where |)");
    const qty = result?.options.find((o) => o.label === "qty");
    const email = result?.options.find((o) => o.label === "email");
    expect((qty?.boost ?? 0) > (email?.boost ?? 0)).toBe(true);
  });

  it("suggests FK joins after JOIN, both directions", () => {
    const l = labels("select * from orders o join |");
    expect(l).toContain("customers c ON c.id = o.customer_id");
    expect(l).toContain("order_items oi ON oi.order_id = o.id");
    expect(l).toContain("sales.invoices i ON i.order_id = o.id");
    // Aliases never collide with names already in scope.
    expect(labels("select * from orders c join |")).toContain("customers c2 ON c2.id = c.customer_id");
  });

  it("suggests FK conditions after ON", () => {
    const l = labels("select * from orders o join order_items oi on |");
    expect(l[0]).toBe("oi.order_id = o.id");
    expect(option("select * from orders o join order_items oi on |", "oi.order_id = o.id")?.apply).toBe("oi.order_id = o.id");
  });

  it("offers clause keywords after a table or a value", () => {
    expect(labels("select * from orders o |").slice(0, 3)).toEqual(["AS", "WHERE", "JOIN"]);
    expect(labels("select total |")).toContain("FROM");
  });

  it("stays quiet while typing nothing, inside strings and comments", () => {
    expect(complete("select | from orders", "postgres", false)).toBeNull();
    expect(complete("select o.| from orders o", "postgres", false)).not.toBeNull();
    expect(complete("select 'abc|' from orders")).toBeNull();
    expect(complete("-- sel|")).toBeNull();
  });

  it("keeps ClickHouse function names in their own case", () => {
    expect(labels("select to|", "clickHouse")).toContain("toDate");
    expect(labels("select |", "clickHouse")).not.toContain("TODATE");
  });

  it("works as a CodeMirror completion source", async () => {
    const support = createSqlLanguage({ dialect: "postgres", schema: new SchemaCache(SHOP), format: async (_d, t) => t.toUpperCase() });
    const extensions = await support.editorExtensions();
    const { doc, pos } = cursor("select o.| from orders o");
    const state = EditorState.create({ doc, extensions });
    const sources = state.languageDataAt<(c: CompletionContext) => unknown>("autocomplete", pos);
    expect(sources.length).toBeGreaterThan(0);
    const result = (await sources[0](new CompletionContext(state, pos, false))) as { options: Completion[] } | null;
    expect(result?.options.map((o) => o.label)).toContain("total");
    expect(await support.format?.("select 1")).toBe("SELECT 1");
    expect(support.statementAt("select 1; select 2", 12)).toEqual({ from: 10, to: 18 });
  });
});

describe("aliasFor", () => {
  it("uses initials and avoids taken names", () => {
    expect(aliasFor("order_items", new Set())).toBe("oi");
    expect(aliasFor("OrderItems", new Set())).toBe("oi");
    expect(aliasFor("customers", new Set(["c"]))).toBe("c2");
    expect(aliasFor("2fa", new Set())).toBe("t2");
  });
});

describe("currentSqlStatement", () => {
  it("runs the selection, else the statement under the cursor", () => {
    const doc = "select 1;\nselect 2;";
    expect(currentSqlStatement(EditorState.create({ doc, selection: { anchor: 12 } }), "generic")?.text).toBe("select 2");
    expect(currentSqlStatement(EditorState.create({ doc, selection: { anchor: 0, head: 6 } }), "generic")?.text).toBe("select");
    expect(currentSqlStatement(EditorState.create({ doc: "  " }), "generic")).toBeNull();
  });
});
