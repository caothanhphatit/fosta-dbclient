/**
 * Oracle UI module (a stub: Oracle uses the shared views from `../shared.ts`).
 *
 * The Oracle agent adds here: package and PL/SQL views, PL/SQL language, schema browser icons. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/oracle/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const oracle: EngineUiModule = {
  id: "oracle",
  register: () => undefined,
};

export default oracle;
