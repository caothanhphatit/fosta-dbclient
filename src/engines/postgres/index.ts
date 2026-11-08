/**
 * PostgreSQL UI module (a stub: PostgreSQL uses the shared views from `../shared.ts`).
 *
 * The PostgreSQL agent adds here: schema-aware tree icons, PL/pgSQL language support, EXPLAIN (ANALYZE) view, extensions overview. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/postgres/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const postgres: EngineUiModule = {
  id: "postgres",
  register: () => undefined,
};

export default postgres;
