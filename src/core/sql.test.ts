import { describe, expect, it } from "vitest";
import { describeKey, equalsFilter, quoteIdent, rowKey, sqlLiteral } from "./sql";
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

describe("quoteIdent", () => {
  it("wraps in backticks and doubles embedded ones", () => {
    expect(quoteIdent("orders")).toBe("`orders`");
    expect(quoteIdent("we`ird")).toBe("`we``ird`");
  });
});

describe("sqlLiteral", () => {
  it("quotes text, escaping quotes and backslashes", () => {
    expect(sqlLiteral(null)).toBe("NULL");
    expect(sqlLiteral("it's")).toBe("'it''s'");
    expect(sqlLiteral(String.raw`C:\tmp`)).toBe(String.raw`'C:\\tmp'`);
  });

  it("leaves numbers bare on numeric columns and hex bare on binary columns", () => {
    expect(sqlLiteral("12.5", column("price", { numeric: true }))).toBe("12.5");
    expect(sqlLiteral("-3e2", column("price", { numeric: true }))).toBe("-3e2");
    expect(sqlLiteral("12abc", column("price", { numeric: true }))).toBe("'12abc'");
    expect(sqlLiteral("12", column("code"))).toBe("'12'");
    expect(sqlLiteral("0xFF", column("hash", { binary: true }))).toBe("0xFF");
  });
});

describe("equalsFilter / describeKey", () => {
  it("joins column = value pairs with AND and uses IS NULL for nulls", () => {
    const id = column("id", { numeric: true });
    expect(
      equalsFilter([
        { column: "id", value: "1", meta: id },
        { column: "deleted_at", value: null },
      ]),
    ).toBe("`id` = 1 AND `deleted_at` IS NULL");
    expect(describeKey([{ column: "code", value: "A1" }])).toBe("`code` = 'A1'");
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
