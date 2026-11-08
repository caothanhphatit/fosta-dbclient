/**
 * Redis UI module. Redis has no rows, so it brings its own views (the console is the shared query view with the Redis
 * language):
 *
 * - `keys` on a database: the key browser (SCAN with a pattern, type, TTL, memory),
 * - `value` and `data` on a key: the typed value editor (string with a JSON pretty view, hash, list, set, sorted set,
 *   stream), TTL set and remove, edits guarded against concurrent changes,
 * - `server` on the connection: INFO and the slow log.
 *
 * Each view is a chunk of its own; styles are `redis.css`, imported by the views.
 */
import type { ViewRegistration } from "../../core/views";
import type { EngineUiModule } from "../index";

const ENGINE = "redis";

const views: ViewRegistration[] = [
  {
    engine: ENGINE,
    kinds: ["database"],
    role: "keys",
    icon: "key",
    title: (ctx) => `db${ctx.object?.database ?? ""} · keys`,
    load: async () => (await import("./keysView")).keysViewFactory,
  },
  // A key opens in the value editor from the tree (`value`), and from the key list through the navigator (`data`).
  { engine: ENGINE, kinds: ["key"], role: "value", icon: "key", load: async () => (await import("./valueView")).valueViewFactory },
  { engine: ENGINE, kinds: ["key"], role: "data", icon: "key", load: async () => (await import("./valueView")).valueViewFactory },
  {
    engine: ENGINE,
    kinds: ["connection"],
    role: "server",
    icon: "server",
    title: () => "Server info",
    load: async () => (await import("./serverView")).serverViewFactory,
  },
];

const redis: EngineUiModule = {
  id: ENGINE,
  register: (register) => register(views),
  // The console language: command and option completion with arity hints, one command per line.
  languages: { redis: async () => (await import("../../lang/redis/support")).createRedisLanguage() },
};

export default redis;
