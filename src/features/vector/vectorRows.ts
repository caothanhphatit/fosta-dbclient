/**
 * The `vector_*` commands still answer with text result sets (`ColumnMeta` + text cells) and take text key cells. These
 * helpers turn their results into typed grid data and typed rows into their key cells, until the vector views move to
 * `invoke_capability` (the MariaDB engine module). Display is unchanged: numbers stay right-aligned (decimal text),
 * binary values stay `0x…` text.
 */
import type { ColumnInfo, Value, ValueKind } from "../../core/contract";
import type { Cell, CellValue, ColumnMeta } from "../../core/types";
import { displayValue } from "../../core/values";

function kindOf(column: ColumnMeta): ValueKind {
  if (column.vectorDims != null) return { kind: "other" };
  if (column.binary) return { kind: "bytes" };
  if (column.numeric) return { kind: "decimal" };
  return { kind: "text" };
}

function textResultColumn(column: ColumnMeta): ColumnInfo {
  return {
    name: column.name,
    typeName: column.typeName,
    kind: kindOf(column),
    nullable: true,
    default: null,
    comment: null,
    identity: null,
    generated: null,
    hints: {
      ...(column.primary ? { primary: true } : {}),
      ...(column.binary ? { binary: true } : {}),
      ...(column.vectorDims != null ? { vectorDims: column.vectorDims } : {}),
    },
  };
}

/** A text cell as a value of the column's kind (decimal text for numeric columns, so the grid aligns it). */
function textResultValue(cell: Cell, column: ColumnInfo): Value {
  if (cell === null) return { t: "null", v: null };
  switch (column.kind.kind) {
    case "decimal":
      return { t: "decimal", v: cell };
    case "bytes":
      return { t: "bytes", v: cell };
    case "other":
      return { t: "other", type: column.typeName, v: cell };
    default:
      return { t: "text", v: cell };
  }
}

export function textResultData(columns: ColumnMeta[], rows: Cell[][]): { columns: ColumnInfo[]; rows: Value[][] } {
  const infos = columns.map(textResultColumn);
  return { columns: infos, rows: rows.map((row) => row.map((cell, index) => textResultValue(cell, infos[index] ?? infos[0]))) };
}

/**
 * The key cells the `vector_*` commands take (`KeyCell { column, value: string | null }`): the key columns of a typed
 * row as text, or every comparable column when the table has no key.
 */
export function keyCells(columns: ColumnInfo[], row: Value[], keyColumns: string[]): CellValue[] {
  const keys = new Set(keyColumns);
  return columns.flatMap((column, index) => {
    const isKey = keys.size === 0 ? column.hints.vectorDims == null : keys.has(column.name);
    return isKey ? [{ column: column.name, value: displayValue(row[index] ?? { t: "null", v: null }) }] : [];
  });
}
