import { describe, expect, it } from "vitest";
import { containsPattern, describeKey, equalsFilter, quoteIdent, quoteTable, rowKey, sqlDialect, sqlLiteral } from "./sql";
import type { ColumnMeta } from "./types";

const column = (name: string, kind: Partial<ColumnMeta> = {}): ColumnMeta => ({
  name,
  typeName: "varchar",
  numeric: false,
  binary: false,
  primary: false,
  vectorDims: null,
  ...kind,
});

describe("sqlDialect", () => {
  it("maps drivers to their quoting style", () => {
    expect(sqlDialect("mariadb")).toBe("mysql");
    expect(sqlDialect("mysql")).toBe("mysql");
    expect(sqlDialect("mssql")).toBe("tsql");
    expect(sqlDialect("postgres")).toBe("ansi");
  });
});

describe("quoteIdent / quoteTable", () => {
  it("wraps in backticks on MySQL and doubles embedded ones", () => {
    expect(quoteIdent("orders", "mysql")).toBe("`orders`");
    expect(quoteIdent("we`ird", "mysql")).toBe("`we``ird`");
  });

  it("wraps in brackets on SQL Server", () => {
    expect(quoteIdent("Invoices", "tsql")).toBe("[Invoices]");
    expect(quoteIdent("we]ird", "tsql")).toBe("[we]]ird]");
  });

  it("wraps in double quotes in ANSI SQL", () => {
    expect(quoteIdent("orders", "ansi")).toBe(`"orders"`);
    expect(quoteIdent(`we"ird`, "ansi")).toBe(`"we""ird"`);
  });

  it("splits schema.table where the engine has schemas", () => {
    expect(quoteTable("sales.Invoices", "tsql")).toBe("[sales].[Invoices]");
    expect(quoteTable("public.orders", "ansi")).toBe(`"public"."orders"`);
    expect(quoteTable("odd.name", "mysql")).toBe("`odd.name`");
    expect(quoteTable("Warehouses", "tsql")).toBe("[Warehouses]");
  });
});

describe("sqlLiteral", () => {
  it("quotes text, escaping quotes and (on MySQL) backslashes", () => {
    expect(sqlLiteral(null, undefined, "mysql")).toBe("NULL");
    expect(sqlLiteral("it's", undefined, "mysql")).toBe("'it''s'");
    expect(sqlLiteral(String.raw`C:\tmp`, undefined, "mysql")).toBe(String.raw`'C:\\tmp'`);
    expect(sqlLiteral(String.raw`C:\tmp`, undefined, "tsql")).toBe(String.raw`N'C:\tmp'`);
    expect(sqlLiteral("it's", undefined, "ansi")).toBe("'it''s'");
    expect(sqlLiteral(String.raw`C:\tmp`, undefined, "ansi")).toBe(String.raw`'C:\tmp'`);
  });

  it("leaves numbers bare on numeric columns and hex bare on binary columns", () => {
    expect(sqlLiteral("12.5", column("price", { numeric: true }), "mysql")).toBe("12.5");
    expect(sqlLiteral("-3e2", column("price", { numeric: true }), "tsql")).toBe("-3e2");
    expect(sqlLiteral("12abc", column("price", { numeric: true }), "mysql")).toBe("'12abc'");
    expect(sqlLiteral("12", column("code"), "mysql")).toBe("'12'");
    expect(sqlLiteral("0xFF", column("hash", { binary: true }), "mysql")).toBe("0xFF");
    expect(sqlLiteral("0xFF", column("hash", { binary: true }), "tsql")).toBe("0xFF");
    expect(sqlLiteral("0xFF", column("hash", { binary: true }), "ansi")).toBe("'0xFF'");
  });

  it("escapes the bracket wildcard in SQL Server LIKE patterns", () => {
    expect(containsPattern("a[b", "tsql")).toBe("N'%a[[]b%'");
    expect(containsPattern("a[b", "mysql")).toBe("'%a[b%'");
  });
});

describe("equalsFilter / describeKey", () => {
  const id = column("id", { numeric: true });
  const pairs = [
    { column: "id", value: "1", meta: id },
    { column: "deleted_at", value: null },
  ];

  it("joins column = value pairs with AND and uses IS NULL for nulls", () => {
    expect(equalsFilter(pairs, "mysql")).toBe("`id` = 1 AND `deleted_at` IS NULL");
    expect(describeKey([{ column: "code", value: "A1" }], "mysql")).toBe("`code` = 'A1'");
  });

  it("quotes foreign-key navigation filters for SQL Server and ANSI engines", () => {
    expect(equalsFilter(pairs, "tsql")).toBe("[id] = 1 AND [deleted_at] IS NULL");
    expect(equalsFilter([{ column: "CustomerCode", value: "C-4" }], "tsql")).toBe("[CustomerCode] = N'C-4'");
    expect(equalsFilter(pairs, "ansi")).toBe(`"id" = 1 AND "deleted_at" IS NULL`);
  });
});

describe("rowKey", () => {
  const columns = [column("id", { numeric: true }), column("name"), column("embedding", { vectorDims: 3 })];
  const row = ["7", "Ann", "[0.1, 0.2, 0.3]"];

  it("takes the primary-key cells", () => {
    expect(rowKey(columns, row, ["id"])).toEqual([{ column: "id", value: "7" }]);
  });

  it("falls back to every non-vector column when there is no primary key", () => {
    expect(rowKey(columns, row, [])).toEqual([
      { column: "id", value: "7" },
      { column: "name", value: "Ann" },
    ]);
  });
});
