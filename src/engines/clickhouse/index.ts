/**
 * ClickHouse UI module. The tree, the design view and the query tab are the shared ones; ClickHouse adds:
 *
 * - `data` on tables: the shared grid with a notice that edits are asynchronous mutations, and read-only for table engines
 *   that have none (anything but MergeTree),
 * - `parts` on tables: data parts and mutations (with Kill),
 * - `server` on the connection: running queries (with Stop) and the query log with duration, memory and rows.
 *
 * Each view is a chunk of its own; styles are `clickhouse.css`, imported by the views.
 */
import type { ViewRegistration } from "../../core/views";
import type { EngineUiModule } from "../index";

const ENGINE = "clickhouse";

const views: ViewRegistration[] = [
  { engine: ENGINE, kinds: ["table"], role: "data", icon: "table", load: async () => (await import("./dataView")).dataViewFactory },
  {
    engine: ENGINE,
    kinds: ["table"],
    role: "parts",
    icon: "layers",
    title: (ctx) => `${ctx.object?.name ?? ""} · parts`,
    load: async () => (await import("./partsView")).partsViewFactory,
  },
  {
    engine: ENGINE,
    kinds: ["connection"],
    role: "server",
    icon: "server",
    title: () => "Server",
    load: async () => (await import("./serverView")).serverViewFactory,
  },
];

const clickhouse: EngineUiModule = {
  id: ENGINE,
  register: (register) => register(views),
};

export default clickhouse;
