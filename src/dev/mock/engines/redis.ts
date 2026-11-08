/**
 * Redis 8 mock: sixteen databases with users (hashes), sessions (strings with TTLs), a JSON config string, a leaderboard
 * (sorted set), a job queue (list), tags (set) and an events stream. It answers the engine's operations (`redis.scan`,
 * `redis.keyInfo`, `redis.readValue`, `redis.edit` with `expect` guards, `redis.info`, `redis.slowlog`) and runs console
 * commands on the same data.
 *
 * Conflict demo: open a key, edit a value, then in the browser console run `__fostaMock.touchRow("c-local", "db0", "users:1")`
 * (someone else changes the key) and save: `redis.edit` fails with `CONFLICT`.
 */
import type { ColumnInfo, ConnectInfo, EngineDescriptor, ObjectDescription, ObjectNode, RunRequest, StreamEvent, Value } from "../../../core/contract";
import type { StatementInfo } from "../../../core/types";
import { builtin, queryAction, viewAction } from "../catalog";
import { DESCRIPTORS, ORDER } from "../descriptors";
import { mockError } from "../errors";
import { pause } from "../sqlRun";
import type { EngineMock, RunContext } from "../types";

export const descriptor: EngineDescriptor = DESCRIPTORS.redis;
export const order = ORDER.indexOf("redis");

const ENGINE = "redis";
const VERSION = "8.2.1";
const DATABASES = 16;
const NOW = Date.now();

type Kind = "string" | "hash" | "list" | "set" | "zset" | "stream";
interface Entry {
  type: Kind;
  value: string | Map<string, string> | string[] | Set<string> | Map<string, number> | { id: string; fields: [string, string][] }[];
  expiresAt: number | null;
}

const blob = (text: string) => ({ text });
const text = (b: { text?: string } | undefined | null): string => b?.text ?? "";

const store: Map<number, Map<string, Entry>> = new Map(Array.from({ length: DATABASES }, (_, db) => [db, new Map<string, Entry>()]));
const db = (n: number): Map<string, Entry> => {
  const found = store.get(n);
  if (!found) throw serverError("ERR", "DB index is out of range");
  return found;
};

function seed(): void {
  const zero = db(0);
  const plans = ["free", "pro", "team"];
  for (let n = 1; n <= 40; n += 1)
    zero.set(`users:${n}`, {
      type: "hash",
      value: new Map([
        ["name", `User ${n}`],
        ["email", `user${n}@example.com`],
        ["plan", plans[n % 3]],
        ["visits", String(n * 7)],
      ]),
      expiresAt: null,
    });
  for (let n = 1; n <= 25; n += 1) zero.set(`sessions:${1000 + n}`, { type: "string", value: `{"user":${n},"ip":"10.0.0.${n}"}`, expiresAt: NOW + n * 90_000 });
  zero.set("cache:config", { type: "string", value: '{"theme":"dark","features":{"beta":true,"limits":[10,20,30]}}', expiresAt: null });
  zero.set("cache:greeting", { type: "string", value: "Hello, Redis", expiresAt: NOW + 3_600_000 });
  zero.set("leaderboard", {
    type: "zset",
    value: new Map(Array.from({ length: 30 }, (_, i) => [`player${i + 1}`, 1000 - i * 17] as [string, number])),
    expiresAt: null,
  });
  zero.set("queue:jobs", { type: "list", value: Array.from({ length: 60 }, (_, i) => `job-${i + 1}`), expiresAt: null });
  zero.set("tags:popular", { type: "set", value: new Set(["redis", "cache", "queue", "stream", "search"]), expiresAt: null });
  zero.set("events", {
    type: "stream",
    value: Array.from({ length: 40 }, (_, i) => ({
      id: `${1_760_000_000_000 + i * 1000}-0`,
      fields: [
        ["kind", i % 3 ? "click" : "view"],
        ["user", String(i % 9)],
      ] as [string, string][],
    })),
    expiresAt: null,
  });
  db(1).set("scratch:note", { type: "string", value: "a key in database 1", expiresAt: null });
}
seed();

