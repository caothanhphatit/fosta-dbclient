/** The grid's data model: setup, row and position types, key moves, and the pure helpers around rows and column kinds. */
import type { PendingRow } from "./changeSet";
import type { MenuItem } from "../../core/contextMenu";
import type { ColumnInfo, FilterOp, Value } from "../../core/contract";
import type { SqlDialect } from "../../core/sql";
import type { ExportFormat } from "../importExport/exporters";
import { displayValue, valuesEqual } from "../../core/values";
import type { ColumnFilters } from "./columnFilter";

export interface SortState {
  column: string;
  descending: boolean;
}

export interface GridSetup {
  columns: ColumnInfo[];
  rows: Value[][];
  /** `Page.rowVersions`: one per row, sent back with updates and deletes. */
  rowVersions: Value[] | null;
  editable: boolean;
  /** Why a non-editable grid is read-only, shown in the cell editor (e.g. a Read-only workspace). */
  readOnlyReason: string | null;
  /** Columns that identify a row (`Page.keyColumns`); empty: every comparable column. */
  keyColumns: string[];
  rowOffset: number;
  sort: SortState | null;
  onSort: ((column: string) => void) | null;
  tableName: string | null;
  /** How "Copy as INSERT statements" quotes names and values. */
  dialect: SqlDialect;
  onSelect: ((row: Value[] | null) => void) | null;
  extraMenu: ((row: Value[]) => MenuItem[]) | null;
  columnFilters: ColumnFilters;
  onColumnFilter: ((filters: ColumnFilters) => void) | null;
  /** Set while the column filters do not apply (the raw filter box is in use); the boxes are shown disabled with it. */
  filtersPaused: string | null;
  clientFilter: boolean;
  /** The header sorts the loaded rows in the browser (the query result grid) instead of calling `onSort`. */
  clientSort?: boolean;
  /** Column names not drawn; their data, filters and edits are kept. */
  hiddenColumns: string[];
  /** Called when the user hides or shows columns (the table view saves the choice). */
  onHiddenColumns: ((hidden: string[]) => void) | null;
  /** Loads all components of a vector cell for the cell editor (table views only). */
  loadVector: ((row: Value[], column: ColumnInfo) => Promise<number[]>) | null;
}

export interface GridRow extends PendingRow {
  original: Value[] | null;
  current: Value[];
  deleted: boolean;
  version: Value | null;
  touched: Set<number>;
  /** Set once a cell was edited; untouched rows skip the comparison. */
  edited: boolean;
  /** Gone from the server (resolved in the conflict dialog): not drawn and not saved. */
  removed: boolean;
  element: HTMLTableRowElement | null;
}

export interface Position {
  row: number;
  column: number;
}

export type RowState = "clean" | "modified" | "new" | "deleted";

export const NULL: Value = { t: "null", v: null };
const PREVIEW_LIMIT = 300;

export const MOVES: Record<string, Position> = {
  ArrowUp: { row: -1, column: 0 },
  ArrowDown: { row: 1, column: 0 },
  ArrowLeft: { row: 0, column: -1 },
  ArrowRight: { row: 0, column: 1 },
};

/** The short mark of each operator on the filter boxes' operator button. */
export const OPERATOR_MARKS: Record<FilterOp, string> = {
  contains: "≈",
  notContains: "!≈",
  startsWith: "^",
  endsWith: "$",
  eq: "=",
  ne: "≠",
  lt: "<",
  le: "≤",
  gt: ">",
  ge: "≥",
  between: "..",
  in: "in",
  notIn: "!in",
  like: "~",
  notLike: "!~",
  isNull: "∅",
  isNotNull: "!∅",
};

export function gridSetup(columns: ColumnInfo[], rows: Value[][], dialect: SqlDialect = "ansi"): GridSetup {
  return {
    columns,
    rows,
    rowVersions: null,
    editable: false,
    readOnlyReason: null,
    keyColumns: [],
    rowOffset: 0,
    sort: null,
    onSort: null,
    tableName: null,
    dialect,
    onSelect: null,
    extraMenu: null,
    columnFilters: {},
    onColumnFilter: null,
    filtersPaused: null,
    clientFilter: false,
    clientSort: false,
    hiddenColumns: [],
    onHiddenColumns: null,
    loadVector: null,
  };
}

export const EXPORT_FORMATS: ExportFormat[] = ["csv", "json", "markdown", "insert"];

export type TypeKind = "number" | "text" | "date" | "bool" | "json" | "binary" | "vector";

export const TYPE_LABELS: Record<TypeKind, string> = { number: "123", text: "abc", date: "date", bool: "bool", json: "json", binary: "bin", vector: "vec" };

/** Coarse family of a column's kind, for the badge under its name. */
export function typeKind(column: ColumnInfo): TypeKind {
  if (column.hints.vectorDims != null) return "vector";
  switch (column.kind.kind) {
    case "int":
    case "float":
    case "decimal":
      return "number";
    case "bool":
      return "bool";
    case "date":
    case "time":
    case "timestamp":
    case "timestampTz":
    case "interval":
      return "date";
    case "json":
    case "document":
    case "array":
      return "json";
    case "bytes":
    case "objectId":
      return "binary";
    default:
      return column.hints.binary ? "binary" : "text";
  }
}

export const TEXT_KINDS = new Set(["text", "other", "interval"]);

export function rowState(row: GridRow): RowState {
  if (row.removed) return "clean";
  const original = row.original;
  if (!original) return "new";
  if (row.deleted) return "deleted";
  if (!row.edited) return "clean";
  return row.current.some((value, index) => !valuesEqual(value, original[index] ?? NULL)) ? "modified" : "clean";
}

/** A row as tab-separated text, NULL spelled out (what Paste reads back). */
export function rowText(row: Value[]): string {
  return row.map((value) => displayValue(value) ?? "NULL").join("\t");
}

export function preview(value: string): string {
  return value.length > PREVIEW_LIMIT ? `${value.slice(0, PREVIEW_LIMIT)}…` : value;
}
