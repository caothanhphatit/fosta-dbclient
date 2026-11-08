/** Pasting a text block over grid rows: each field is parsed for its column's kind and written to the pending row. */
import { isEditableColumn } from "./changeSet";
import { pastedValue, splitPasted } from "./cellValue";
import type { GridRow, Position } from "./gridModel";
import type { ColumnInfo } from "../../core/contract";

/** The grid state a paste reads and writes. */
export interface PasteTarget {
  columns: ColumnInfo[];
  rows: GridRow[];
  /** Row indexes in display order, and the display slot of each row index. */
  visible: number[];
  visiblePosition: Map<number, number>;
  /** Indexes of the visible columns, in order. */
  displayed: number[];
}

export interface PasteResult {
  pasted: number;
  errors: string[];
  /** Rows that were written to, in order, for the caller to redraw. */
  changedRows: number[];
}

/** Writes `text` (one value, or a tab/newline block) starting at `position`; fields that do not fit are skipped. */
export function pasteBlock(target: PasteTarget, position: Position, text: string): PasteResult {
  const block = splitPasted(text);
  const errors: string[] = [];
  const changedRows: number[] = [];
  let pasted = 0;
  const startRow = target.visiblePosition.get(position.row) ?? 0;
  const startColumn = target.displayed.indexOf(position.column);
  block.forEach((fields, rowOffset) => {
    const rowIndex = target.visible[startRow + rowOffset];
    const row = rowIndex === undefined ? undefined : target.rows[rowIndex];
    if (!row || row.deleted) return;
    fields.forEach((field, columnOffset) => {
      const columnIndex = target.displayed[startColumn + columnOffset];
      const column = columnIndex === undefined ? undefined : target.columns[columnIndex];
      if (!column || !isEditableColumn(column)) return;
      const value = pastedValue(column.kind, field);
      if (value instanceof Error) {
        errors.push(`${column.name}: ${value.message}`);
        return;
      }
      row.current[columnIndex] = value;
      row.touched.add(columnIndex);
      row.edited = true;
      pasted += 1;
    });
    changedRows.push(rowIndex);
  });
  return { pasted, errors, changedRows };
}