const serverError = (code: string, message: string) =>
  mockError("ENGINE_ERROR", message, { engine: ENGINE, vendorCode: code, kind: "server", retryable: false });

const alive = (entries: Map<string, Entry>, key: string): Entry | undefined => {
  const entry = entries.get(key);
  if (entry?.expiresAt && entry.expiresAt <= Date.now()) {
    entries.delete(key);
    return undefined;
  }
  return entry;
};
const ttlOf = (entry: Entry | undefined): number => (!entry ? -2 : entry.expiresAt === null ? -1 : Math.max(1, entry.expiresAt - Date.now()));
const sizeOf = (entry: Entry): number => {
  const value = entry.value;
  if (typeof value === "string") return value.length;
  if (Array.isArray(value)) return value.length;
  return value instanceof Map || value instanceof Set ? value.size : 0;
};

// --- Tree ---------------------------------------------------------------------------------------------------------------

const databaseNode = (n: number): ObjectNode => {
  const keys = [...db(n).keys()].filter((key) => alive(db(n), key)).length;
  const object = { database: String(n), schema: null, name: `db${n}`, kind: "database" };
  return {
    key: { id: `db:${n}`, kind: "database", object },
    label: `db${n}`,
    kind: "database",
    icon: "database",
    object,
    detail: keys === 1 ? "1 key" : `${keys} keys`,
    badges: [],
    expandable: keys > 0,
    defaultView: "keys",
    actions: [
      viewAction("keys", "Browse keys", "keys"),
      builtin("newQuery", "New console", "newQuery"),
      builtin("refresh", "Refresh", "refresh", true),
      queryAction("flush", "Flush database…", "FLUSHDB", true),
    ],
  };
};

const keyNode = (n: number, key: string, entry: Entry): ObjectNode => {
  const object = { database: String(n), schema: null, name: key, kind: "key" };
  const ttl = ttlOf(entry);
  return {
    key: { id: `db:${n}/key:${key}`, kind: "key", object },
    label: key,
    kind: "key",
    icon: "key",
    object,
    detail: null,
    badges: [{ text: entry.type, tone: "neutral" }, ...(ttl >= 0 ? [{ text: `ttl ${Math.floor(ttl / 1000)}s`, tone: "warn" as const }] : [])],
    expandable: false,
    defaultView: "value",
    actions: [builtin("copyName", "Copy key", "copyName")],
  };
};

export const tree: NonNullable<EngineMock["tree"]> = (parent) => {
  if (!parent) return Array.from({ length: DATABASES }, (_, n) => databaseNode(n));
  if (parent.kind !== "database") return [];
  const n = Number(parent.object?.database ?? 0);
  return [...db(n).entries()].filter(([key]) => alive(db(n), key)).map(([key, entry]) => keyNode(n, key, entry));
};

export const connectInfo = (): ConnectInfo => ({
  engine: ENGINE,
  server: { product: "Redis", version: VERSION, label: `Redis ${VERSION}`, flavor: "standalone" },
  capabilities: descriptor.capabilities,
  languages: descriptor.languages,
  connectionActions: [
    { id: "server", label: "Server info and slow log", icon: "server", target: { kind: "view", role: "server" }, danger: false, separatorBefore: false },
  ],
});

export const describe: NonNullable<EngineMock["describe"]> = (object): ObjectDescription => {
  const n = Number(object.database ?? 0);
  const entry = object.kind === "key" ? alive(db(n), object.name) : undefined;
  return {
    object,
    comment: null,
    columns: [],
    primaryKey: null,
    uniqueKeys: [],
    foreignKeys: [],
    referencedBy: [],
    indexes: [],
    rowEstimate: entry ? sizeOf(entry) : null,
    ddl: null,
    extra: entry
      ? [
          ["Type", entry.type],
          ["TTL", ttlOf(entry) < 0 ? "no expiry" : `${Math.floor(ttlOf(entry) / 1000)}s`],
        ]
      : [],
  };
};

