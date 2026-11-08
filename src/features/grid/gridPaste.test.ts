import { describe, expect, it } from "vitest";
import type { ColumnInfo, Value, ValueKind } from "../../core/contract";
import type { GridRow } from "./gridModel";
import { pasteBlock, type PasteTarget } from "./gridPaste";

const col = (name: string, kind: ValueKind["kind"]): ColumnInfo => ({
  name,
  typeName: kind,
  kind: { kind } as ValueKind,
  nullable: true,
  default: null,
  comment: null,
  identity: null,
  generated: null,
  hints: {},
});

const text = (v: string): Value => ({ t: "text", v });
const int = (v: number): Value => ({ t: "int", v });

function row(values: Value[], deleted = false): GridRow {
  return { original: values, current: [...values], deleted, version: null, touched: new Set(), edited: false, removed: false, element: null } as GridRow;
}

function target(rows: GridRow[]): PasteTarget {
  const visible = rows.map((_, index) => index);
  return {
    columns: [col("id", "int"), col("name", "text")],
    rows,
    visible,
    visiblePosition: new Map(visible.map((index) => [index, index])),
    displayed: [0, 1],
  };
}

describe("pasteBlock", () => {
  it("writes a block over the cells to the right and below", () => {
    const rows = [row([int(1), text("a")]), row([int(2), text("b")])];
    const result = pasteBlock(target(rows), { row: 0, column: 1 }, "x\ny");
    expect(result).toEqual({ pasted: 2, errors: [], changedRows: [0, 1] });
    expect(rows[0].current[1]).toEqual(text("x"));
    expect(rows[1].current[1]).toEqual(text("y"));
    expect(rows[1].edited).toBe(true);
    expect([...rows[1].touched]).toEqual([1]);
  });

  it("reports fields that do not parse and keeps the old value", () => {
    const rows = [row([int(1), text("a")])];
    const result = pasteBlock(target(rows), { row: 0, column: 0 }, "nope\tz");
    expect(result.pasted).toBe(1);
    expect(result.errors).toHaveLength(1);
    expect(result.errors[0].startsWith("id: ")).toBe(true);
    expect(rows[0].current[0]).toEqual(int(1));
    expect(rows[0].current[1]).toEqual(text("z"));
  });

  it("skips deleted rows and cells past the last column or row", () => {
    const rows = [row([int(1), text("a")], true), row([int(2), text("b")])];
    const result = pasteBlock(target(rows), { row: 0, column: 1 }, "p\tq\nr\ts");
    expect(result.changedRows).toEqual([1]);
    expect(rows[0].current[1]).toEqual(text("a"));
    expect(rows[1].current[1]).toEqual(text("r"));
  });
});
