import { describe, expect, it } from "vitest";
import {
  columnStorageKey,
  hideAllButFirst,
  loadHiddenColumns,
  matchColumns,
  nearestVisibleColumn,
  normalizeHidden,
  setColumnHidden,
  stepVisibleColumn,
  visibleColumnIndexes,
} from "./columnVisibility";

const COLUMNS = ["id", "sku", "name", "price"];

describe("column visibility", () => {
  it("builds the storage key per connection, database and table", () => {
    expect(columnStorageKey("c-1", "shop", "products")).toBe("db-studio.columns.c-1.shop.products");
  });

  it("returns no hidden columns when storage is unavailable", () => {
    expect(loadHiddenColumns("db-studio.columns.x.y.z")).toEqual([]);
  });

  it("lists visible indexes and keeps one column when everything is hidden", () => {
    expect(visibleColumnIndexes(COLUMNS, ["sku", "missing"])).toEqual([0, 2, 3]);
    expect(visibleColumnIndexes(COLUMNS, COLUMNS)).toEqual([0]);
    expect(visibleColumnIndexes([], ["x"])).toEqual([]);
  });

  it("normalises hidden names to existing columns", () => {
    expect(normalizeHidden(COLUMNS, ["price", "gone", "sku"])).toEqual(["sku", "price"]);
    expect(normalizeHidden(COLUMNS, COLUMNS)).toEqual(["sku", "name", "price"]);
  });

  it("hides and shows one column but never the last visible one", () => {
    expect(setColumnHidden(COLUMNS, [], "name", true)).toEqual(["name"]);
    expect(setColumnHidden(COLUMNS, ["name"], "name", false)).toEqual([]);
    expect(setColumnHidden(COLUMNS, ["id", "sku", "name"], "price", true)).toEqual(["id", "sku", "name"]);
    expect(setColumnHidden(COLUMNS, ["sku"], "unknown", true)).toEqual(["sku"]);
  });

  it("hides all but the first column", () => {
    expect(hideAllButFirst(COLUMNS)).toEqual(["sku", "name", "price"]);
  });

  it("steps over hidden columns", () => {
    const visible = [0, 2, 3];
    expect(stepVisibleColumn(visible, 0, 1)).toBe(2);
    expect(stepVisibleColumn(visible, 2, -1)).toBe(0);
    expect(stepVisibleColumn(visible, 3, 1)).toBe(3);
    expect(stepVisibleColumn(visible, 0, -1)).toBe(0);
    // From a hidden column: snap to the next visible one, then move.
    expect(stepVisibleColumn(visible, 1, 1)).toBe(3);
  });

  it("snaps to the nearest visible column", () => {
    expect(nearestVisibleColumn([0, 2, 3], 1)).toBe(2);
    expect(nearestVisibleColumn([0, 1], 3)).toBe(1);
    expect(nearestVisibleColumn([0, 2], 2)).toBe(2);
  });

  it("filters the column list by name or type", () => {
    const columns = [
      { name: "id", typeName: "int(11)" },
      { name: "Name", typeName: "varchar(120)" },
      { name: "created", typeName: "datetime" },
    ];
    expect(matchColumns(columns, "")).toEqual([0, 1, 2]);
    expect(matchColumns(columns, "NAME")).toEqual([1]);
    expect(matchColumns(columns, "int")).toEqual([0]);
  });
});