// --- Operations -----------------------------------------------------------------------------------------------------------

function glob(pattern: string): RegExp {
  const source = pattern
    .replace(/[.+^${}()|\\]/g, "\\$&")
    .replace(/\*/g, ".*")
    .replace(/\?/g, ".");
  return new RegExp(`^${source}$`);
}

const memoryOf = (key: string, entry: Entry): number => 56 + key.length + sizeOf(entry) * (entry.type === "string" ? 1 : 48);

function scan(args: Record<string, unknown>) {
  const n = Number(args.db ?? 0);
  const pattern = typeof args.pattern === "string" && args.pattern ? glob(args.pattern) : null;
  const typeFilter = typeof args.typeFilter === "string" ? args.typeFilter : "";
  const pageSize = Number(args.pageSize ?? 500);
  const start = Number(args.cursor || 0);
  const all = [...db(n).entries()].filter(([key, entry]) => alive(db(n), key) && (!pattern || pattern.test(key)) && (!typeFilter || entry.type === typeFilter));
  const page = all.slice(start, start + pageSize);
  const done = start + pageSize >= all.length;
  return {
    cursor: done ? "0" : String(start + pageSize),
    done,
    keys: page.map(([key, entry]) => ({
      key: blob(key),
      keyType: entry.type,
      ttlMs: ttlOf(entry),
      memoryBytes: args.withMemory ? memoryOf(key, entry) : null,
    })),
    calls: 1,
    dbSize: [...db(n).keys()].length,
  };
}

function keyInfo(n: number, key: string) {
  const entry = alive(db(n), key);
  return {
    key: blob(key),
    keyType: entry ? entry.type : "none",
    ttlMs: ttlOf(entry),
    memoryBytes: entry ? memoryOf(key, entry) : null,
    encoding: entry ? (entry.type === "string" ? "embstr" : "listpack") : null,
    length: entry ? sizeOf(entry) : null,
  };
}

function readValue(args: Record<string, unknown>) {
  const n = Number(args.db ?? 0);
  const key = text(args.key as { text?: string });
  const entry = alive(db(n), key);
  const info = keyInfo(n, key);
  if (!entry) return { info, value: { type: "none" } };
  const count = Number(args.count ?? 100);
  const value = entry.value;
  switch (entry.type) {
    case "string": {
      const raw = value as string;
      let json: unknown = null;
      try {
        const parsed: unknown = JSON.parse(raw);
        if (typeof parsed === "object" && parsed !== null) json = parsed;
      } catch {
        json = null;
      }
      return {
        info,
        value: { type: "string", data: { value: blob(raw), length: raw.length, truncated: false, format: json === null ? "text" : "json", json } },
      };
    }
    case "hash": {
      const pattern = typeof args.pattern === "string" && args.pattern ? glob(args.pattern) : null;
      const items = [...(value as Map<string, string>).entries()].filter(([field]) => !pattern || pattern.test(field));
      const start = Number(args.cursor || 0);
      const page = items.slice(start, start + count);
      const done = start + count >= items.length;
      return {
        info,
        value: {
          type: "hash",
          data: {
            items: page.map(([field, v]) => ({ field: blob(field), value: blob(v) })),
            cursor: done ? "0" : String(start + count),
            done,
            total: (value as Map<string, string>).size,
          },
        },
      };
    }
    case "set": {
      const pattern = typeof args.pattern === "string" && args.pattern ? glob(args.pattern) : null;
      const items = [...(value as Set<string>)].filter((member) => !pattern || pattern.test(member));
      const start = Number(args.cursor || 0);
      const done = start + count >= items.length;
      return {
        info,
        value: {
          type: "set",
          data: { items: items.slice(start, start + count).map(blob), cursor: done ? "0" : String(start + count), done, total: (value as Set<string>).size },
        },
      };
    }
    case "list": {
      const start = Number(args.start ?? 0);
      const list = value as string[];
      return { info, value: { type: "list", data: { start, items: list.slice(start, start + count).map(blob), total: list.length } } };
    }
    case "zset": {
      const range = (args.zset ?? { start: 0, rev: false }) as { start: number; count?: number; rev: boolean };
      const sorted = [...(value as Map<string, number>).entries()].sort((a, b) => (range.rev ? b[1] - a[1] : a[1] - b[1]));
      const size = range.count ?? count;
      return {
        info,
        value: {
          type: "zset",
          data: {
            items: sorted.slice(range.start, range.start + size).map(([member, score]) => ({ member: blob(member), score })),
            offset: range.start,
            total: sorted.length,
          },
        },
      };
    }
    case "stream": {
      const entries = value as { id: string; fields: [string, string][] }[];
      const request = (args.stream ?? {}) as { before?: string | null; count?: number };
      const newestFirst = [...entries].reverse();
      const from = request.before ? newestFirst.findIndex((item) => item.id === request.before) + 1 : 0;
      const size = request.count ?? 50;
      const page = newestFirst.slice(from, from + size);
      const more = from + size < newestFirst.length;
      return {
        info,
        value: {
          type: "stream",
          data: {
            page: {
              entries: page.map((item) => ({ id: item.id, fields: item.fields.map(([f, v]) => ({ field: blob(f), value: blob(v) })) })),
              next: more ? (page[page.length - 1]?.id ?? null) : null,
              length: entries.length,
            },
            info: {
              length: entries.length,
              firstId: entries[0]?.id ?? null,
              lastId: entries[entries.length - 1]?.id ?? null,
              groups: [{ name: "workers", consumers: 2, pending: 3, lastDeliveredId: entries[entries.length - 3]?.id ?? "0-0", lag: 2 }],
            },
          },
        },
      };
    }
  }
}

