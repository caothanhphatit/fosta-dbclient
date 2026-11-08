/**
 * ClickHouse UI module (a stub: ClickHouse uses the shared views from `../shared.ts`).
 *
 * The ClickHouse agent adds here: dictionary and materialized view views, ClickHouse SQL language, query progress display. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/clickhouse/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const clickhouse: EngineUiModule = {
  id: "clickhouse",
  register: () => undefined,
};

export default clickhouse;
