/**
 * MongoDB UI module (a stub: MongoDB uses the shared views from `../shared.ts`).
 *
 * The MongoDB agent adds here: a document view (JSON tree), the aggregation pipeline builder, the query bar with syntax help, mongo-command language. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/mongodb/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const mongodb: EngineUiModule = {
  id: "mongodb",
  register: () => undefined,
};

export default mongodb;
