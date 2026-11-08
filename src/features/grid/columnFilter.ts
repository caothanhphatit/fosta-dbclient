/**
 * The filter boxes under a grid's column headers. Each box holds a short text (`>= 10`, `^Oak`, `in:a,b`, `null` …) that
 * parses to one structured condition (`FilterCond`: column, operator, values as typed text). Table views send the
 * conditions to the engine (`Filter.conditions`), which binds the values for the column's type, so nothing here quotes
 * SQL; result grids of the query tab apply the same conditions client-side (`matchesFilter`).
 */
import type { ColumnInfo, Filter, FilterCond, FilterOp, Value, ValueKind } from "../../core/contract";
import { displayValue, isNumericKind, parseInput } from "../../core/values";

/** Column name → the text in its filter box. */
export type ColumnFilters = Record<string, string>;

export interface FilterOperator {
  op: FilterOp;
  label: string;
  /** How the operator is written in a filter box ("x" stands for the value). */
  syntax: string;
}

/** Every operator the filter boxes understand, in menu order. */
export const FILTER_OPERATORS: FilterOperator[] = [
  { op: "contains", label: "Contains", syntax: "x" },
  { op: "notContains", label: "Does not contain", syntax: "!x" },
  { op: "startsWith", label: "Starts with", syntax: "^x" },
  { op: "endsWith", label: "Ends with", syntax: "x$" },
  { op: "eq", label: "Equals", syntax: "=x" },
  { op: "ne", label: "Not equal", syntax: "!=x" },
  { op: "lt", label: "Less than", syntax: "<x" },
  { op: "le", label: "Less or equal", syntax: "<=x" },
  { op: "gt", label: "Greater than", syntax: ">x" },
  { op: "ge", label: "Greater or equal", syntax: ">=x" },
  { op: "between", label: "Between", syntax: "a..b" },
  { op: "in", label: "In list", syntax: "in:a,b,c" },
  { op: "notIn", label: "Not in list", syntax: "!in:a,b,c" },
  { op: "like", label: "Like (% and _)", syntax: "~a%b" },
  { op: "notLike", label: "Not like", syntax: "!~a%b" },
  { op: "isNull", label: "Is NULL", syntax: "null" },
  { op: "isNotNull", label: "Is not NULL", syntax: "!null" },
];

export const FILTER_HELP =
  "abc contains (equals on number, date and id columns) · !abc · ^starts · ends$ · =x != < <= > >= · a..b · in:a,b · !in:a,b · ~like% · !~like% · null · !null · applies as you type, Esc clears; the funnel picks an operator";

/** Kinds where plain text means "contains"; elsewhere it means "equals". */
const TEXT_KINDS = new Set(["text", "other", "json", "document", "array", "bytes", "interval"]);

/** Kinds whose operand is checked before the filter is sent (the engine binds it as that type). */
const TYPED_KINDS = new Set(["int", "float", "decimal", "bool", "date", "time", "timestamp", "timestampTz", "uuid", "objectId"]);

/** Operators whose values are compared as the column's type (pattern operators always take text). */
const VALUE_OPS = new Set<FilterOp>(["eq", "ne", "lt", "le", "gt", "ge", "between", "in", "notIn"]);

const COMPARISON = /^(>=|<=|<>|!=|=|>|<)\s*([\s\S]*)$/;
const COMPARISON_OPS: Record<string, FilterOp> = { ">=": "ge", "<=": "le", "<>": "ne", "!=": "ne", "=": "eq", ">": "gt", "<": "lt" };

function list(text: string): string[] {
  return text
    .split(",")
    .map((item) => item.trim())
    .filter((item) => item !== "");
}

/** The operator and values a filter text stands for, before the column's type is considered; null for an empty box. */
export function parseFilterText(text: string, kind: ValueKind = { kind: "text" }): { op: FilterOp; values: string[] } | null {
  const t = text.trim();
  if (!t) return null;
  const lower = t.toLowerCase();
  if (lower === "null") return { op: "isNull", values: [] };
  if (lower === "!null" || lower === "not null") return { op: "isNotNull", values: [] };
  if (lower.startsWith("!in:")) return { op: "notIn", values: list(t.slice(4)) };
  if (lower.startsWith("in:")) return { op: "in", values: list(t.slice(3)) };
  if (t.startsWith("!~")) return { op: "notLike", values: [t.slice(2).trim()] };
  if (t.startsWith("~")) return { op: "like", values: [t.slice(1).trim()] };
  const comparison = COMPARISON.exec(t);
  if (comparison) return { op: COMPARISON_OPS[comparison[1]], values: [comparison[2].trim()] };
  if (t.startsWith("!")) return { op: "notContains", values: [t.slice(1).trim()] };
  if (t.startsWith("^") && t.length > 1) return { op: "startsWith", values: [t.slice(1)] };
  if (t.endsWith("$") && t.length > 1) return { op: "endsWith", values: [t.slice(0, -1)] };
  const range = /^(.+?)\.\.(.+)$/.exec(t);
  if (range) return { op: "between", values: [range[1].trim(), range[2].trim()] };
  return { op: TEXT_KINDS.has(kind.kind) ? "contains" : "eq", values: [t] };
}

