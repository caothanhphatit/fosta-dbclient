/**
 * Elasticsearch UI module (a stub: Elasticsearch uses the shared views from `../shared.ts`).
 *
 * The Elasticsearch agent adds here: index and mapping views, the Console (REST) language, ES|QL and SQL languages, cluster health overview. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/elasticsearch/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const elasticsearch: EngineUiModule = {
  id: "elasticsearch",
  register: () => undefined,
};

export default elasticsearch;