/** What the caller saw of the part of the key an edit changes, read now in the same shape (null fields mean missing). */
function observe(n: number, key: string, expect: Record<string, unknown>): Record<string, unknown> {
  const entry = alive(db(n), key);
  const value = entry?.value;
  switch (expect.kind) {
    case "string":
      return { kind: "string", value: entry && typeof value === "string" ? blob(value) : null };
    case "hash":
      return {
        kind: "hash",
        fields: (expect.fields as { field: { text: string } }[]).map((item) => ({
          field: item.field,
          value: value instanceof Map && value.has(item.field.text) ? blob(String(value.get(item.field.text))) : null,
        })),
      };
    case "listItem": {
      const index = Number(expect.index);
      return { kind: "listItem", index, value: Array.isArray(value) && value[index] !== undefined ? blob(value[index] as string) : null };
    }
    case "set":
      return {
        kind: "set",
        members: (expect.members as { member: { text: string } }[]).map((item) => ({
          member: item.member,
          present: value instanceof Set && value.has(item.member.text),
        })),
      };
    case "zset":
      return {
        kind: "zset",
        members: (expect.members as { member: { text: string } }[]).map((item) => ({
          member: item.member,
          score: value instanceof Map && value.has(item.member.text) ? Number(value.get(item.member.text)) : null,
        })),
      };
    default:
      return { kind: entry ? "exists" : "missing" };
  }
}

function conflict(key: string, expected: unknown, current: unknown) {
  return mockError("CONFLICT", `Nothing was saved: \`${key}\` was changed by someone else since you loaded it.`, {
    engine: ENGINE,
    key: blob(key),
    expected,
    current,
  });
}

