import { describe, expect, it } from "vitest";
import type { Value } from "./contract";
import { compareValues, parseDecimal, parseTimestamptz, sortIndexes, sortKey } from "./valueOrder";

const int = (v: number | string): Value => ({ t: "int", v });
const float = (v: number | string): Value => ({ t: "float", v });
const decimal = (v: string): Value => ({ t: "decimal", v });
const text = (v: string): Value => ({ t: "text", v });
const tz = (v: string): Value => ({ t: "timestamptz", v });
const NULL: Value = { t: "null", v: null };

function sorted(values: Value[], descending = false): Value[] {
  return sortIndexes(
    values.map((_, index) => index),
    (index) => values[index],
    descending,
  ).map((index) => values[index]);
}

const texts = (values: Value[]) => values.map((value) => String(value.v));

describe("numbers", () => {
  it("orders numbers by value, not as text", () => {
    expect(sorted([int(10), int(9), int(-2)])).toEqual([int(-2), int(9), int(10)]);
    expect(sorted([float(1.5), decimal("1.25"), int(2)])).toEqual([decimal("1.25"), float(1.5), int(2)]);
  });

  it("compares decimals exactly as text: sign, integer length, digits, fraction", () => {
    expect(compareValues(decimal("-10"), decimal("-5"))).toBe(-1);
    expect(compareValues(decimal("1.10"), decimal("1.5"))).toBe(-1);
    expect(compareValues(decimal("1.50"), decimal("1.5"))).toBe(0);
    expect(compareValues(decimal("-0.00"), decimal("0"))).toBe(0);
    expect(compareValues(decimal("0.1"), decimal("0.09"))).toBe(1);
    expect(compareValues(decimal("-0.1"), decimal("-0.09"))).toBe(-1);
    expect(texts(sorted([decimal("10.5"), decimal("-10"), decimal("9.99"), decimal("-5"), decimal("0"), decimal("1.10"), decimal("1.5")]))).toEqual([
      "-10",
      "-5",
      "0",
      "1.10",
      "1.5",
      "9.99",
      "10.5",
    ]);
  });

  it("orders values beyond 2^53 exactly", () => {
    expect(compareValues(int("9223372036854775807"), int("9223372036854775806"))).toBe(1);
    expect(compareValues(int("-9223372036854775808"), int("-9223372036854775807"))).toBe(-1);
    // 2^53 + 1 and 2^53 are the same JS number.
    expect(compareValues(decimal("9007199254740993"), decimal("9007199254740992"))).toBe(1);
    expect(compareValues(decimal("12345678901234567890.000000000000000001"), decimal("12345678901234567890"))).toBe(1);
    expect(compareValues(int("9007199254740993"), int(9007199254740992))).toBe(1);
  });

  it("reads floats in exponent form and puts NaN and the infinities in their place", () => {
    expect(compareValues(float(1e21), float(9e20))).toBe(1);
    expect(compareValues(float(1e-7), float(2e-8))).toBe(1);
    expect(compareValues(float("1.5E+3"), int(1499))).toBe(1);
    expect(texts(sorted([float("NaN"), float("Infinity"), float(1), float("-Infinity")]))).toEqual(["-Infinity", "1", "Infinity", "NaN"]);
    expect(parseDecimal("1e5")).toEqual({ kind: 1, negative: false, integer: "100000", fraction: "" });
    expect(parseDecimal("-.5")).toEqual({ kind: 1, negative: true, integer: "", fraction: "5" });
    expect(parseDecimal("abc")).toBeNull();
    expect(parseDecimal(".")).toBeNull();
  });
});

describe("text", () => {
  it("orders numeric-looking text by its number, before other text", () => {
    expect(texts(sorted([text("-5"), text("10"), text("-10"), text("abc"), text("2")]))).toEqual(["-10", "-5", "2", "10", "abc"]);
    expect(compareValues(text("1.10"), text("1.5"))).toBe(-1);
  });

  it("orders other text naturally and case-insensitively", () => {
    expect(sorted([text("item 10"), text("Item 2"), text("apple")])).toEqual([text("apple"), text("Item 2"), text("item 10")]);
  });

  it("is a consistent order when numeric and other text mix", () => {
    const values = [text("10"), text("9a"), text("9"), text("a"), text("-1")];
    const once = sorted(values);
    expect(sorted([...values].reverse())).toEqual(once);
    expect(texts(once)).toEqual(["-1", "9", "10", "9a", "a"]);
  });
});

