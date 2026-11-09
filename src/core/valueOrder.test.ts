import { describe, expect, it } from "vitest";
import type { Value } from "./contract";
import { compareValues, sortIndexes } from "./valueOrder";

const int = (v: number | string): Value => ({ t: "int", v });
const float = (v: number | string): Value => ({ t: "float", v });
const text = (v: string): Value => ({ t: "text", v });
const NULL: Value = { t: "null", v: null };

function sorted(values: Value[], descending = false): Value[] {
  return sortIndexes(
    values.map((_, index) => index),
    (index) => values[index],
    descending,
  ).map((index) => values[index]);
}

describe("compareValues", () => {
  it("orders numbers by value, not as text", () => {
    expect(sorted([int(10), int(9), int(-2)])).toEqual([int(-2), int(9), int(10)]);
    expect(sorted([float(1.5), { t: "decimal", v: "1.25" }, int(2)])).toEqual([{ t: "decimal", v: "1.25" }, float(1.5), int(2)]);
  });

  it("compares 64-bit integers exactly", () => {
    expect(compareValues(int("9223372036854775807"), int("9223372036854775806"))).toBe(1);
  });

  it("keeps NULL last in both directions", () => {
    expect(sorted([NULL, int(1), int(3)])).toEqual([int(1), int(3), NULL]);
    expect(sorted([NULL, int(1), int(3)], true)).toEqual([int(3), int(1), NULL]);
  });

  it("orders text naturally and case-insensitively", () => {
    expect(sorted([text("item 10"), text("Item 2"), text("apple")])).toEqual([text("apple"), text("Item 2"), text("item 10")]);
  });

  it("orders timestamps by instant and booleans false first", () => {
    const later: Value = { t: "timestamptz", v: "2024-01-01T10:00:00+00:00" };
    const earlier: Value = { t: "timestamptz", v: "2024-01-01T11:00:00+02:00" };
    expect(compareValues(earlier, later)).toBe(-1);
    expect(
      sorted([
        { t: "bool", v: true },
        { t: "bool", v: false },
      ]),
    ).toEqual([
      { t: "bool", v: false },
      { t: "bool", v: true },
    ]);
  });

  it("is stable for equal cells", () => {
    const values = [int(1), int(1), int(0)];
    expect(sortIndexes([0, 1, 2], (index) => values[index], false)).toEqual([2, 0, 1]);
  });
});
