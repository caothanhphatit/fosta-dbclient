/** Imported text fields as typed values of their columns (pure, so the rules are unit-tested). */
import type { ColumnInfo, Field, RowChange, Value } from "../../core/contract";
import type { Cell } from "../../core/types";
import { parseInput } from "../../core/values";

const MAX_LISTED_ERRORS = 5;

/**
 * The typed value of one imported field, or an `Error`. Missing and (optionally) empty fields are NULL; an empty field of a
 * non-text column is an error otherwise, since only text can be empty.
 */
export function importValue(column: ColumnInfo, cell: Cell, emptyAsNull: boolean): Value | Error {
  if (cell === null || (emptyAsNull && cell === "")) return { t: "null", v: null };
  return parseInput(column.kind, cell);
}

/** Insert changes for the mapped file columns, and the first errors (row and column) when values do not fit. */
export function importChanges(
  rows: Cell[][],
  mapping: { index: number; column: ColumnInfo }[],
  emptyAsNull: boolean,
): { changes: RowChange[]; errors: string[]; errorCount: number } {
  const changes: RowChange[] = [];
  const errors: string[] = [];
  let errorCount = 0;
  rows.forEach((row, rowIndex) => {
    const values: Field[] = [];
    for (const { index, column } of mapping) {
      const value = importValue(column, row[index] ?? null, emptyAsNull);
      if (value instanceof Error) {
        errorCount += 1;
        if (errors.length < MAX_LISTED_ERRORS) errors.push(`Row ${rowIndex + 1}, ${column.name}: ${value.message}`);
        continue;
      }
      // An empty identity or generated column is left to the database (SQL Server refuses an explicit NULL there).
      if (value.t === "null" && (column.identity || column.generated)) continue;
      values.push({ column: column.name, value });
    }
    changes.push({ op: "insert", values });
  });
  return { changes, errors, errorCount };
}