describe("dates and timestamps", () => {
  it("compares dates and timestamps as ISO text, years of any length in order", () => {
    const date = (v: string): Value => ({ t: "date", v });
    expect(texts(sorted([date("2024-03-01"), date("10000-01-01"), date("0099-12-31"), date("-0044-03-15"), date("2024-02-29")]))).toEqual([
      "-0044-03-15",
      "0099-12-31",
      "2024-02-29",
      "2024-03-01",
      "10000-01-01",
    ]);
    const stamp = (v: string): Value => ({ t: "timestamp", v });
    expect(compareValues(stamp("2024-01-01 10:00:00.5"), stamp("2024-01-01 10:00:00.25"))).toBe(1);
    const time = (v: string): Value => ({ t: "time", v });
    expect(compareValues(time("09:00:00"), time("10:00:00"))).toBe(-1);
  });

  it("orders timestamps with a time zone by instant", () => {
    const later = tz("2024-01-01 10:00:00+00:00");
    const earlier = tz("2024-01-01 11:00:00+02:00");
    expect(compareValues(earlier, later)).toBe(-1);
    expect(compareValues(tz("2024-01-01T00:30:00-01:00"), tz("2024-01-01 01:30:00Z"))).toBe(0);
    expect(compareValues(tz("2024-01-01 00:00:00.000001+00"), tz("2024-01-01 00:00:00+00"))).toBe(1);
    // Across a day, a month and a year boundary through the offset.
    expect(compareValues(tz("2024-01-01 00:30:00+01:00"), tz("2023-12-31 23:45:00+00:00"))).toBe(-1);
    expect(compareValues(tz("-0001-06-01 00:00:00+00:00"), tz("0001-01-01 00:00:00+00:00"))).toBe(-1);
  });

  it("reads timestamps with a time zone strictly", () => {
    expect(parseTimestamptz("1970-01-01 00:00:00+00:00")).toEqual({ instant: 0, fraction: "" });
    expect(parseTimestamptz("1970-01-02T00:00:00.500Z")).toEqual({ instant: 86_400, fraction: "5" });
    expect(parseTimestamptz("2026-01-02 03:04:05.5-09:30")?.instant).toBe(Date.UTC(2026, 0, 2, 12, 34, 5) / 1000);
    for (const bad of ["2024-13-01 00:00:00+00", "2024-01-01 00:00:00", "Jan 1 2024 00:00 GMT", "2024-01-01 00:00:00+0", "2024-01-01 25:00:00Z"]) {
      expect(parseTimestamptz(bad)).toBeNull();
    }
    // What does not read still sorts, as text with the dates.
    expect(sortKey(tz("someday")).family).toBe(3);
  });
});

describe("compareValues and sortIndexes", () => {
  it("keeps NULL last in both directions", () => {
    expect(sorted([NULL, int(1), int(3)])).toEqual([int(1), int(3), NULL]);
    expect(sorted([NULL, int(1), int(3)], true)).toEqual([int(3), int(1), NULL]);
    expect(compareValues(undefined, int(1))).toBe(1);
  });

  it("orders booleans false first and keeps families apart", () => {
    expect(
      sorted([
        { t: "bool", v: true },
        { t: "bool", v: false },
      ]),
    ).toEqual([
      { t: "bool", v: false },
      { t: "bool", v: true },
    ]);
    expect(texts(sorted([text("x"), int(5), { t: "bool", v: true }, { t: "date", v: "2024-01-01" }]))).toEqual(["true", "5", "2024-01-01", "x"]);
  });

  it("is stable for equal cells", () => {
    const values = [int(1), int(1), int(0)];
    expect(sortIndexes([0, 1, 2], (index) => values[index], false)).toEqual([2, 0, 1]);
  });

  it("reads each cell once per sort", () => {
    const values = Array.from({ length: 200 }, (_, index) => decimal(String((index * 7919) % 200)));
    let reads = 0;
    sortIndexes(
      values.map((_, index) => index),
      (index) => {
        reads += 1;
        return values[index];
      },
      false,
    );
    expect(reads).toBe(values.length);
  });
});
