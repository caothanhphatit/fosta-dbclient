/**
 * MySQL UI module (a stub: MySQL uses the shared views from `../shared.ts`).
 *
 * The MySQL agent adds here: server overview, MySQL SQL language with completion; shares most of its views with MariaDB. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/mysql/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const mysql: EngineUiModule = {
  id: "mysql",
  register: () => undefined,
};

export default mysql;