/** How a condition is written in a filter box (the inverse of `parseFilterText`). */
export function formatFilter(op: FilterOp, values: string[]): string {
  const [first = "", second = ""] = values;
  switch (op) {
    case "contains":
      return first;
    case "notContains":
      return `!${first}`;
    case "startsWith":
      return `^${first}`;
    case "endsWith":
      return `${first}$`;
    case "eq":
      return `=${first}`;
    case "ne":
      return `!=${first}`;
    case "lt":
      return `<${first}`;
    case "le":
      return `<=${first}`;
    case "gt":
      return `>${first}`;
    case "ge":
      return `>=${first}`;
    case "between":
      return `${first}..${second}`;
    case "in":
      return `in:${values.join(",")}`;
    case "notIn":
      return `!in:${values.join(",")}`;
    case "like":
      return `~${first}`;
    case "notLike":
      return `!~${first}`;
    case "isNull":
      return "null";
    case "isNotNull":
      return "!null";
  }
}

/** The filter text with its operator replaced by `op`, keeping the value(s) typed so far (the funnel menu). */
export function withOperator(text: string, op: FilterOp): string {
  const parsed = parseFilterText(text);
  const values = parsed && parsed.op !== "isNull" && parsed.op !== "isNotNull" ? parsed.values : [];
  if (op === "between" && values.length < 2) return formatFilter(op, [values[0] ?? "", ""]);
  return formatFilter(op, op === "in" || op === "notIn" || op === "between" ? values : [values.join(",")]);
}

/**
 * The condition of one filter box: null when it is empty, an `Error` when a value does not fit the column's type
 * (`> abc` on a number column). Values stay text; the engine binds them for the column.
 */
export function columnCondition(column: ColumnInfo, text: string): FilterCond | Error | null {
  const parsed = parseFilterText(text, column.kind);
  if (!parsed) return null;
  if (VALUE_OPS.has(parsed.op) && TYPED_KINDS.has(column.kind.kind)) {
    if (parsed.values.length === 0) return new Error(`${column.name}: give at least one value`);
    for (const value of parsed.values) {
      const checked = parseInput(column.kind, value);
      if (checked instanceof Error) return new Error(`${column.name}: ${checked.message}`);
    }
  }
  if (parsed.op === "between" && parsed.values.length !== 2) return new Error(`${column.name}: write a range as low..high`);
  return { column: column.name, op: parsed.op, values: parsed.values };
}

/** The valid conditions of every filled-in box, and the errors of the invalid ones (which are not sent). */
export function filterConditions(columns: ColumnInfo[], filters: ColumnFilters): { conditions: FilterCond[]; errors: Map<string, string> } {
  const conditions: FilterCond[] = [];
  const errors = new Map<string, string>();
  for (const column of columns) {
    const text = filters[column.name];
    if (text === undefined) continue;
    const condition = columnCondition(column, text);
    if (condition instanceof Error) errors.set(column.name, condition.message);
    else if (condition) conditions.push(condition);
  }
  return { conditions, errors };
}

/** The `Filter` of the column boxes (all conditions must hold). */
export function conditionsFilter(conditions: FilterCond[]): Filter {
  return conditions.length === 0 ? { kind: "none" } : { kind: "conditions", conditions, matchAny: false };
}

// --- Client-side matching (result grids of the query tab) -------------------------------------------------------------

function order(value: Value, operand: string, kind: ValueKind): number {
  const shown = displayValue(value) ?? "";
  if (isNumericKind(kind) || value.t === "int" || value.t === "float" || value.t === "decimal") {
    const [a, b] = [Number(shown), Number(operand)];
    if (!Number.isNaN(a) && !Number.isNaN(b) && operand.trim() !== "") return a - b;
  }
  if (value.t === "bool") return Number(value.v) - Number(["true", "1", "yes", "t", "y", "on"].includes(operand.trim().toLowerCase()));
  return shown.localeCompare(operand, undefined, { sensitivity: "accent" });
}

function likeRegex(pattern: string): RegExp {
  const escaped = pattern
    .replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    .replace(/%/g, ".*")
    .replace(/_/g, ".");
  return new RegExp(`^${escaped}$`, "is");
}

/** Whether a cell meets a condition, the way the engines evaluate it (NULL meets only "is NULL" and "not …" never). */
export function matchesCondition(value: Value, condition: FilterCond, kind: ValueKind): boolean {
  const isNull = value.t === "null";
  if (condition.op === "isNull") return isNull;
  if (condition.op === "isNotNull") return !isNull;
  if (isNull) return false;
  const [first = "", second = ""] = condition.values;
  const text = (displayValue(value) ?? "").toLowerCase();
  switch (condition.op) {
    case "contains":
      return text.includes(first.toLowerCase());
    case "notContains":
      return !text.includes(first.toLowerCase());
    case "startsWith":
      return text.startsWith(first.toLowerCase());
    case "endsWith":
      return text.endsWith(first.toLowerCase());
    case "like":
      return likeRegex(first).test(displayValue(value) ?? "");
    case "notLike":
      return !likeRegex(first).test(displayValue(value) ?? "");
    case "eq":
      return order(value, first, kind) === 0;
    case "ne":
      return order(value, first, kind) !== 0;
    case "lt":
      return order(value, first, kind) < 0;
    case "le":
      return order(value, first, kind) <= 0;
    case "gt":
      return order(value, first, kind) > 0;
    case "ge":
      return order(value, first, kind) >= 0;
    case "between":
      return order(value, first, kind) >= 0 && order(value, second, kind) <= 0;
    case "in":
      return condition.values.some((operand) => order(value, operand, kind) === 0);
    case "notIn":
      return condition.values.every((operand) => order(value, operand, kind) !== 0);
  }
}

/** Client-side check of one filter box against a cell; an invalid filter matches nothing. */
export function matchesFilter(value: Value, column: ColumnInfo, text: string): boolean {
  const condition = columnCondition(column, text);
  if (condition === null) return true;
  if (condition instanceof Error) return false;
  return matchesCondition(value, condition, column.kind);
}
