/**
 * Redis UI module (a stub: Redis uses the shared views from `../shared.ts`).
 *
 * The Redis agent adds here: key views per type (string, hash, list, set, zset, stream), a key browser by pattern, the Redis console language, icons for key types. Views go through `register`, editor languages through `languages`, node icons
 * through `icons`, and styles in `src/engines/redis/*.css` imported by those views (never `src/styles.css`).
 */
import type { EngineUiModule } from "../index";

const redis: EngineUiModule = {
  id: "redis",
  register: () => undefined,
  // The console language: command and option completion with arity hints, one command per line.
  languages: { redis: async () => (await import("../../lang/redis/support")).createRedisLanguage() },
};

export default redis;
