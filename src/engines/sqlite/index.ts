/**
 * SQLite UI module (a stub: SQLite uses the shared views from `../shared.ts`).
 *
 * The SQLite agent adds here: file-based connection helpers, PRAGMA overview, SQLite SQL language. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/sqlite/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const sqlite: EngineUiModule = {
  id: "sqlite",
  register: () => undefined,
};

export default sqlite;
