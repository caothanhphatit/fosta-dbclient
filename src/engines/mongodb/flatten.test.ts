import { describe, expect, it } from "vitest";
import { columnTypeText, flatten, isOddCell } from "./flatten";

describe("flattened documents", () => {
  it("opens embedded documents into dotted paths and keeps arrays as one cell", () => {
    const table = flatten([
      { _id: 1, address: { city: "Oslo", geo: { lat: 1 } }, tags: ["a", "b"] },
      { _id: 2, address: { city: "Lisbon" }, extra: true },
    ]);
    expect(table.columns.map((column) => column.path)).toEqual(["_id", "address.city", "address.geo.lat", "tags", "extra"]);
    expect(table.rows[0][3]).toEqual({ missing: false, value: ["a", "b"] });
    expect(table.rows[1][2]).toEqual({ missing: true });
    expect(table.rows[1][4]).toEqual({ missing: false, value: true });
    expect(table.columns[2].present).toBe(1);
    expect(table.hiddenPaths).toBe(0);
  });

  it("reads Extended JSON wrappers as values, not documents", () => {
    const table = flatten([{ _id: { $oid: "68e783b8c0ffee0000000001" }, at: { $date: "2026-01-01T00:00:00Z" } }]);
    expect(table.columns.map((column) => [column.path, columnTypeText(column)])).toEqual([
      ["_id", "ObjectId"],
      ["at", "Date"],
    ]);
  });

  it("stops opening documents at the depth and keeps empty documents as cells", () => {
    const table = flatten([{ a: { b: { c: { d: 1 } } }, e: {} }], { maxDepth: 2 });
    expect(table.columns.map((column) => column.path)).toEqual(["a.b", "e"]);
    expect(table.rows[0][0]).toEqual({ missing: false, value: { c: { d: 1 } } });
  });

  it("flags columns of mixed types, not counting null and missing", () => {
    const table = flatten([{ rating: 4 }, { rating: "n/a" }, { rating: 5 }, { rating: null }, {}]);
    const [rating] = table.columns;
    expect(rating.mixed).toBe(true);
    expect(rating.types).toEqual(["number", "string"]);
    expect(columnTypeText(rating)).toBe("Mixed: Number, String");
    expect(isOddCell(rating, table.rows[1][0])).toBe(true);
    expect(isOddCell(rating, table.rows[0][0])).toBe(false);
    expect(isOddCell(rating, table.rows[3][0])).toBe(false);
    expect(isOddCell(rating, table.rows[4][0])).toBe(false);
    const plain = flatten([{ n: 1 }, { n: null }]).columns[0];
    expect(plain.mixed).toBe(false);
    expect(columnTypeText(flatten([{ n: null }]).columns[0])).toBe("Null");
  });

  it("caps the columns, keeping the paths most documents have in first-seen order", () => {
    const wide = Object.fromEntries(Array.from({ length: 50 }, (_, index) => [`f${index}`, index]));
    const table = flatten(
      [
        { _id: 1, ...wide, last: 1 },
        { _id: 2, last: 2 },
        { _id: 3, last: 3 },
      ],
      { maxColumns: 10 },
    );
    expect(table.columns).toHaveLength(10);
    expect(table.columns[0].path).toBe("_id");
    expect(table.columns[table.columns.length - 1].path).toBe("last");
    expect(table.columns.slice(1, -1).map((column) => column.path)).toEqual(["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7"]);
    expect(table.hiddenPaths).toBe(42);
    expect(table.rows[1]).toHaveLength(10);
  });
});
