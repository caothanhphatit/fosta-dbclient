/**
 * Elasticsearch UI module. Indices, data streams and aliases are `collection` nodes, so the shared document grid (typed
 * by the mapping, Query DSL raw filters, keyset paging) and the design view (the mapping) open them. The engine adds
 * the Dev Tools console language (`es-console`: highlighting, completion of endpoints, index names, Query DSL keys and
 * fields, run-current-request, body formatting); ES|QL and SQL use the SQL editor. Console runs show a hits table and
 * the JSON response per request.
 */
import type { EngineUiModule } from "../index";

const elasticsearch: EngineUiModule = {
  id: "elasticsearch",
  register: () => undefined,
  languages: {
    "es-console": async () => (await import("./language")).consoleLanguage(),
  },
};

export default elasticsearch;
