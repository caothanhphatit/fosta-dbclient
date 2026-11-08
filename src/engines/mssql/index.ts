/**
 * SQL Server UI module (a stub: SQL Server uses the shared views from `../shared.ts`).
 *
 * The SQL Server agent adds here: T-SQL language (GO batches, bracket quoting), schema folders in the tree, an execution plan view. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/mssql/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const mssql: EngineUiModule = {
  id: "mssql",
  register: () => undefined,
};

export default mssql;
