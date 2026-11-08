// Shared shapes of the language library. `LanguageSupport` matches the hook the query view calls (run current
// statement, format); `CompletionSchema` matches the Rust `lang::CompletionSchema` returned by `completion_schema`.
import type { Extension } from "@codemirror/state";

/** SQL dialect ids, identical to the engine contract's `SqlDialectId` (camelCase serde names). */
export type LangDialect = "mariaDb" | "mySql" | "msSql" | "postgres" | "sqlite" | "clickHouse" | "oracle" | "generic";

export const ALL_DIALECTS: readonly LangDialect[] = ["mariaDb", "mySql", "msSql", "postgres", "sqlite", "clickHouse", "oracle", "generic"];

export interface ForeignRef {
  table: string;
  column: string;
  schema?: string;
}

export interface CompletionColumn {
  name: string;
  type: string;
  pk: boolean;
  fk?: ForeignRef;
}

export interface CompletionTable {
  name: string;
  schema?: string;
  /** "table" | "view" | "materializedView" | "collection" | ... */
  kind: string;
  columns: CompletionColumn[];
}

export interface CompletionSchema {
  databases: string[];
  schemas: string[];
  /** The schema unqualified names resolve to (public, dbo, the current database for MySQL). */
  defaultSchema?: string;
  tables: CompletionTable[];
  truncated?: boolean;
}

export const EMPTY_SCHEMA: CompletionSchema = { databases: [], schemas: [], tables: [] };

/** A range in the editor text (UTF-16 offsets, as CodeMirror uses). */
export interface TextRange {
  from: number;
  to: number;
}

/** What the query view may pass when it builds an editor. Every field is optional. */
export interface LanguageContext {
  connectionId?: string;
  database?: string | null;
}

/** The editor language hook: extensions for the editor, the statement under the cursor, and an optional formatter. */
export interface LanguageSupport {
  id: string;
  editorExtensions(ctx?: LanguageContext): Promise<Extension[]>;
  statementAt(text: string, offset: number): TextRange;
  format?(text: string): Promise<string>;
}

export type KeywordCase = "upper" | "lower" | "preserve";

/** Options of the backend formatter (`format_sql`), camelCase like the Rust `FormatOptions`. */
export interface FormatOptions {
  indent?: number;
  useTabs?: boolean;
  keywordCase?: KeywordCase;
  maxLine?: number;
}
