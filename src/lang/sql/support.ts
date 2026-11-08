// The SQL `LanguageSupport`: highlighting per dialect, the schema-driven completion source, statement at cursor and
// the backend formatter, plus editor helpers for the "Run current statement" and "Format" actions.
import { MariaSQL, MSSQL, MySQL, PLSQL, PostgreSQL, SQLDialect, SQLite, StandardSQL } from "@codemirror/lang-sql";
import { LanguageSupport as CMLanguageSupport } from "@codemirror/language";
import type { EditorState, Extension } from "@codemirror/state";
import { currentStatement, type CurrentStatement } from "../editor";
import { SchemaCache } from "../schemaCache";
import type { FormatOptions, LangDialect, LanguageContext, LanguageSupport } from "../types";
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
      const connection = ctx?.connection;
      if (ownsCache && connection) void schema.load(() => connection.completionSchema(ctx.database ?? null));
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

export { currentStatement, formatInEditor, type CurrentStatement } from "../editor";

/** `currentStatement` for a SQL dialect. */
export function currentSqlStatement(state: EditorState, dialect: LangDialect): CurrentStatement | null {
  return currentStatement(state, (text, offset) => statementRangeAt(dialect, text, offset));
}
