// The SQL `LanguageSupport`: highlighting per dialect, the schema-driven completion source, statement at cursor and
// the backend formatter, plus editor helpers for the "Run current statement" and "Format" actions.
import { MariaSQL, MSSQL, MySQL, PLSQL, PostgreSQL, SQLDialect, SQLite, StandardSQL } from "@codemirror/lang-sql";
import { LanguageSupport as CMLanguageSupport } from "@codemirror/language";
import type { EditorState, Extension } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { fetchCompletionSchema, SchemaCache } from "../schemaCache";
import type { FormatOptions, LangDialect, LanguageContext, LanguageSupport, TextRange } from "../types";
import { sqlCompletionSource } from "./completion";
import { statementAt, statementRangeAt } from "./split";

const ClickHouseSQL = SQLDialect.define({
  backslashEscapes: true,
  doubleQuotedStrings: false,
  hashComments: true,
  identifierQuotes: '`"',
  keywords:
    "select from where prewhere final sample array join global any asof left right inner outer full cross semi anti on using group by order limit offset with having union all format settings insert into values create table database view materialized dictionary engine partition primary key ttl alter drop truncate optimize system show describe exists if not as and or in between like ilike is null case when then else end distinct",
  types:
    "UInt8 UInt16 UInt32 UInt64 Int8 Int16 Int32 Int64 Float32 Float64 Decimal String FixedString Date Date32 DateTime DateTime64 UUID Array Tuple Map Nullable LowCardinality Enum8 Enum16 Bool JSON",
});

/** The CodeMirror dialect used for highlighting. */
export function codemirrorDialect(dialect: LangDialect): SQLDialect {
  switch (dialect) {
    case "mySql":
      return MySQL;
    case "mariaDb":
      return MariaSQL;
    case "msSql":
      return MSSQL;
    case "postgres":
      return PostgreSQL;
    case "sqlite":
      return SQLite;
    case "clickHouse":
      return ClickHouseSQL;
    case "oracle":
      return PLSQL;
    default:
      return StandardSQL;
  }
}

/** Calls the backend `format_sql` command. */
export async function formatSqlRemote(dialect: LangDialect, text: string, options?: FormatOptions): Promise<string> {
  const { invoke } = await import("../../core/transport");
  return invoke<string>("format_sql", { dialect, text, options: options ?? null });
}

export interface SqlLanguageOptions {
  dialect: LangDialect;
  /** Schema cache shared with the caller; one is created (and loaded from `completion_schema`) when omitted. */
  schema?: SchemaCache;
  /** Formatter; defaults to the backend `format_sql` command. */
  format?: (dialect: LangDialect, text: string, options?: FormatOptions) => Promise<string>;
  formatOptions?: FormatOptions;
}

export interface SqlLanguageSupport extends LanguageSupport {
  dialect: LangDialect;
  schema: SchemaCache;
}

/** Builds the SQL language hook for one dialect. */
export function createSqlLanguage(options: SqlLanguageOptions): SqlLanguageSupport {
  const { dialect } = options;
  const schema = options.schema ?? new SchemaCache();
  const ownsCache = !options.schema;
  const format = options.format ?? formatSqlRemote;
  return {
    id: "sql",
    dialect,
    schema,
    async editorExtensions(ctx?: LanguageContext): Promise<Extension[]> {
      if (ownsCache && ctx?.connectionId) {
        const { connectionId } = ctx;
        void schema.load(() => fetchCompletionSchema(connectionId, ctx.database ?? null));
      }
      return [sqlEditorLanguage(dialect, schema)];
    },
    statementAt: (text, offset) => statementAt(dialect, text, offset),
    format: (text) => format(dialect, text, options.formatOptions),
  };
}

/** Highlighting plus schema-driven completion, without lang-sql's own keyword completion. */
export function sqlEditorLanguage(dialect: LangDialect, schema: SchemaCache): Extension {
  const cm = codemirrorDialect(dialect);
  const source = sqlCompletionSource({ dialect, schema: () => schema.get() });
  return new CMLanguageSupport(cm.language, cm.language.data.of({ autocomplete: source }));
}

export interface CurrentStatement extends TextRange {
  text: string;
}

/**
 * What "Run current statement" runs: the selection when there is one, else the statement under the cursor (null
 * when the editor holds no statement).
 */
export function currentStatement(state: EditorState, statementAtFn: (text: string, offset: number) => TextRange | null): CurrentStatement | null {
  const sel = state.selection.main;
  if (!sel.empty) return { from: sel.from, to: sel.to, text: state.sliceDoc(sel.from, sel.to) };
  const doc = state.doc.toString();
  const range = statementAtFn(doc, sel.head);
  if (!range || range.from === range.to) return null;
  return { ...range, text: doc.slice(range.from, range.to) };
}

/** `currentStatement` for a SQL dialect. */
export function currentSqlStatement(state: EditorState, dialect: LangDialect): CurrentStatement | null {
  return currentStatement(state, (text, offset) => statementRangeAt(dialect, text, offset));
}

/**
 * Formats the selection (or the whole document) with `support.format` and replaces it in one undoable change.
 * Returns false when the language has no formatter or the text did not change.
 */
export async function formatInEditor(view: EditorView, support: Pick<LanguageSupport, "format">): Promise<boolean> {
  if (!support.format) return false;
  const sel = view.state.selection.main;
  const from = sel.empty ? 0 : sel.from;
  const to = sel.empty ? view.state.doc.length : sel.to;
  const before = view.state.sliceDoc(from, to);
  const after = await support.format(before);
  // The document may have changed while the formatter ran.
  if (after === before || view.state.sliceDoc(from, to) !== before) return false;
  view.dispatch({ changes: { from, to, insert: after }, userEvent: "input.format" });
  return true;
}