function edit(args: Record<string, unknown>) {
  const n = Number(args.db ?? 0);
  const entries = db(n);
  const op = String(args.op);
  const keyText = args.key ? text(args.key as { text?: string }) : "";
  const expect = args.expect as Record<string, unknown> | undefined;
  if (expect) {
    const current = observe(n, keyText, expect);
    if (JSON.stringify(current) !== JSON.stringify({ ...expect })) throw conflict(keyText, expect, current);
  }
  const entry = (type: Kind, make: () => Entry["value"]): Entry => {
    const existing = alive(entries, keyText);
    if (existing && existing.type !== type) throw serverError("WRONGTYPE", "Operation against a key holding the wrong kind of value");
    const found = existing ?? { type, value: make(), expiresAt: null };
    entries.set(keyText, found);
    return found;
  };
  type Bag = { field?: { text?: string }; member?: { text?: string }; value?: { text?: string } };
  const list = <T = Bag>(items: unknown): T[] => (Array.isArray(items) ? (items as T[]) : []);
  switch (op) {
    case "setString": {
      const existing = alive(entries, keyText);
      if ((args.condition === "ifMissing" && existing) || (args.condition === "ifExists" && !existing)) return { count: null, applied: false };
      entries.set(keyText, {
        type: "string",
        value: text(args.value as { text?: string }),
        expiresAt: args.keepTtl && existing ? existing.expiresAt : args.ttlMs ? Date.now() + Number(args.ttlMs) : null,
      });
      return { count: null, applied: true };
    }
    case "expire": {
      const existing = alive(entries, keyText);
      if (!existing) return { count: 0, applied: false };
      existing.expiresAt = Date.now() + Number(args.ttlMs);
      return { count: 1, applied: true };
    }
    case "persist": {
      const existing = alive(entries, keyText);
      if (!existing || existing.expiresAt === null) return { count: 0, applied: false };
      existing.expiresAt = null;
      return { count: 1, applied: true };
    }
    case "delete": {
      let count = 0;
      for (const key of list<{ text?: string }>(args.keys)) if (entries.delete(key.text ?? "")) count += 1;
      return { count, applied: true };
    }
    case "hashSet": {
      const target = entry("hash", () => new Map()).value as Map<string, string>;
      for (const item of list(args.fields)) target.set(text(item.field), text(item.value));
      return { count: null, applied: true };
    }
    case "hashDelete": {
      const target = alive(entries, keyText)?.value as Map<string, string> | undefined;
      for (const field of list<{ text?: string }>(args.fields)) target?.delete(field.text ?? "");
      if (target?.size === 0) entries.delete(keyText);
      return { count: null, applied: true };
    }
    case "listPush": {
      const target = entry("list", () => []).value as string[];
      const values = list<{ text?: string }>(args.values).map((item) => item.text ?? "");
      if (args.end === "head") target.unshift(...values.reverse());
      else target.push(...values);
      return { count: target.length, applied: true };
    }
    case "listSet": {
      const target = alive(entries, keyText)?.value as string[] | undefined;
      const index = Number(args.index);
      if (!target || index >= target.length) throw serverError("ERR", "index out of range");
      target[index] = text(args.value as { text?: string });
      return { count: null, applied: true };
    }
    case "listRemove": {
      const target = alive(entries, keyText)?.value as string[] | undefined;
      const at = target?.indexOf(text(args.value as { text?: string })) ?? -1;
      if (target && at >= 0) target.splice(at, 1);
      return { count: at >= 0 ? 1 : 0, applied: true };
    }
    case "setAdd": {
      const target = entry("set", () => new Set()).value as Set<string>;
      for (const member of list<{ text?: string }>(args.members)) target.add(member.text ?? "");
      return { count: null, applied: true };
    }
    case "setRemove": {
      const target = alive(entries, keyText)?.value as Set<string> | undefined;
      for (const member of list<{ text?: string }>(args.members)) target?.delete(member.text ?? "");
      return { count: null, applied: true };
    }
    case "zsetAdd": {
      const target = entry("zset", () => new Map()).value as Map<string, number>;
      for (const item of list<{ member?: { text?: string }; score?: number }>(args.members)) target.set(text(item.member), Number(item.score));
      return { count: null, applied: true };
    }
    case "zsetRemove": {
      const target = alive(entries, keyText)?.value as Map<string, number> | undefined;
      for (const member of list<{ text?: string }>(args.members)) target?.delete(member.text ?? "");
      return { count: null, applied: true };
    }
    case "streamAdd": {
      const target = entry("stream", () => []).value as { id: string; fields: [string, string][] }[];
      const id = typeof args.id === "string" ? args.id : `${Date.now()}-0`;
      target.push({ id, fields: list(args.fields).map((item) => [text(item.field), text(item.value)] as [string, string]) });
      return { count: null, applied: true };
    }
    default:
      throw serverError("ERR", `unknown edit ${op}`);
  }
}

