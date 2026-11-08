/**
 * PostgreSQL UI module. Tables, views and materialized views use the shared grid and design view; this module adds the
 * definition view for objects without rows (functions, procedures, sequences, types, extensions) and the icons the
 * backend's tree names as `engine:postgres/<name>`. Object actions (explain, analyze, truncate, refresh) come from the
 * tree nodes themselves, so they need nothing here.
 */
import type { EngineUiModule } from "../index";

const postgres: EngineUiModule = {
  id: "postgres",
  register: (register) =>
    register([
      {
        engine: "postgres",
        kinds: ["function", "procedure", "sequence", "type", "extension"],
        role: "definition",
        icon: "design",
        title: (ctx) => `${ctx.object?.name ?? "Definition"} · definition`,
        load: async () => (await import("./definitionView")).definitionView(),
      },
    ]),
  icons: {
    sequence: "expand",
    extension: "plug",
    enum: "columns",
    type: "layers",
  },
};

export default postgres;
