import { describe, expect, it } from "vitest";
import { documentPaths, foldedText, isNested, isWrapper, scalarText } from "./documents";

describe("document values", () => {
  it("reads Extended JSON wrappers as one value", () => {
    expect(scalarText({ $oid: "68e783b8c0ffee0000000001" })).toEqual({ text: 'ObjectId("68e783b8c0ffee0000000001")', kind: "objectId" });
    expect(scalarText({ $date: "2026-02-02T15:01:00Z" })).toEqual({ text: 'ISODate("2026-02-02T15:01:00Z")', kind: "date" });
    expect(scalarText({ $date: { $numberLong: "0" } }).text).toBe('ISODate("1970-01-01T00:00:00.000Z")');
    expect(scalarText({ $numberDecimal: "1.50" }).text).toBe('NumberDecimal("1.50")');
    expect(scalarText({ $numberLong: "9007199254740993" }).text).toBe("NumberLong(9007199254740993)");
    expect(scalarText({ $regularExpression: { pattern: "^a", options: "i" } }).text).toBe("/^a/i");
    expect(scalarText({ $binary: { base64: "AQ==", subType: "04" } }).text).toBe('Binary.createFromBase64("AQ==", 4)');
    expect(scalarText("x")).toEqual({ text: '"x"', kind: "string" });
    expect(scalarText(null).kind).toBe("null");
    expect(isWrapper({ $oid: "x" })).toBe(true);
    expect(isWrapper({ $gt: 1 })).toBe(false);
    expect(isNested({ a: 1 })).toBe(true);
    expect(isNested({ $date: "x" })).toBe(false);
  });

  it("folds nested values", () => {
    expect(foldedText([1, 2])).toBe("[ 2 items ]");
    expect(foldedText([1])).toBe("[ 1 item ]");
    expect(foldedText({ a: 1 })).toBe("{ 1 field }");
    expect(foldedText({})).toBe("{}");
  });

  it("collects dotted field paths through subdocuments and arrays, not into wrappers", () => {
    const paths = documentPaths([
      {
        _id: { $oid: "x" },
        customer: { name: "a", address: { city: "b" } },
        items: [
          { sku: "s", qty: 1 },
          { sku: "t", note: "n" },
        ],
      },
      { _id: { $oid: "y" }, extra: true },
    ]);
    expect(paths).toEqual([
      "_id",
      "customer",
      "customer.name",
      "customer.address",
      "customer.address.city",
      "items",
      "items.sku",
      "items.qty",
      "items.note",
      "extra",
    ]);
  });
});
