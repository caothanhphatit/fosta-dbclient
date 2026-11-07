import { quoteIdent, sqlLiteral } from "./sql";
import type { Cell, ColumnMeta } from "./types";

export type ExportFormat = "csv" | "json" | "markdown" | "insert";

export const EXPORT_LABELS: Record<ExportFormat, string> = {
  csv: "CSV",
  json: "JSON",
  markdown: "Markdown table",
  insert: "INSERT statements",
};

export function exportRows(format: ExportFormat, columns: ColumnMeta[], rows: Cell[][], tableName: string | null): string {
  const exportable = columns.map((column, index) => ({ column, index })).filter(({ column }) => column.vectorDims === null);
  const pick = (row: Cell[]) => exportable.map(({ index }) => row[index]);
  const names = exportable.map(({ column }) => column.name);
  switch (format) {
    case "csv":
      return [names, ...rows.map(pick)].map((values) => values.map(csvValue).join(",")).join("\r\n");
    case "json":
      return JSON.stringify(rows.map((row) => Object.fromEntries(pick(row).map((value, index) => [names[index], value]))), null, 2);
    case "markdown":
      return markdownTable(names, rows.map(pick));
    case "insert":
      return insertStatements(tableName ?? "table_name", exportable.map(({ column }) => column), rows.map(pick));
  }
}

function csvValue(value: Cell): string {
  if (value === null) return "";
  return /[",\r\n]/.test(value) ? `"${value.replace(/"/g, '""')}"` : value;
}

function markdownTable(names: string[], rows: Cell[][]): string {
  const escape = (value: Cell) => (value ?? "NULL").replace(/\|/g, "\\|").replace(/\r?\n/g, " ");
  const header = `| ${names.map(escape).join(" | ")} |`;
  const divider = `| ${names.map(() => "---").join(" | ")} |`;
  return [header, divider, ...rows.map((row) => `| ${row.map(escape).join(" | ")} |`)].join("\n");
}

function insertStatements(table: string, columns: ColumnMeta[], rows: Cell[][]): string {
  const columnList = columns.map((column) => quoteIdent(column.name)).join(", ");
  return rows
    .map((row) => `INSERT INTO ${quoteIdent(table)} (${columnList}) VALUES (${row.map((value, index) => sqlLiteral(value, columns[index])).join(", ")});`)
    .join("\n");
}
