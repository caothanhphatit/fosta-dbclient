/**
 * The order of typed cells (`Value`) for sorting loaded rows in the browser (the query result grid): numbers by value
 * (64-bit integers exactly), dates and times by instant, booleans false first, text in natural order ("item 2" before
 * "item 10"). NULLs sort last in both directions. Pure, so it is unit-tested.
 */
import type { Value } from "./contract";
import { displayValue } from "./values";

const collator = new Intl.Collator("en", { numeric: true, sensitivity: "base" });

/** Rank of each family, so cells of different kinds in one column (a document field) still sort consistently. */
function family(value: Value): number {
  switch (value.t) {
    case "bool":
      return 0;
    case "int":
    case "float":
    case "decimal":
      return 1;
    case "date":
    case "time":
    case "timestamp":
    case "timestamptz":
      return 2;
    default:
      return 3;
  }
}

function sign(difference: number): number {
  return difference < 0 ? -1 : difference > 0 ? 1 : 0;
}

function integer(value: string | number): bigint | null {
  const text = String(value).trim();
  return /^[+-]?\d+$/.test(text) ? BigInt(text) : null;
}

function compareNumbers(a: Value & { v: unknown }, b: Value & { v: unknown }): number {
  const left = integer(a.v as string | number);
  const right = integer(b.v as string | number);
  if (left !== null && right !== null) return left < right ? -1 : left > right ? 1 : 0;
  const x = Number(a.v);
  const y = Number(b.v);
  if (Number.isNaN(x) || Number.isNaN(y)) return Number.isNaN(x) === Number.isNaN(y) ? 0 : Number.isNaN(x) ? 1 : -1;
  return sign(x - y);
}

function compareTimes(a: string, b: string): number {
  const x = Date.parse(a);
  const y = Date.parse(b);
  // Times of day and dates outside what `Date` reads compare as text (ISO text sorts chronologically).
  if (Number.isNaN(x) || Number.isNaN(y) || x === y) return collator.compare(a, b);
  return sign(x - y);
}

/** Ascending order of two non-NULL cells. */
function compareNonNull(a: Value, b: Value): number {
  const familyA = family(a);
  const familyB = family(b);
  if (familyA !== familyB) return familyA - familyB;
  if (a.t === "bool" && b.t === "bool") return Number(a.v) - Number(b.v);
  if (familyA === 1) return compareNumbers(a, b);
  if (familyA === 2) return compareTimes(String(a.v), String(b.v));
  return collator.compare(displayValue(a) ?? "", displayValue(b) ?? "");
}

/** Compares two cells for a sort in `descending` order or not; NULL (and a missing cell) always goes last. */
export function compareValues(a: Value | undefined, b: Value | undefined, descending = false): number {
  const nullA = a === undefined || a.t === "null";
  const nullB = b === undefined || b.t === "null";
  if (nullA || nullB) return nullA === nullB ? 0 : nullA ? 1 : -1;
  const order = compareNonNull(a, b);
  return descending ? -order : order;
}

/** Row indexes ordered by the cell at `column` (stable: equal cells keep their order). */
export function sortIndexes(indexes: number[], cell: (index: number) => Value | undefined, descending: boolean): number[] {
  return indexes
    .map((index, position) => ({ index, position, value: cell(index) }))
    .sort((x, y) => compareValues(x.value, y.value, descending) || x.position - y.position)
    .map((item) => item.index);
}
