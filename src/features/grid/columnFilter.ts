import { quoteIdent, sqlLiteral } from "../../core/sql";
import type { Cell, ColumnMeta } from "../../core/types";

export type ColumnFilters = Record<string, string>;

export const FILTER_HELP = "abc = contains · =x  !=x  >x  <x  >=x  <=x · null · !null · applies as you type, Esc clears";

const COMPARISON = /^(>=|<=|<>|!=|=|>|<)\s*([\s\S]*)$/;
const NUMBER = /^-?\d+(\.\d+)?(e[+-]?\d+)?$/i;
const HEX = /^0x[0-9a-f]+$/i;

type Comparison = { operator: string; operand: string };

function parseComparison(text: string): Comparison | null {
  const match = COMPARISON.exec(text);
  if (!match) return null;
  return { operator: match[1] === "!=" ? "<>" : match[1], operand: match[2] };
}

function isNullKeyword(text: string): boolean {
  return text.toLowerCase() === "null";
}

function isNotNullKeyword(text: string): boolean {
  const lower = text.toLowerCase();
  return lower === "!null" || lower === "not null";
}

export function columnCondition(column: ColumnMeta, text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  const name = quoteIdent(column.name);
  if (isNullKeyword(trimmed)) return `${name} IS NULL`;
  if (isNotNullKeyword(trimmed)) return `${name} IS NOT NULL`;
  const comparison = parseComparison(trimmed);
  if (comparison) return `${name} ${comparison.operator} ${sqlLiteral(comparison.operand, column)}`;
  if (column.binary && HEX.test(trimmed)) return `${name} = ${trimmed}`;
  if (column.numeric && NUMBER.test(trimmed)) return `${name} = ${trimmed}`;
  return `${name} LIKE ${sqlLiteral(`%${trimmed}%`)}`;
}

export function combinedCondition(columns: ColumnMeta[], filters: ColumnFilters, extra: string): string | null {
  const conditions = [
    extra.trim(),
    ...columns.map((column) => columnCondition(column, filters[column.name] ?? "")),
  ].filter((condition): condition is string => Boolean(condition));
  if (conditions.length === 0) return null;
  return conditions.length === 1 ? conditions[0] : conditions.map((condition) => `(${condition})`).join(" AND ");
}

/** MongoDB flavour: each column filter becomes a query clause, combined with `$and`. */
export function combinedJsonFilter(columns: ColumnMeta[], filters: ColumnFilters, extra: string): string | null {
  const clauses: unknown[] = [];
  const base = extra.trim();
  if (base) {
    try {
      clauses.push(JSON.parse(base));
    } catch {
      throw new Error('The filter must be a JSON query document, e.g. { "status": "ACTIVE" }');
    }
  }
  for (const column of columns) {
    const clause = jsonCondition(column, filters[column.name] ?? "");
    if (clause) clauses.push(clause);
  }
  if (clauses.length === 0) return null;
  return JSON.stringify(clauses.length === 1 ? clauses[0] : { $and: clauses });
}

const JSON_OPERATORS: Record<string, string> = { "=": "$eq", "<>": "$ne", ">": "$gt", "<": "$lt", ">=": "$gte", "<=": "$lte" };

function jsonCondition(column: ColumnMeta, text: string): Record<string, unknown> | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  if (isNullKeyword(trimmed)) return { [column.name]: null };
  if (isNotNullKeyword(trimmed)) return { [column.name]: { $ne: null } };
  const comparison = parseComparison(trimmed);
  if (comparison) return { [column.name]: { [JSON_OPERATORS[comparison.operator]]: jsonOperand(comparison.operand) } };
  if (NUMBER.test(trimmed)) return { [column.name]: Number(trimmed) };
  return { [column.name]: { $regex: escapeRegex(trimmed), $options: "i" } };
}

function jsonOperand(operand: string): unknown {
  const trimmed = operand.trim();
  if (NUMBER.test(trimmed)) return Number(trimmed);
  if (trimmed === "true" || trimmed === "false") return trimmed === "true";
  return trimmed.replace(/^"(.*)"$/, "$1");
}

function escapeRegex(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function matchesFilter(value: Cell, text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed) return true;
  if (isNullKeyword(trimmed)) return value === null;
  if (isNotNullKeyword(trimmed)) return value !== null;
  if (value === null) return false;
  const comparison = parseComparison(trimmed);
  if (!comparison) return value.toLowerCase().includes(trimmed.toLowerCase());
  return compare(value, comparison);
}

function compare(value: string, { operator, operand }: Comparison): boolean {
  const numeric = NUMBER.test(value) && NUMBER.test(operand);
  const order = numeric ? Number(value) - Number(operand) : value.localeCompare(operand);
  switch (operator) {
    case "=":
      return order === 0;
    case "<>":
      return order !== 0;
    case ">":
      return order > 0;
    case "<":
      return order < 0;
    case ">=":
      return order >= 0;
    default:
      return order <= 0;
  }
}
