import { describe, expect, it } from "vitest";
import { cursor } from "../testSchema";
import type { CompletionSchema } from "../types";
import { filterValueContext, queryKeyCompletions, valueLiteral, valueOptions, withFields, type QueryBarField } from "./queryCompletion";

const SCHEMA: CompletionSchema = {
  databases: ["shop"],
  schemas: [],
  tables: [
    {
      name: "orders",
      kind: "collection",
      columns: [
        { name: "_id", type: "objectId", pk: true },
        { name: "status", type: "string", pk: false },
        { name: "total", type: "double", pk: false },
      ],
    },
  ],
};

const keys = (field: QueryBarField, text: string, explicit = true) => {
  const { doc, pos } = cursor(text);
  return queryKeyCompletions(field, "orders", SCHEMA, doc, pos, explicit);
};
const labels = (field: QueryBarField, text: string) => keys(field, text)?.options.map((o) => o.label) ?? [];
const value = (text: string) => {
  const { doc, pos } = cursor(text);
  return filterValueContext(doc, pos);
};

describe("query bar key completion", () => {
  it("offers fields and logical operators at the top of a filter, with or without braces", () => {
    expect(labels("filter", "{ |")).toEqual(expect.arrayContaining(["status", "total", "$and", "$or"]));
    expect(labels("filter", "sta|")).toContain("status");
    const result = keys("filter", "{ sta|");
    expect(result?.from).toBe(2);
    expect(result?.to).toBe(5);
    expect(keys("filter", "sta|")?.from).toBe(0);
  });

  it("offers query operators inside a field's condition", () => {
    expect(labels("filter", "{ total: { $|")).toEqual(expect.arrayContaining(["$gt", "$in", "$exists"]));
  });

  it("completes typed keys without Ctrl+Space, and nothing on an empty key unless asked", () => {
    const { doc, pos } = cursor("{ st|");
    expect(queryKeyCompletions("filter", "orders", SCHEMA, doc, pos, false)?.options.map((o) => o.label)).toContain("status");
    const empty = cursor("{ |");
    expect(queryKeyCompletions("filter", "orders", SCHEMA, empty.doc, empty.pos, false)).toBeNull();
  });

  it("offers fields in Project and Sort, and collation options", () => {
    expect(labels("project", "{ |")).toEqual(expect.arrayContaining(["status", "total"]));
    expect(labels("project", "{ |")).not.toContain("$and");
    expect(labels("sort", "tot|")).toContain("total");
    expect(labels("collation", "{ |")).toEqual(expect.arrayContaining(["locale", "strength"]));
    expect(labels("collation", "{ locale: 'en', str|")).toContain("strength");
  });

  it("adds field paths seen in the documents shown", () => {
    const merged = withFields(SCHEMA, "orders", ["status", "items.sku"]);
    expect(merged.tables[0].columns.map((c) => c.name)).toEqual(["_id", "status", "total", "items.sku"]);
    expect(withFields(SCHEMA, "new", ["a"]).tables[0]).toMatchObject({ name: "new", columns: [{ name: "a" }] });
  });
});

describe("query bar value contexts", () => {
  it("finds the field of an equality and of value operators", () => {
    expect(value("{ status: |")).toMatchObject({ field: "status", operator: null, prefix: "", quote: null });
    expect(value("{ status: 'pa|")).toMatchObject({ field: "status", prefix: "pa", quote: "'", from: 11 });
    expect(value('status: "x|')).toMatchObject({ field: "status", prefix: "x", quote: '"', from: 9 });
    expect(value("{ total: { $gt: 1|")).toMatchObject({ field: "total", operator: "$gt", prefix: "1" });
    expect(value("{ status: { $in: ['paid', '|")).toMatchObject({ field: "status", operator: "$in", quote: "'" });
  });

  it("builds dotted paths through subdocuments, $elemMatch and $or", () => {
    expect(value("{ 'customer.name': '|")?.field).toBe("customer.name");
    expect(value("{ items: { $elemMatch: { sku: '|")?.field).toBe("items.sku");
    expect(value("{ $or: [ { status: '|")?.field).toBe("status");
  });

  it("is null for keys, non-value operators, expressions and constructor arguments", () => {
    expect(value("{ sta|")).toBeNull();
    expect(value("{ status: { $exists: |")).toBeNull();
    expect(value("{ $expr: { $gt: ['$a', |")).toBeNull();
    expect(value("{ _id: ObjectId('|")).toBeNull();
    expect(value("{ status: 'a', |")).toBeNull();
  });
});

describe("sampled values as completions", () => {
  it("writes values the way a filter takes them", () => {
    expect(valueLiteral("paid")).toBe('"paid"');
    expect(valueLiteral(12.5)).toBe("12.5");
    expect(valueLiteral({ $oid: "68e783b8c0ffee0000000001" })).toBe('ObjectId("68e783b8c0ffee0000000001")');
    expect(valueLiteral({ $date: "2026-02-02T15:01:00Z" })).toBe('ISODate("2026-02-02T15:01:00Z")');
    expect(valueLiteral({ $date: { $numberLong: "0" } })).toBe('ISODate("1970-01-01T00:00:00.000Z")');
    expect(valueLiteral({ $numberDecimal: "61.50" })).toBe('NumberDecimal("61.50")');
    expect(valueLiteral({ nested: 1 })).toBeNull();
    expect(valueLiteral([1])).toBeNull();
  });

  it("inside a string offers only strings, escaped for the quote and closing it", () => {
    const options = valueOptions(["it's", 'say "hi"', 3], { quote: "'" }, "");
    expect(options.map((o) => o.apply)).toEqual(["it\\'s'", 'say "hi"\'']);
    expect(valueOptions(["paid"], { quote: '"' }, '"')[0].apply).toBe("paid");
    expect(valueOptions(["paid", 3, null], { quote: null }, "").map((o) => o.label)).toEqual(['"paid"', "3"]);
  });
});
