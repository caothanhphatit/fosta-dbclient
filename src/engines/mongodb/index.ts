/**
 * MongoDB UI module (a stub: MongoDB uses the shared views from `../shared.ts`).
 *
 * The MongoDB agent adds here: a document view (JSON tree), the aggregation pipeline builder, the query bar with syntax help, mongo-command language. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/mongodb/*.css` imported by those views (never `src/styles.css`).
 */
import { jsonLanguageSupport, type LanguageSupport } from "../../core/language";
import type { EngineUiModule } from "../index";

/**
 * Database commands: JSON highlighting and formatting, completion of command names, operators, collections and their
 * sampled fields (`completion_schema`), and the command under the cursor from the Mongo statement splitter.
 */
async function mongoCommandLanguage(): Promise<LanguageSupport> {
  const { createMongoLanguage } = await import("../../lang/mongo/support");
  const json = jsonLanguageSupport("mongo-command");
  const mongo = createMongoLanguage();
  return {
    id: "mongo-command",
    editorExtensions: async (ctx) => [...(await json.editorExtensions(ctx)), ...(await mongo.editorExtensions(ctx))],
    statementAt: mongo.statementAt,
    format: json.format,
  };
}

const mongodb: EngineUiModule = {
  id: "mongodb",
  register: () => undefined,
  languages: { "mongo-command": mongoCommandLanguage },
};

export default mongodb;
