/**
 * Rows of a grid as text: CSV / delimited and Markdown (cells as the grid shows them, `displayValue`), JSON (relaxed JSON
 * values: numbers stay numbers, documents stay objects) and SQL INSERT statements (typed literals per dialect). Vector
 * columns are left out (the grid holds only a summary of them).
 */
import type { ColumnInfo, Value } from "../../core/contract";
import { quoteIdent, quoteTable, valueLiteral, type SqlDialect } from "../../core/sql";
import { displayValue, valueToJson } from "../../core/values";

export type ExportFormat = "csv" | "json" | "markdown" | "insert";

export const EXPORT_LABELS: Record<ExportFormat, string> = {
  csv: "CSV",
  json: "JSON",
  markdown: "Markdown table",
  insert: "INSERT statements",
};

type Exportable = { column: ColumnInfo; index: number }[];

function exportableColumns(columns: ColumnInfo[]): Exportable {
  return columns.map((column, index) => ({ column, index })).filter(({ column }) => column.hints.vectorDims == null);
}

const NULL: Value = { t: "null", v: null };

export function exportRows(format: ExportFormat, columns: ColumnInfo[], rows: Value[][], tableName: string | null, dialect: SqlDialect): string {
  const exportable = exportableColumns(columns);
  const pick = (row: Value[]) => exportable.map(({ index }) => row[index] ?? NULL);
  const names = exportable.map(({ column }) => column.name);
  switch (format) {
    case "csv":
      return delimitedText(names, rows.map(pick), ",");
    case "json":
      return JSON.stringify(
        rows.map((row) => Object.fromEntries(pick(row).map((value, index) => [names[index], valueToJson(value)]))),
        null,
        2,
      );
    case "markdown":
      return markdownTable(names, rows.map(pick));
    case "insert":
      return insertStatements(tableName ?? "table_name", names, rows.map(pick), dialect);
  }
}

/**
 * The list separator Excel expects when it opens a CSV: ";" where the decimal separator is a comma (Vietnamese,
 * most of Europe), "," elsewhere (US, UK). Windows ties its list separator to the locale the same way by default.
 */
export function excelSeparator(locale?: string): string {
  const decimal = new Intl.NumberFormat(locale).formatToParts(1.5).find((part) => part.type === "decimal")?.value;
  return decimal === "," ? ";" : ",";
}

/**
 * RFC 4180 text with any separator: a header line, then one record per row.
 * Values holding the separator, a quote or a line break are quoted, so a cell
 * may span lines. NULL becomes an empty field. Vector columns are left out.
 */
export function exportDelimited(columns: ColumnInfo[], rows: Value[][], separator: string): string {
  const exportable = exportableColumns(columns);
  return delimitedText(
    exportable.map(({ column }) => column.name),
    rows.map((row) => exportable.map(({ index }) => row[index] ?? NULL)),
    separator,
  );
}

function delimitedText(names: string[], rows: Value[][], separator: string): string {
  const field = (text: string | null): string => {
    if (text === null) return "";
    const needsQuotes = text.includes(separator) || /["\r\n]/.test(text);
    return needsQuotes ? `"${text.replace(/"/g, '""')}"` : text;
  };
  return [names.map(field), ...rows.map((row) => row.map((value) => field(displayValue(value))))].map((values) => values.join(separator)).join("\r\n");
}

function markdownTable(names: string[], rows: Value[][]): string {
  const escape = (text: string) => text.replace(/\|/g, "\\|").replace(/\r?\n/g, " ");
  const header = `| ${names.map(escape).join(" | ")} |`;
  const divider = `| ${names.map(() => "---").join(" | ")} |`;
  return [header, divider, ...rows.map((row) => `| ${row.map((value) => escape(displayValue(value) ?? "NULL")).join(" | ")} |`)].join("\n");
}

function insertStatements(table: string, names: string[], rows: Value[][], dialect: SqlDialect): string {
  const columnList = names.map((name) => quoteIdent(name, dialect)).join(", ");
  const target = quoteTable(table, dialect);
  return rows.map((row) => `INSERT INTO ${target} (${columnList}) VALUES (${row.map((value) => valueLiteral(value, dialect)).join(", ")});`).join("\n");
}
