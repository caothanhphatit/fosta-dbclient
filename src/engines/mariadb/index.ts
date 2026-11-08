/**
 * MariaDB UI module (a stub: MariaDB uses the shared views from `../shared.ts`).
 *
 * The MariaDB agent adds here: vector views (search, health) through `invoke_capability`, server overview, MariaDB SQL language with completion. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/mariadb/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const mariadb: EngineUiModule = {
  id: "mariadb",
  register: () => undefined,
};

export default mariadb;
