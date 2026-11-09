/**
 * What the query editor needs from a query language: its CodeMirror extensions, where the statement under the cursor
 * starts and ends ("run current statement"), and optionally a formatter. Engine UI modules provide their own
 * (`EngineUiModule.languages`); `defaultLanguageSupport` / `sqlLanguageSupport` are the fallbacks.
 */
import type { Extension } from "@codemirror/state";
import { SchemaCache } from "../lang/schemaCache";
import { statementAt as sqlStatementAt } from "../lang/sql/split";
import { api } from "./api";
import type { ConnectionApi } from "./connectionApi";
import type { LanguageSpec, SqlDialectId } from "./contract";

export interface LanguageContext {
  language: LanguageSpec;
  connection?: ConnectionApi;
  database?: string | null;
  /** Load the names again even when they were just loaded for this database (after DDL, or "Refresh names"). */
  reload?: boolean;
}

let reloadSequence = 0;

export interface TextRange {
  from: number;
  to: number;
}

export interface LanguageSupport {
  id: string;
  editorExtensions(ctx?: LanguageContext): Promise<Extension[]>;
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

/**
 * SQL from the language library (`src/lang`): highlighting for the dialect, completion from `completion_schema` for the
 * context database (tables, columns, joins along foreign keys), the dialect-aware statement splitter, and `format`
 * (the given formatter, else the backend `format_sql`). CodeMirror's SQL package loads on first use.
 */
export function sqlLanguageSupport(dialect: SqlDialectId | null, format?: (text: string) => Promise<string>): LanguageSupport {
  const langDialect = dialect ?? "generic";
  const schema = new SchemaCache();
  let language: Promise<LanguageSupport> | null = null;
  const load = () =>
    (language ??= import("../lang/sql/support").then(({ createSqlLanguage }) =>
      createSqlLanguage({
        dialect: langDialect,
        schema,
        format: (_dialect, text, options) => (format ? format(text) : api.formatSql(dialect, text, options ?? null)),
      }),
    ));
  return {
    id: "sql",
    editorExtensions: async (ctx) => {
      const connection = ctx?.connection;
      const database = ctx?.database;
      if (connection && database) {
        const key = ctx?.reload ? `${database}#${++reloadSequence}` : database;
        void schema.load(() => connection.completionSchema(database), key);
      }
      return (await load()).editorExtensions(ctx);
    },
    statementAt: (text, offset) => sqlStatementAt(langDialect, text, offset),
    format: async (text) => {
      const support = await load();
      return support.format ? support.format(text) : text;
    },
  };
}

/**
 * A JSON command language (MongoDB database commands): JSON highlighting, the whole text as one statement, and a
 * whitespace-only formatter (`formatJsonLike`) so numbers and extended-JSON spellings stay exactly as typed.
 */
export function jsonLanguageSupport(id = "json"): LanguageSupport {
  return {
    id,
    editorExtensions: async () => [(await import("./jsonMode")).jsonMode()],
    statementAt: (text) => trimRange(text, 0, text.length),
    format: async (text) => formatJsonLike(text),
  };
}

/**
 * Re-indents JSON (or relaxed JSON such as `{ find: "x" }` and `ObjectId("…")`) by two spaces without touching the
 * tokens: strings are copied verbatim, numbers are never re-parsed. Text with comments comes back unchanged.
 */
export function formatJsonLike(text: string, indent = "  "): string {
  let out = "";
  let depth = 0;
  let i = 0;
  const newline = () => `\n${indent.repeat(depth)}`;
  const word = (char: string | undefined) => char !== undefined && /[\w$.]/.test(char);
  while (i < text.length) {
    const char = text[i];
    if (char === '"' || char === "'") {
      let end = i + 1;
      while (end < text.length && text[end] !== char) end += text[end] === "\\" ? 2 : 1;
      out += text.slice(i, end + 1);
      i = end + 1;
      continue;
    }
    if (char === "/" && (text[i + 1] === "/" || text[i + 1] === "*")) return text;
    if (/\s/.test(char)) {
      while (i < text.length && /\s/.test(text[i])) i += 1;
      if (word(out[out.length - 1]) && word(text[i])) out += " ";
      continue;
    }
    if (char === "{" || char === "[") {
      const close = char === "{" ? "}" : "]";
      let next = i + 1;
      while (next < text.length && /\s/.test(text[next])) next += 1;
      if (text[next] === close) {
        out += char + close;
        i = next + 1;
        continue;
      }
      depth += 1;
      out += char + newline();
    } else if (char === "}" || char === "]") {
      depth = Math.max(0, depth - 1);
      out += newline() + char;
    } else if (char === ",") {
      out += `,${newline()}`;
    } else if (char === ":") {
      out += ": ";
    } else {
      out += char;
    }
    i += 1;
  }
  return out;
}
