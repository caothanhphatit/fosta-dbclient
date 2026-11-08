// The Mongo console `LanguageSupport` (language id "mongo-command"): completion and statement at cursor.
import { EditorState, type Extension } from "@codemirror/state";
import { fetchCompletionSchema, SchemaCache } from "../schemaCache";
import type { LanguageContext, LanguageSupport } from "../types";
import { mongoCompletionSource, mongoStatementAt } from "./completion";

export interface MongoLanguageOptions {
  /** Collections (tables) and their sampled fields (columns); loaded from `completion_schema` when omitted. */
  schema?: SchemaCache;
}

export function createMongoLanguage(options: MongoLanguageOptions = {}): LanguageSupport & { schema: SchemaCache } {
  const schema = options.schema ?? new SchemaCache();
  const ownsCache = !options.schema;
  return {
    id: "mongo-command",
    schema,
    async editorExtensions(ctx?: LanguageContext): Promise<Extension[]> {
      if (ownsCache && ctx?.connectionId) {
        const { connectionId } = ctx;
        void schema.load(() => fetchCompletionSchema(connectionId, ctx.database ?? null));
      }
      return [mongoEditorExtension(schema)];
    },
    statementAt: mongoStatementAt,
  };
}

/** Completion for an editor without a parsed language (the console text is shell syntax or JSON). */
export function mongoEditorExtension(schema: SchemaCache): Extension {
  const source = mongoCompletionSource(() => schema.get());
  return EditorState.languageData.of(() => [{ autocomplete: source }]);
}