const SLOWLOG = [
  { id: 14, timestamp: Math.floor(NOW / 1000) - 120, durationUs: 48_200, command: ["KEYS", "*"], client: "127.0.0.1:50321", clientName: "cli" },
  {
    id: 13,
    timestamp: Math.floor(NOW / 1000) - 600,
    durationUs: 15_900,
    command: ["SORT", "queue:jobs", "ALPHA"],
    client: "127.0.0.1:50400",
    clientName: null,
  },
  {
    id: 12,
    timestamp: Math.floor(NOW / 1000) - 3600,
    durationUs: 11_300,
    command: ["ZUNIONSTORE", "tmp", "2", "leaderboard", "leaderboard"],
    client: "127.0.0.1:50411",
    clientName: null,
  },
];

function info() {
  const keyspace = [...store.entries()]
    .map(([n, entries]) => ({
      db: n,
      keys: entries.size,
      expires: [...entries.values()].filter((entry) => entry.expiresAt !== null).length,
      avgTtlMs: 1_800_000,
    }))
    .filter((item) => item.keys > 0);
  const sections = {
    server: { redis_version: VERSION, redis_mode: "standalone", os: "Linux 6.8 x86_64", tcp_port: "6379", uptime_in_seconds: "93784", process_id: "1" },
    clients: { connected_clients: "3", blocked_clients: "0" },
    memory: { used_memory: "1874512", used_memory_human: "1.79M", used_memory_peak: "2201344", maxmemory: "268435456", maxmemory_policy: "noeviction" },
    stats: { total_commands_processed: "182044", keyspace_hits: "9120", keyspace_misses: "311", instantaneous_ops_per_sec: "4" },
    replication: { role: "master", connected_slaves: "0" },
    keyspace: Object.fromEntries(keyspace.map((item) => [`db${item.db}`, `keys=${item.keys},expires=${item.expires},avg_ttl=${item.avgTtlMs}`])),
  };
  return {
    version: VERSION,
    flavor: "redis",
    mode: "standalone",
    role: "master",
    os: "Linux 6.8 x86_64",
    uptimeSeconds: 93_784,
    connectedClients: 3,
    usedMemory: 1_874_512,
    usedMemoryPeak: 2_201_344,
    maxmemory: 268_435_456,
    maxmemoryPolicy: "noeviction",
    totalCommandsProcessed: 182_044,
    keyspaceHits: 9120,
    keyspaceMisses: 311,
    databases: DATABASES,
    currentDb: 0,
    keyspace,
    modules: [{ name: "ReJSON", version: 20_800 }],
    sections,
  };
}

export const invoke: NonNullable<EngineMock["invoke"]> = (name, raw) => {
  const args = (raw ?? {}) as Record<string, unknown>;
  switch (name) {
    case "redis.info":
      return info();
    case "redis.slowlog":
      return { length: SLOWLOG.length, entries: SLOWLOG.slice(0, Number(args.count ?? 128)), slowerThanUs: "10000", maxLen: "128" };
    case "redis.scan":
      return scan(args);
    case "redis.keyInfo":
      return keyInfo(Number(args.db ?? 0), text(args.key as { text?: string }));
    case "redis.readValue":
      return readValue(args);
    case "redis.edit":
      return edit(args);
    default:
      return undefined;
  }
};

