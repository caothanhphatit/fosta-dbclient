// Dialect helpers: mapping engine ids to dialects, and quoting identifiers the way each dialect needs.
import type { LangDialect } from "../types";
import { isReservedWord } from "./keywords";

/** The dialect of an engine or driver id (`mysql`, `mariadb`, `mssql`, `postgres`, ...); unknown ids are generic. */
export function dialectFor(engine: string | null | undefined): LangDialect {
  switch ((engine ?? "").toLowerCase()) {
    case "mariadb":
      return "mariaDb";
    case "mysql":
      return "mySql";
    case "mssql":
    case "sqlserver":
    case "tsql":
      return "msSql";
    case "postgres":
    case "postgresql":
    case "pg":
      return "postgres";
    case "sqlite":
      return "sqlite";
    case "clickhouse":
      return "clickHouse";
    case "oracle":
      return "oracle";
    default:
      return "generic";
  }
}

const PLAIN = /^[A-Za-z_][A-Za-z0-9_$]*$/;
const MSSQL_PLAIN = /^[A-Za-z_#@][A-Za-z0-9_$#@]*$/;

/** True when `name` must be quoted to be read back as the same identifier. */
export function needsQuoting(name: string, dialect: LangDialect): boolean {
  if (name === "") return true;
  const plain = dialect === "msSql" ? MSSQL_PLAIN : PLAIN;
  if (!plain.test(name)) return true;
  if (isReservedWord(dialect, name)) return true;
  // Unquoted names fold to lower case in PostgreSQL and to upper case in Oracle.
  if (dialect === "postgres" && name !== name.toLowerCase()) return true;
  if (dialect === "oracle" && name !== name.toUpperCase()) return true;
  return false;
}

/** Quotes an identifier unconditionally. */
export function quoteName(name: string, dialect: LangDialect): string {
  switch (dialect) {
    case "mySql":
    case "mariaDb":
    case "clickHouse":
      return "`" + name.replace(/`/g, "``") + "`";
    case "msSql":
      return "[" + name.replace(/]/g, "]]") + "]";
    default:
      return '"' + name.replace(/"/g, '""') + '"';
  }
}

/** Quotes an identifier only when it needs it. */
export function quoteIfNeeded(name: string, dialect: LangDialect): string {
  return needsQuoting(name, dialect) ? quoteName(name, dialect) : name;
}

/** The text of an identifier token without its quotes (`[a]]b]` -> `a]b`, `"x""y"` -> `x"y`). */
export function unquote(text: string): string {
  if (text.length < 2) return text;
  const open = text[0];
  const close = open === "[" ? "]" : open;
  if ((open === '"' || open === "`" || open === "[") && text.endsWith(close)) {
    return text
      .slice(1, -1)
      .split(close + close)
      .join(close);
  }
  return text;
}

/** The closing quote character for an opening one. */
export function closingQuote(open: string): string {
  return open === "[" ? "]" : open;
}
