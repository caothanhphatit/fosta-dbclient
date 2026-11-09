/**
 * The order of typed cells (`Value`) for sorting loaded rows in the browser (the query result grid). Pure, so it is
 * unit-tested.
 *
 * - Numbers (ints, floats, decimals) compare exactly, as decimal text: sign, then the length of the integer part, then
 *   its digits, then the fraction ("-10" < "-5", "1.10" < "1.5", integers beyond 2^53 in order). NaN sorts after every
 *   number, -Infinity and Infinity at their ends.
 * - Text that looks like a number ("-5", "3.25") sorts by that number, before other text; other text in natural order
 *   ("item 2" before "item 10"), case-insensitively.
 * - Dates and timestamps compare as ISO text (years of any length and sign in order); a timestamp with a time zone by
 *   its instant, read with a strict pattern (no `Date.parse`).
 * - Booleans false first. Families never mix: booleans, then numbers, then dates and times, then text.
 * - NULLs (and missing cells) sort last in both directions.
 *
 * `sortKey` reads a cell once; a sort compares the keys (`sortIndexes`), not the cells.
 */
import type { Value } from "./contract";
import { displayValue } from "./values";

const collator = new Intl.Collator("en", { numeric: true, sensitivity: "base" });

/** A number as exact decimal text. `kind`: 0 -Infinity, 1 finite, 2 Infinity, 3 NaN (or unreadable). */
export interface NumberKey {
  kind: 0 | 1 | 2 | 3;
  negative: boolean;
  /** Integer digits without leading zeros ("" for zero). */
  integer: string;
  /** Fraction digits without trailing zeros. */
  fraction: string;
}

/** Families in their order: booleans, numbers, instants, ISO text, numeric-looking text, text (NULL: -1). */
export type SortKey =
  | { family: -1 }
  | { family: 0; value: number }
  | { family: 1; number: NumberKey }
  | { family: 2; instant: number; fraction: string }
  | { family: 3; year: NumberKey | null; text: string }
  | { family: 4; number: NumberKey }
  | { family: 5; text: string };

const NUMBER = /^\s*([+-])?(\d*)(?:\.(\d*))?(?:[eE]([+-]?\d{1,4}))?\s*$/;
/** Digits beyond this many from an exponent are not spelled out (such a value still sorts, as unreadable). */
const MAX_SHIFT = 1200;

const NAN: NumberKey = { kind: 3, negative: false, integer: "", fraction: "" };

/** Reads decimal text (with an optional exponent) exactly; null when it is not a number. */
export function parseDecimal(text: string): NumberKey | null {
  const match = NUMBER.exec(text);
  if (!match) return null;
  const [, sign, whole = "", part = "", exponent] = match;
  if (whole === "" && part === "") return null;
  let digits = whole + part;
  let point = whole.length + (exponent ? Number(exponent) : 0);
  if (Math.abs(point) > MAX_SHIFT) return null;
  if (point < 0) {
    digits = "0".repeat(-point) + digits;
    point = 0;
  } else if (point > digits.length) {
    digits += "0".repeat(point - digits.length);
  }
  const integer = digits.slice(0, point).replace(/^0+/, "");
  const fraction = digits.slice(point).replace(/0+$/, "");
  const zero = integer === "" && fraction === "";
  return { kind: 1, negative: sign === "-" && !zero, integer, fraction };
}

function numberKey(value: number | string): NumberKey {
  const text = String(value).trim();
  const lower = text.toLowerCase();
  if (lower === "nan") return NAN;
  if (/^[+]?(inf|infinity)$/.test(lower)) return { kind: 2, negative: false, integer: "", fraction: "" };
  if (/^-(inf|infinity)$/.test(lower)) return { kind: 0, negative: true, integer: "", fraction: "" };
  return parseDecimal(text) ?? NAN;
}

function compareText(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Larger magnitude last, comparing digits as text. */
function compareMagnitude(a: NumberKey, b: NumberKey): number {
  if (a.integer.length !== b.integer.length) return a.integer.length - b.integer.length;
  return compareText(a.integer, b.integer) || compareText(a.fraction, b.fraction);
}

export function compareNumbers(a: NumberKey, b: NumberKey): number {
  if (a.kind !== b.kind) return a.kind - b.kind;
  if (a.kind !== 1) return 0;
  if (a.negative !== b.negative) return a.negative ? -1 : 1;
  const order = compareMagnitude(a, b);
  return a.negative ? -order : order;
}

/** Days since 1970-01-01 of a proleptic Gregorian date (exact for any year, integer arithmetic only). */
function daysFromCivil(year: number, month: number, day: number): number {
  const y = month <= 2 ? year - 1 : year;
  const era = Math.floor(y / 400);
  const yearOfEra = y - era * 400;
  const dayOfYear = Math.floor((153 * ((month + 9) % 12) + 2) / 5) + day - 1;
  const dayOfEra = yearOfEra * 365 + Math.floor(yearOfEra / 4) - Math.floor(yearOfEra / 100) + dayOfYear;
  return era * 146097 + dayOfEra - 719468;
}

/** `2024-01-01 11:00:00.5+02:00` (a space or `T`; `Z`, `±hh`, `±hhmm` or `±hh:mm`; seconds and fraction optional). */
const TIMESTAMPTZ = /^([+-]?\d{4,6})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,12}))?)?\s?(Z|[+-]\d{2}(?::?\d{2})?)$/;