/** Mock-only: "someone else" changes a key. `table` is `db0`, `key` the key name; `remove` deletes it. */
export const touchRow = (table: string, key: unknown, remove: boolean): string => {
  const n = Number(table.replace(/^db/, "")) || 0;
  const name = String(key);
  const entry = alive(db(n), name);
  if (!entry) return `${name} does not exist in db${n}`;
  if (remove) {
    db(n).delete(name);
    return `Deleted ${name} in db${n}`;
  }
  const value = entry.value;
  if (typeof value === "string") entry.value = `${value} (edited elsewhere)`;
  else if (value instanceof Map) value.set([...value.keys()][0] ?? "field", "edited elsewhere" as never);
  else if (Array.isArray(value)) (value as string[]).push("edited-elsewhere");
  else if (value instanceof Set) value.add("edited-elsewhere");
  return `Changed ${name} in db${n}`;
};

// --- Console --------------------------------------------------------------------------------------------------------------

function tokens(line: string): string[] {
  return [...line.matchAll(/"((?:[^"\\]|\\.)*)"|'([^']*)'|(\S+)/g)].map((match) => (match[1] ?? match[2] ?? match[3]).replace(/\\(["\\])/g, "$1"));
}

function lines(textValue: string): { number: number; line: string }[] {
  return textValue
    .split("\n")
    .map((line, index) => ({ number: index + 1, line: line.trim() }))
    .filter(({ line }) => line && !line.startsWith("#"));
}

const READ = new Set([
  "GET",
  "MGET",
  "STRLEN",
  "EXISTS",
  "TYPE",
  "TTL",
  "PTTL",
  "SCAN",
  "HGETALL",
  "HGET",
  "HLEN",
  "LRANGE",
  "LLEN",
  "SMEMBERS",
  "SCARD",
  "ZRANGE",
  "ZCARD",
  "XLEN",
  "DBSIZE",
  "PING",
  "INFO",
  "ECHO",
  "SELECT",
  "TIME",
  "RANDOMKEY",
]);
const WRITE = new Set([
  "SET",
  "DEL",
  "UNLINK",
  "EXPIRE",
  "PEXPIRE",
  "PERSIST",
  "HSET",
  "HDEL",
  "LPUSH",
  "RPUSH",
  "SADD",
  "SREM",
  "ZADD",
  "ZREM",
  "XADD",
  "INCR",
  "DECR",
  "APPEND",
  "RENAME",
]);
const DANGEROUS: Record<string, string> = {
  FLUSHALL: "Deletes every key in every database.",
  FLUSHDB: "Deletes every key in the selected database.",
  "CONFIG SET": "Changes the server configuration.",
  DEBUG: "Changes server state.",
  SHUTDOWN: "Changes server state.",
};

export function classify(source: string): StatementInfo[] {
  return lines(source).map(({ line }) => {
    const [first = "", second = ""] = tokens(line);
    const name = first.toUpperCase();
    const full = name === "CONFIG" ? `CONFIG ${second.toUpperCase()}` : name;
    const key = name === "KEYS" && (second === "" || /^\*+$/.test(second)) ? "KEYS *" : full;
    const category: StatementInfo["category"] =
      key === "FLUSHALL" || key === "FLUSHDB"
        ? "destructive"
        : key in DANGEROUS || key === "KEYS *"
          ? "admin"
          : READ.has(name) || key === "CONFIG GET" || name === "KEYS"
            ? "read"
            : WRITE.has(name)
              ? "write"
              : "unknown";
    return {
      category,
      kind: key,
      tables: [],
      excerpt: line.slice(0, 200),
      reason: DANGEROUS[key] ?? (key === "KEYS *" ? "KEYS * blocks the server while it walks every key; use SCAN." : null),
    };
  });
}

function column(name: string): ColumnInfo {
  return { name, typeName: "reply", kind: { kind: "other" }, nullable: true, default: null, comment: null, identity: null, generated: null, hints: {} };
}

