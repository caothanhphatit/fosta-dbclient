/** Plain text columns and rows for grids that show text the UI builds itself (the design view, filter placeholders). */
import type { ColumnInfo, Value } from "../../core/contract";
import type { Cell } from "../../core/types";

export function textColumn(name: string): ColumnInfo {
  return { name, typeName: "", kind: { kind: "text" }, nullable: true, default: null, comment: null, identity: null, generated: null, hints: {} };
}

export function textRows(rows: Cell[][]): Value[][] {
  return rows.map((row) => row.map((cell): Value => (cell === null ? { t: "null", v: null } : { t: "text", v: cell })));
}
