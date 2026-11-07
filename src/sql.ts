import type { Cell, CellValue, ColumnMeta } from "./types";

export function quoteIdent(name: string): string {
  return `\`${name.replace(/`/g, "``")}\``;
}

export function sqlLiteral(value: Cell, column?: ColumnMeta): string {
  if (value === null) return "NULL";
  if (column?.binary && /^0x[0-9a-f]*$/i.test(value)) return value;
  if (column?.numeric && /^-?\d+(\.\d+)?(e[+-]?\d+)?$/i.test(value)) return value;
  return `'${value.replace(/\\/g, "\\\\").replace(/'/g, "''")}'`;
}

export function equalsFilter(pairs: { column: string; value: Cell; meta?: ColumnMeta }[]): string {
  return pairs
    .map(({ column, value, meta }) => (value === null ? `${quoteIdent(column)} IS NULL` : `${quoteIdent(column)} = ${sqlLiteral(value, meta)}`))
    .join(" AND ");
}

export function rowKey(columns: ColumnMeta[], row: Cell[], primaryKey: string[]): CellValue[] {
  const keyColumns = new Set(primaryKey);
  const usesAllColumns = keyColumns.size === 0;
  return columns.flatMap((column, index) => {
    const isKey = usesAllColumns ? column.vectorDims === null : keyColumns.has(column.name);
    return isKey ? [{ column: column.name, value: row[index] }] : [];
  });
}

export function describeKey(key: CellValue[]): string {
  return equalsFilter(key.map((cell) => ({ column: cell.column, value: cell.value })));
}
