/**
 * SQLite UI module. Tables and views use the shared grid and design view (a table without a primary key is edited by
 * rowid, a view is read-only: both come from the backend's page, not from this module). It adds the definition view of
 * indexes and triggers and the trigger icon. Object actions (explain, analyze, delete all rows, reindex) come from the
 * tree nodes themselves; the file picker and the attached databases are fields of the engine descriptor.
 */
import type { EngineUiModule } from "../index";

const sqlite: EngineUiModule = {
  id: "sqlite",
  register: (register) =>
    register([
      {
        engine: "sqlite",
        kinds: ["index", "trigger"],
        role: "definition",
        icon: "design",
        title: (ctx) => `${ctx.object?.name ?? "Definition"} · definition`,
        load: async () => (await import("./definitionView")).definitionView(),
      },
    ]),
  icons: {
    trigger: "bolt",
  },
};

export default sqlite;
