/**
 * Key cells for the `vector_*` commands: the key columns of a typed row with their typed values (`KeyCell`), as the
 * grid holds them, so the engine binds each one with its own type.
 */
import type { ColumnInfo, Value } from "../../core/contract";
import type { KeyCell } from "../../core/types";

/** The key cells of `row`: its key columns, or every non-vector column when the table has no key. */
export function keyCells(columns: ColumnInfo[], row: Value[], keyColumns: string[]): KeyCell[] {
  const keys = new Set(keyColumns);
  return columns.flatMap((column, index) => {
    const isKey = keys.size === 0 ? column.hints.vectorDims == null : keys.has(column.name);
    return isKey ? [{ column: column.name, value: row[index] ?? { t: "null", v: null } }] : [];
  });
}
