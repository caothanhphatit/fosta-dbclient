/**
 * Pending grid edits → contract `RowChange`s (optimistic concurrency). Pure, so the rules are unit-tested:
 *
 * - insert: `values` = the cells the user filled in (or set to NULL explicitly).
 * - update: `key`, `values` (the edited columns), `original` (those columns as loaded) and `version` (the page's row
 *   version for that row, when the engine reports one). Several edits of one row are one update.
 * - delete: `key`, `original` (every comparable column of the row as loaded) and `version`.
 *
 * The engine applies an update or delete only when the row still matches `original` / `version`; otherwise the whole set
 * fails with `CONFLICT` and nothing is written. No transaction is held while the user edits.
 */
import type { ColumnInfo, Field, RowChange, Value } from "../../core/contract";
import { valuesEqual } from "../../core/values";

/** One grid row: what was loaded (`null` for a new row), what it is now, and whether it is marked for deletion. */
export interface PendingRow {
  original: Value[] | null;
  current: Value[];
  deleted: boolean;
  /** `Page.rowVersions[i]` of the row, sent back as `RowChange.version`. */
  version?: Value | null;
  /** Columns the user touched in a new row (an explicit NULL is sent; untouched columns get their default). */
  touched?: ReadonlySet<number>;
}

/** A change and the index of the grid row it came from (to map `RowConflict.index` back to a row). */
export interface BuiltChange {
  row: number;
  change: RowChange;
}

const NULL: Value = { t: "null", v: null };
const NOT_COMPARABLE = new Set(["bytes", "json", "document", "array", "float", "other"]);

/**
 * Whether a column's loaded value is a reliable match for "the row is unchanged": exact types only. Bytes, JSON,
 * documents, arrays, floats and engine-specific values do not round-trip exactly (or cannot be compared in a WHERE), so
 * a delete's `original` leaves them out; so do binary and read-only columns.
 */
export function isComparable(column: ColumnInfo): boolean {
  if (NOT_COMPARABLE.has(column.kind.kind)) return false;
  return !column.hints.binary && !column.hints.readOnly && column.hints.vectorDims == null;
}

/** Whether the user may change the column in the grid (vectors and read-only columns may not). */
export function isEditableColumn(column: ColumnInfo): boolean {
  return column.hints.vectorDims == null && !column.hints.readOnly;
}

/** The fields that identify a row: the key columns, or every comparable column when the object has no key. */
export function keyFields(columns: ColumnInfo[], keyColumns: string[], values: Value[]): Field[] {
  const keys = new Set(keyColumns);
  return columns.flatMap((column, index) => {
    const isKey = keys.size === 0 ? isComparable(column) : keys.has(column.name);
    return isKey ? [{ column: column.name, value: values[index] ?? NULL }] : [];
  });
}

function fields(columns: ColumnInfo[], values: Value[], include: (index: number) => boolean): Field[] {
  return columns.flatMap((column, index) => (include(index) ? [{ column: column.name, value: values[index] ?? NULL }] : []));
}

/** Indexes of the columns whose current value differs from the loaded one. */
export function changedColumns(row: PendingRow): number[] {
  const original = row.original;
  if (!original) return [];
  return row.current.flatMap((value, index) => (valuesEqual(value, original[index] ?? NULL) ? [] : [index]));
}

/** The change one row stands for, or null when it is unchanged. */
export function rowChange(columns: ColumnInfo[], keyColumns: string[], row: PendingRow): RowChange | null {
  const original = row.original;
  if (original === null) {
    if (row.deleted) return null;
    const touched = row.touched;
    const values = fields(
      columns,
      row.current,
      (index) => isEditableColumn(columns[index]) && (touched ? touched.has(index) : row.current[index]?.t !== "null"),
    );
    return { op: "insert", values };
  }
  const version = row.version ? { version: row.version } : {};
  const key = keyFields(columns, keyColumns, original);
  if (row.deleted) {
    return { op: "delete", key, original: fields(columns, original, (index) => isComparable(columns[index])), ...version };
  }
  const changed = new Set(changedColumns(row).filter((index) => isEditableColumn(columns[index])));
  if (changed.size === 0) return null;
  return {
    op: "update",
    key,
    values: fields(columns, row.current, (index) => changed.has(index)),
    original: fields(columns, original, (index) => changed.has(index)),
    ...version,
  };
}

/** Every pending change of the grid, in row order, with the row each one came from. */
export function buildChanges(columns: ColumnInfo[], keyColumns: string[], rows: PendingRow[]): BuiltChange[] {
  return rows.flatMap((row, index) => {
    const change = rowChange(columns, keyColumns, row);
    return change ? [{ row: index, change }] : [];
  });
}

/** The same change, applied even though the row changed since it was read (the conflict dialog's "Overwrite"). */
export function forced(change: RowChange): RowChange {
  return change.op === "insert" ? change : { ...change, force: true };
}
