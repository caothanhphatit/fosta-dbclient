/**
 * What the query editor needs from a query language: its CodeMirror extensions, where the statement under the cursor
 * starts and ends ("run current statement"), and optionally a formatter. Engine UI modules provide their own
 * (`EngineUiModule.languages`); `defaultLanguageSupport` / `sqlLanguageSupport` are the fallbacks.
 */
import type { Extension } from "@codemirror/state";
import type { ConnectionApi } from "./connectionApi";
import type { LanguageSpec, SqlDialectId } from "./contract";

export interface LanguageContext {
  language: LanguageSpec;
  connection?: ConnectionApi;
  database?: string | null;
}

export interface TextRange {
  from: number;
  to: number;
}

export interface LanguageSupport {
  id: string;
  editorExtensions(ctx: LanguageContext): Promise<Extension[]>;
  /** The statement around `offset` (trimmed, without its terminator). */
  statementAt(text: string, offset: number): TextRange;
  format?(text: string): Promise<string>;
}

export interface SplitOptions {
  /** `#` starts a line comment (MySQL, MariaDB). */
  hashComments?: boolean;
  /** `[ … ]` quotes an identifier (SQL Server). */
  bracketIdentifiers?: boolean;
}

/**
 * The statements of a script: ranges between `;` that are outside quotes ('…', "…", `…`) and comments (-- …, /* … *\/),
 * trimmed, skipping parts that hold only comments or whitespace.
 */
export function splitStatements(text: string, options: SplitOptions = {}): TextRange[] {
  const ranges: TextRange[] = [];
  let start = 0;
  let hasCode = false;
  let i = 0;
  const push = (end: number) => {
    if (hasCode) ranges.push(trimRange(text, start, end));
    hasCode = false;
  };
  while (i < text.length) {
    const char = text[i];
    const next = text[i + 1];
    if ((char === "-" && next === "-") || (char === "#" && options.hashComments)) {
      const newline = text.indexOf("\n", i);
      i = newline < 0 ? text.length : newline + 1;
      continue;
    }
    if (char === "/" && next === "*") {
      const close = text.indexOf("*/", i + 2);
      i = close < 0 ? text.length : close + 2;
      continue;
    }
    if (char === "'" || char === '"' || char === "`" || (char === "[" && options.bracketIdentifiers)) {
      const closing = char === "[" ? "]" : char;
      hasCode = true;
      i += 1;
      while (i < text.length) {
        if (text[i] === "\\" && char !== "[" && closing !== "`") {
          i += 2;
          continue;
        }
        if (text[i] === closing) {
          // A doubled quote is an escaped quote.
          if (text[i + 1] === closing) {
            i += 2;
            continue;
          }
          break;
        }
        i += 1;
      }
      i += 1;
      continue;
    }
    if (char === ";") {
      push(i);
      start = i + 1;
      i += 1;
      continue;
    }
    if (!/\s/.test(char)) hasCode = true;
    i += 1;
  }
  push(text.length);
  return ranges;
}

function trimRange(text: string, from: number, to: number): TextRange {
  while (from < to && /\s/.test(text[from])) from += 1;
  while (to > from && /\s/.test(text[to - 1])) to -= 1;
  return { from, to };
}

/** The statement containing `offset`; between statements, the one before (else the one after); empty text gives an empty range. */
export function statementAt(text: string, offset: number, options: SplitOptions = {}): TextRange {
  const ranges = splitStatements(text, options);
  if (ranges.length === 0) return { from: offset, to: offset };
  let previous: TextRange | null = null;
  for (const range of ranges) {
    if (offset >= range.from && offset <= range.to) return range;
    if (offset < range.from) return previous ?? range;
    previous = range;
  }
  return ranges[ranges.length - 1];
}

/** A plain-text language: no extensions, statements split on `;`. */
export function defaultLanguageSupport(id = "text", options: SplitOptions = {}): LanguageSupport {
  return {
    id,
    editorExtensions: async () => [],
    statementAt: (text, offset) => statementAt(text, offset, options),
  };
}

const SQL_SPLIT: Partial<Record<SqlDialectId, SplitOptions>> = {
  mariaDb: { hashComments: true },
  mySql: { hashComments: true },
  msSql: { bracketIdentifiers: true },
};

/** SQL with CodeMirror's SQL mode for the dialect (loaded on first use); `format` when a formatter is given. */
export function sqlLanguageSupport(dialect: SqlDialectId | null, format?: (text: string) => Promise<string>): LanguageSupport {
  const options = (dialect && SQL_SPLIT[dialect]) || {};
  return {
    id: "sql",
    editorExtensions: async () => {
      const lang = await import("@codemirror/lang-sql");
      const dialects: Partial<Record<SqlDialectId, typeof lang.StandardSQL>> = {
        mariaDb: lang.MariaSQL,
        mySql: lang.MySQL,
        msSql: lang.MSSQL,
        postgres: lang.PostgreSQL,
        sqlite: lang.SQLite,
        oracle: lang.PLSQL,
      };
      return [lang.sql({ dialect: (dialect && dialects[dialect]) || lang.StandardSQL })];
    },
    statementAt: (text, offset) => statementAt(text, offset, options),
    format,
  };
}
