import type { SqlDialectId } from "./contract";
import type { Cell, CellValue, ColumnMeta } from "./types";

/**
 * How the UI writes identifiers and literals into the SQL it builds (filters, foreign-key navigation, INSERT
 * export): `mysql` (MariaDB, MySQL: backticks, backslash escapes), `tsql` (SQL Server: brackets, N'…' strings)
 * and `ansi` (double-quoted identifiers, standard strings) for every other SQL engine.
 */
export type SqlDialect = "mysql" | "tsql" | "ansi";

export function sqlDialect(driver: string): SqlDialect {
  switch (driver) {
    case "mariadb":
    case "mysql":
      return "mysql";
    case "mssql":
      return "tsql";
    default:
      return "ansi";
  }
}

/** The quoting rules of an engine descriptor's `sqlDialect` (prefer this to `sqlDialect(driver)`, which compares ids). */
export function dialectOf(id: SqlDialectId | null | undefined): SqlDialect {
  switch (id) {
    case "mariaDb":
    case "mySql":
      return "mysql";
    case "msSql":
      return "tsql";
    default:
      return "ansi";
  }
}

export function quoteIdent(name: string, dialect: SqlDialect): string {
  switch (dialect) {
    case "mysql":
      return `\`${name.replace(/`/g, "``")}\``;
    case "tsql":
      return `[${name.replace(/]/g, "]]")}]`;
    case "ansi":
      return `"${name.replace(/"/g, '""')}"`;
  }
}

/**
 * A table name as the tree gives it. SQL Server and the ANSI engines address tables as `schema.table`, so the
 * first dot separates the schema; MySQL table names are plain (and may contain dots).
 */
export function quoteTable(name: string, dialect: SqlDialect): string {
  const dot = name.indexOf(".");
  if (dialect === "mysql" || dot <= 0 || dot === name.length - 1) return quoteIdent(name, dialect);
  return `${quoteIdent(name.slice(0, dot), dialect)}.${quoteIdent(name.slice(dot + 1), dialect)}`;
}

export function sqlLiteral(value: Cell, column: ColumnMeta | undefined, dialect: SqlDialect): string {
  if (value === null) return "NULL";
  if (column?.binary && dialect !== "ansi" && /^0x[0-9a-f]*$/i.test(value)) return value;
  if (column?.numeric && /^-?\d+(\.\d+)?(e[+-]?\d+)?$/i.test(value)) return value;
  return stringLiteral(value, dialect);
}

function stringLiteral(value: string, dialect: SqlDialect): string {
  const quoted = value.replace(/'/g, "''");
  switch (dialect) {
    case "mysql":
      // Backslash is an escape character in MySQL string literals (unless NO_BACKSLASH_ESCAPES).
      return `'${quoted.replace(/\\/g, "\\\\")}'`;
    case "tsql":
      // N'…' keeps non-Latin text intact when compared with nvarchar columns.
      return `N'${quoted}'`;
    case "ansi":
      return `'${quoted}'`;
  }
}

/** A "contains" LIKE pattern for `text`. SQL Server treats `[` as a wildcard too, so it is escaped there. */
export function containsPattern(text: string, dialect: SqlDialect): string {
  const escaped = dialect === "tsql" ? text.replace(/\[/g, "[[]") : text;
  return stringLiteral(`%${escaped}%`, dialect);
}

export function equalsFilter(pairs: { column: string; value: Cell; meta?: ColumnMeta }[], dialect: SqlDialect): string {
  return pairs
    .map(({ column, value, meta }) =>
      value === null ? `${quoteIdent(column, dialect)} IS NULL` : `${quoteIdent(column, dialect)} = ${sqlLiteral(value, meta, dialect)}`,
    )
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

export function describeKey(key: CellValue[], dialect: SqlDialect): string {
  return equalsFilter(
    key.map((cell) => ({ column: cell.column, value: cell.value })),
    dialect,
  );
}