/** A timestamp with a time zone as seconds since the epoch (UTC) and its fraction digits; null when it does not read. */
export function parseTimestamptz(text: string): { instant: number; fraction: string } | null {
  const match = TIMESTAMPTZ.exec(text.trim());
  if (!match) return null;
  const [, year, month, day, hour, minute, second = "0", fraction = "", zone] = match;
  const [m, d, h, mi, s] = [month, day, hour, minute, second].map(Number);
  if (m < 1 || m > 12 || d < 1 || d > 31 || h > 24 || mi > 59 || s > 60) return null;
  let offset = 0;
  if (zone !== "Z") {
    const digits = zone.slice(1).replace(":", "");
    const offsetHours = Number(digits.slice(0, 2));
    const offsetMinutes = digits.length > 2 ? Number(digits.slice(2)) : 0;
    if (offsetHours > 23 || offsetMinutes > 59) return null;
    offset = (zone.startsWith("-") ? -1 : 1) * (offsetHours * 3600 + offsetMinutes * 60);
  }
  const instant = daysFromCivil(Number(year), m, d) * 86_400 + h * 3600 + mi * 60 + s - offset;
  return { instant, fraction: fraction.replace(/0+$/, "") };
}

/** ISO text of a date or timestamp: its year (of any length or sign) as a number, the rest as text. */
function isoKey(text: string): SortKey {
  const match = /^([+-]?\d+)(-.*)$/.exec(text);
  return match ? { family: 3, year: parseDecimal(match[1]), text: match[2] } : { family: 3, year: null, text };
}

/** What a sort compares for a cell: read once per cell, so a sort does not parse in every comparison. */
export function sortKey(value: Value | undefined): SortKey {
  if (value === undefined || value.t === "null") return { family: -1 };
  switch (value.t) {
    case "bool":
      return { family: 0, value: Number(value.v) };
    case "int":
    case "float":
    case "decimal":
      return { family: 1, number: numberKey(value.v) };
    case "timestamptz": {
      const parsed = parseTimestamptz(value.v);
      return parsed ? { family: 2, ...parsed } : isoKey(value.v);
    }
    case "date":
    case "time":
    case "timestamp":
      return isoKey(value.v);
    default: {
      const text = displayValue(value) ?? "";
      const number = parseDecimal(text);
      return number ? { family: 4, number } : { family: 5, text };
    }
  }
}

/** Ascending order of two non-NULL keys. */
function compareNonNull(a: Exclude<SortKey, { family: -1 }>, b: Exclude<SortKey, { family: -1 }>): number {
  if (a.family !== b.family) return a.family - b.family;
  switch (a.family) {
    case 0:
      return a.value - (b as typeof a).value;
    case 1:
    case 4:
      return compareNumbers(a.number, (b as typeof a).number);
    case 2: {
      const other = b as typeof a;
      return a.instant - other.instant || compareText(a.fraction, other.fraction);
    }
    case 3: {
      const other = b as typeof a;
      if (a.year && other.year) return compareNumbers(a.year, other.year) || compareText(a.text, other.text);
      if (a.year || other.year) return a.year ? -1 : 1;
      return compareText(a.text, other.text);
    }
    case 5:
      return collator.compare(a.text, (b as typeof a).text);
  }
}

/** Compares two keys for a sort in `descending` order or not; NULL always goes last. */
export function compareSortKeys(a: SortKey, b: SortKey, descending = false): number {
  if (a.family === -1 || b.family === -1) return a.family === b.family ? 0 : a.family === -1 ? 1 : -1;
  const order = Math.sign(compareNonNull(a, b));
  return descending ? -order : order;
}

/** Compares two cells for a sort in `descending` order or not; NULL (and a missing cell) always goes last. */
export function compareValues(a: Value | undefined, b: Value | undefined, descending = false): number {
  return compareSortKeys(sortKey(a), sortKey(b), descending);
}

/** Row indexes ordered by the cell at `column` (stable: equal cells keep their order); each cell is read once. */
export function sortIndexes(indexes: number[], cell: (index: number) => Value | undefined, descending: boolean): number[] {
  return indexes
    .map((index, position) => ({ index, position, key: sortKey(cell(index)) }))
    .sort((x, y) => compareSortKeys(x.key, y.key, descending) || x.position - y.position)
    .map((item) => item.index);
}
