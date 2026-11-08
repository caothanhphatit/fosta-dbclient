import { describe, expect, it } from "vitest";
import type { ColumnMeta } from "../../core/types";
import { columnCondition, combinedCondition, combinedJsonFilter, matchesFilter } from "./columnFilter";

const column = (name: string, kind: Partial<ColumnMeta> = {}): ColumnMeta => ({
  name,
  typeName: "varchar",
  numeric: false,
  binary: false,
  primary: false,
  vectorDims: null,
  ...kind,
});

const name = column("name");
const price = column("price", { typeName: "decimal", numeric: true });
const hash = column("hash", { typeName: "varbinary", binary: true });

describe("columnCondition", () => {
  it("turns plain text into a case-insensitive contains", () => {
    expect(columnCondition(name, "abc", "mysql")).toBe("`name` LIKE '%abc%'");
    expect(columnCondition(name, "O'Brien", "mysql")).toBe("`name` LIKE '%O''Brien%'");
  });

  it("compares numbers on numeric columns and hex on binary ones", () => {
    expect(columnCondition(price, "42", "mysql")).toBe("`price` = 42");
    expect(columnCondition(name, "42", "mysql")).toBe("`name` LIKE '%42%'");
    expect(columnCondition(hash, "0xABCD", "mysql")).toBe("`hash` = 0xABCD");
  });

  it("understands comparison operators, with != written as <>", () => {
    expect(columnCondition(price, ">= 10", "mysql")).toBe("`price` >= 10");
    expect(columnCondition(name, "!=x", "mysql")).toBe("`name` <> 'x'");
    expect(columnCondition(name, "<b", "mysql")).toBe("`name` < 'b'");
  });

  it("understands null and not null", () => {
    expect(columnCondition(name, "NULL", "mysql")).toBe("`name` IS NULL");
    expect(columnCondition(name, "!null", "mysql")).toBe("`name` IS NOT NULL");
    expect(columnCondition(name, "not null", "mysql")).toBe("`name` IS NOT NULL");
  });

  it("ignores empty filters", () => {
    expect(columnCondition(name, "   ", "mysql")).toBeNull();
  });
});

describe("combinedCondition", () => {
  it("returns null without filters and a bare condition for one", () => {
    expect(combinedCondition([name, price], {}, " ", "mysql")).toBeNull();
    expect(combinedCondition([name, price], { price: ">5" }, "", "mysql")).toBe("`price` > 5");
  });

  it("wraps several conditions in parentheses, the WHERE text first", () => {
    expect(combinedCondition([name, price], { name: "x", price: "<9" }, "status = 1", "mysql")).toBe("(status = 1) AND (`name` LIKE '%x%') AND (`price` < 9)");
  });
});

describe("column filters per dialect", () => {
  it("uses brackets and N'…' strings on SQL Server", () => {
    const status = column("Status");
    expect(columnCondition(status, "paid", "tsql")).toBe("[Status] LIKE N'%paid%'");
    expect(columnCondition(status, "a[b", "tsql")).toBe("[Status] LIKE N'%a[[]b%'");
    expect(columnCondition(column("we]ird"), "=x", "tsql")).toBe("[we]]ird] = N'x'");
    expect(columnCondition(price, "42", "tsql")).toBe("[price] = 42");
    expect(columnCondition(hash, "0xABCD", "tsql")).toBe("[hash] = 0xABCD");
    expect(columnCondition(status, String.raw`C:\tmp`, "tsql")).toBe(String.raw`[Status] LIKE N'%C:\tmp%'`);
    expect(combinedCondition([status, price], { Status: "paid", price: ">5" }, "", "tsql")).toBe("([Status] LIKE N'%paid%') AND ([price] > 5)");
  });

  it("uses double quotes and standard strings in ANSI SQL", () => {
    expect(columnCondition(name, "O'Brien", "ansi")).toBe(`"name" LIKE '%O''Brien%'`);
    expect(columnCondition(name, String.raw`a\b`, "ansi")).toBe(String.raw`"name" LIKE '%a\b%'`);
    expect(columnCondition(hash, "0xABCD", "ansi")).toBe(`"hash" = '0xABCD'`);
    expect(columnCondition(name, "null", "ansi")).toBe(`"name" IS NULL`);
  });
});

describe("combinedJsonFilter (MongoDB)", () => {
  const parse = (value: string | null) => (value === null ? null : JSON.parse(value));

  it("turns text into a case-insensitive regex with special characters escaped", () => {
    expect(parse(combinedJsonFilter([name], { name: "a.b" }, ""))).toEqual({ name: { $regex: String.raw`a\.b`, $options: "i" } });
  });

  it("maps comparison operators and typed operands", () => {
    expect(parse(combinedJsonFilter([price], { price: ">5" }, ""))).toEqual({ price: { $gt: 5 } });
    expect(parse(combinedJsonFilter([name], { name: "=true" }, ""))).toEqual({ name: { $eq: true } });
    expect(parse(combinedJsonFilter([name], { name: '!="x"' }, ""))).toEqual({ name: { $ne: "x" } });
    expect(parse(combinedJsonFilter([price], { price: "7" }, ""))).toEqual({ price: 7 });
  });

  it("combines the JSON query and column filters with $and", () => {
    expect(parse(combinedJsonFilter([name, price], { name: "null", price: "!null" }, '{ "status": "ACTIVE" }'))).toEqual({
      $and: [{ status: "ACTIVE" }, { name: null }, { price: { $ne: null } }],
    });
    expect(combinedJsonFilter([name], {}, "")).toBeNull();
  });

  it("rejects a filter that is not JSON", () => {
    expect(() => combinedJsonFilter([name], {}, "status = 1")).toThrow("JSON query document");
  });
});

describe("matchesFilter (client-side filter of loaded rows)", () => {
  it("matches text case-insensitively and handles null", () => {
    expect(matchesFilter("Hello", "ell")).toBe(true);
    expect(matchesFilter("Hello", "xyz")).toBe(false);
    expect(matchesFilter(null, "null")).toBe(true);
    expect(matchesFilter(null, "x")).toBe(false);
    expect(matchesFilter("x", "!null")).toBe(true);
    expect(matchesFilter("x", "")).toBe(true);
  });

  it("compares numerically when both sides are numbers, otherwise by text", () => {
    expect(matchesFilter("10", ">9")).toBe(true);
    expect(matchesFilter("10", "=10.0")).toBe(true);
    expect(matchesFilter("b", "<c")).toBe(true);
    expect(matchesFilter("b", "<>b")).toBe(false);
  });
});
