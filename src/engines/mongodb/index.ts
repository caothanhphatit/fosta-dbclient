/**
 * MongoDB UI module:
 *
 * - `workspace` on a collection or a view: the collection workspace (`workspace.ts`), the default open; its Documents section
 *   has the query bar (mongosh syntax, completion of fields, operators and values) and the documents (editable) as a tree, JSON
 *   or a table,
 * - the `mongo-command` console language; the grid and the design view are the shared ones (`../shared.ts`).
 * - query-history entries of the query bar reopen in their collection workspace (`historyTarget`).
 * - the assistant's `mongo-filter` and `mongo-pipeline` blocks get "Apply to query bar", "Open in Aggregations",
 *   "Insert as stage(s)" and "Replace stage" (`assistantFences.ts`).
 *
 * Views are chunks of their own; styles are `mongodb.css`, imported by the views (never `src/styles.css`).
 */
import { jsonLanguageSupport, type LanguageSupport } from "../../core/language";
import type { ViewRegistration } from "../../core/views";
import type { EngineUiModule } from "../index";

const ENGINE = "mongodb";

const views: ViewRegistration[] = [
  {
    engine: ENGINE,
    kinds: ["collection", "view"],
    role: "workspace",
    icon: "table",
    load: async () => (await import("./workspace")).collectionWorkspaceFactory,
  },
];

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
  id: ENGINE,
  register: (register) => register(views),
  languages: { "mongo-command": mongoCommandLanguage },
  // A query-bar run in the query history reopens in its collection workspace, with the bar filled in.
  historyTarget: async (entry) => (await import("./savedQueries")).historyTarget(entry),
  assistantFences: {
    "mongo-filter": async () => (await import("./assistantFences")).filterFenceActions,
    "mongo-pipeline": async () => (await import("./assistantFences")).pipelineFenceActions,
  },
};

export default mongodb;