export async function run(request: RunRequest, emit: (event: StreamEvent) => void, context: RunContext): Promise<number> {
  let selected = Number(request.database ?? context.session.database ?? 0) || 0;
  let index = 0;
  for (const { number, line } of lines(request.text)) {
    emit({ event: "statement", index, sql: line });
    await pause(40, context, () => mockError("CANCELLED", "The command was cancelled.", { engine: ENGINE }));
    const started = performance.now();
    const [first = "", ...rest] = tokens(line);
    const name = first.toUpperCase();
    const entries = db(selected);
    const fail = (code: string, message: string): never => {
      throw { ...serverError(code, message), position: { line: number }, statement: { index, text: line } };
    };
    const reply = (columns: string[], rows: Value[][]) => {
      emit({ event: "columns", index, columns: columns.map(column) });
      emit({ event: "rows", index, rows });
      emit({ event: "done", index, affected: null, rows: rows.length, truncated: false, elapsedMs: Math.max(1, Math.round(performance.now() - started)) });
    };
    const txt = (value: string): Value => ({ t: "text", v: value });
    const int = (value: number): Value => ({ t: "int", v: value });
    const key = rest[0] ?? "";
    switch (name) {
      case "PING":
        reply(["reply"], [[txt(rest[0] ?? "PONG")]]);
        break;
      case "SELECT":
        selected = Number(rest[0]);
        if (!Number.isInteger(selected) || selected < 0 || selected >= DATABASES) fail("ERR", "DB index is out of range");
        reply(["reply"], [[txt("OK")]]);
        break;
      case "DBSIZE":
        reply(["reply"], [[int(entries.size)]]);
        break;
      case "GET": {
        const entry = alive(entries, key);
        if (entry && entry.type !== "string") fail("WRONGTYPE", "Operation against a key holding the wrong kind of value");
        reply(["reply"], [[entry ? txt(entry.value as string) : { t: "null", v: null }]]);
        break;
      }
      case "SET":
        if (rest.length < 2) fail("ERR", "wrong number of arguments for 'set' command");
        entries.set(key, { type: "string", value: rest[1], expiresAt: null });
        reply(["reply"], [[txt("OK")]]);
        break;
      case "DEL":
      case "UNLINK":
        reply(["reply"], [[int(rest.filter((item) => entries.delete(item)).length)]]);
        break;
      case "TYPE":
        reply(["reply"], [[txt(alive(entries, key)?.type ?? "none")]]);
        break;
      case "TTL":
        reply(["reply"], [[int(Math.round(ttlOf(alive(entries, key)) / 1000))]]);
        break;
      case "HGETALL": {
        const entry = alive(entries, key);
        if (entry && entry.type !== "hash") fail("WRONGTYPE", "Operation against a key holding the wrong kind of value");
        reply(
          ["field", "value"],
          [...((entry?.value as Map<string, string> | undefined) ?? new Map()).entries()].map(([field, value]) => [txt(field), txt(value)]),
        );
        break;
      }
      case "SCAN":
      case "KEYS": {
        const matchAt = rest.findIndex((item) => item.toUpperCase() === "MATCH");
        const pattern = name === "KEYS" ? (rest[0] ?? "*") : matchAt >= 0 ? rest[matchAt + 1] : "*";
        const found = [...entries.keys()].filter((candidate) => alive(entries, candidate) && glob(pattern).test(candidate)).slice(0, 200);
        reply(
          ["#", "value"],
          found.map((item, position) => [int(position + 1), txt(item)]),
        );
        break;
      }
      case "INFO":
        reply(
          ["#", "value"],
          Object.entries(info().sections.server).map(([field, value], position) => [int(position + 1), txt(`${field}:${value}`)]),
        );
        break;
      case "FLUSHDB":
        entries.clear();
        reply(["reply"], [[txt("OK")]]);
        break;
      default:
        if (READ.has(name) || WRITE.has(name)) reply(["reply"], [[txt("OK (the dev mock runs a subset of commands)")]]);
        else fail("ERR", `unknown command '${first}', with args beginning with: ${rest.slice(0, 2).join(" ")}`);
    }
    index += 1;
  }
  return index;
}
