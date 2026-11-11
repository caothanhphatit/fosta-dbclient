/**
 * The dev mock's index operations, like src-tauri/src/engines/mongo/indexes.rs: `mongo.indexes` (specifications with
 * options, made-up sizes and usage), `mongo.createIndex` (keys and options checked, the server's default name, an
 * identical index not created twice), `mongo.hideIndex` / `mongo.unhideIndex` and `mongo.dropIndex` (never `_id_`).
 * Indexes live per collection for the session, seeded with `_id_`, the catalog's indexes and a few demo ones.
 */
import { mockError } from "../errors";

type Json = unknown;
type Doc = Record<string, Json>;

interface MockIndex {
  name: string;
  key: Doc;
  options: Doc;
  ops: number;
  since: string;
}

const KEY_TYPES = ["text", "2dsphere", "2d", "hashed"];
const DAY = 86_400_000;
const isDoc = (value: Json): value is Doc => typeof value === "object" && value !== null && !Array.isArray(value);

/** Demo indexes beyond the catalog's, so the section shows partial, TTL, hidden, text and unused indexes. */
const DEMO: Record<string, Omit<MockIndex, "since">[]> = {
  "shop.orders": [
    { name: "customerId_1_placedAt_-1", key: { customerId: 1, placedAt: -1 }, options: {}, ops: 18_240 },
    { name: "status_1", key: { status: 1 }, options: { partialFilterExpression: { status: { $in: ["paid", "shipped"] } } }, ops: 0 },
    { name: "placedAt_1", key: { placedAt: 1 }, options: { hidden: true }, ops: 0 },
  ],
  "shop.products": [
    {
      name: "name_text",
      key: { _fts: "text", _ftsx: 1 },
      options: { weights: { name: 3, tags: 1 }, default_language: "english", textIndexVersion: 3 },
      ops: 412,
    },
  ],
  "events.pageviews": [{ name: "at_1", key: { at: 1 }, options: { expireAfterSeconds: 2_592_000 }, ops: 96 }],
};

const store = new Map<string, MockIndex[]>();

export interface IndexHost {
  /** The catalog's index definitions of a collection (`{ "sku": 1 }` texts), null when there is no such collection. */
  definitions(database: string, collection: string): { name: string; definition: string | null; unique: boolean; primary: boolean }[] | null;
  /** Documents in the collection, for made-up sizes. */
  count(database: string, collection: string): number;
}

function indexesOf(host: IndexHost, args: Doc): { list: MockIndex[]; collection: string; database: string } {
  const database = String(args.database ?? "");
  const collection = String(args.collection ?? "");
  const id = `${database}.${collection}`;
  let list = store.get(id);
  if (!list) {
    const definitions = host.definitions(database, collection);
    if (!definitions) throw mockError("ENGINE_ERROR", `ns does not exist: ${id}`);
    const since = new Date(Date.now() - 30 * DAY).toISOString();
    list = [{ name: "_id_", key: { _id: 1 }, options: {}, ops: 5230, since }];
    for (const index of definitions) {
      if (index.primary) continue;
      let key: Doc = {};
      try {
        key = JSON.parse(index.definition ?? "{}") as Doc;
      } catch {
        // Not a key document: skipped.
      }
      list.push({ name: index.name, key, options: index.unique ? { unique: true } : {}, ops: 1204, since });
    }
    for (const demo of DEMO[id] ?? []) list.push({ ...demo, since });
    store.set(id, list);
  }
  return { list, collection, database };
}

function parse(args: Doc, key: string): Json {
  const value = args[key];
  if (typeof value !== "string") return value ?? undefined;
  if (!value.trim()) return undefined;
  try {
    return JSON.parse(value) as Json;
  } catch (error) {
    throw mockError("ENGINE_ERROR", `${key} must be a JSON document (${error instanceof Error ? error.message : String(error)})`);
  }
}

function nameArg(args: Doc): string {
  const name = typeof args.name === "string" ? args.name.trim() : "";
  if (!name) throw mockError("ENGINE_ERROR", "name (the index name) is required");
  return name;
}

const quote = (text: string) => JSON.stringify(text);

export function listIndexes(host: IndexHost, args: Doc): Json {
  const { list, database, collection } = indexesOf(host, args);
  const documents = Math.max(1, host.count(database, collection));
  const indexes = list.map((index) => {
    const fields = Object.keys(index.key).length;
    return {
      name: index.name,
      key: index.key,
      keys: Object.entries(index.key).map(([field, kind]) => ({ field, kind: typeof kind === "number" ? Math.sign(kind) : kind })),
      options: index.options,
      size: 4096 + documents * (index.name === "_id_" ? 24 : 18 * fields),
      usage: { ops: index.ops, since: index.since },
    };
  });
  return {
    indexes,
    totalSize: indexes.reduce((sum, index) => sum + index.size, 0),
    stats: { sizes: true, usage: true, note: null },
    elapsedMs: 3,
  };
}

function checkedKeys(args: Doc): Doc {
  const keys = parse(args, "keys");
  if (!isDoc(keys)) throw mockError("ENGINE_ERROR", "keys is required");
  const entries = Object.entries(keys);
  if (entries.length === 0) throw mockError("ENGINE_ERROR", "An index needs at least one key");
  const checked: Doc = {};
  for (const [field, value] of entries) {
    const wildcard = field === "$**" || field.endsWith(".$**");
    const path = wildcard ? field.slice(0, -3).replace(/\.$/, "") : field;
    if ((!wildcard && !path) || (path && path.split(".").some((part) => !part || part.startsWith("$"))))
      throw mockError("ENGINE_ERROR", `'${field}' is not a field path`);
    if (value === 1 || value === -1) checked[field] = value;
    else if (typeof value === "string" && KEY_TYPES.includes(value) && !wildcard) checked[field] = value;
    else throw mockError("ENGINE_ERROR", `The key ${field} must be 1, -1${wildcard ? "" : ', "text", "2dsphere", "2d" or "hashed"'}`);
  }
  return checked;
}

export function createIndex(host: IndexHost, args: Doc): Json {
  const { list, collection } = indexesOf(host, args);
  const keys = checkedKeys(args);
  const options: Doc = {};
  for (const flag of ["unique", "sparse", "hidden"]) if (args[flag] === true) options[flag] = true;
  if (args.expireAfterSeconds !== undefined && args.expireAfterSeconds !== null) {
    const seconds = Number(args.expireAfterSeconds);
    if (!Number.isInteger(seconds) || seconds < 0) throw mockError("ENGINE_ERROR", "expireAfterSeconds must be a whole number of 0 or more");
    options.expireAfterSeconds = seconds;
  }
  for (const key of ["partialFilterExpression", "collation", "wildcardProjection", "weights"]) {
    const value = parse(args, key);
    if (value === undefined) continue;
    if (!isDoc(value)) throw mockError("ENGINE_ERROR", `${key} must be a JSON document`);
    if (Object.keys(value).length) options[key] = value;
  }
  if (typeof args.defaultLanguage === "string" && args.defaultLanguage.trim()) options.default_language = args.defaultLanguage.trim();
  const name =
    typeof args.name === "string" && args.name.trim()
      ? args.name.trim()
      : Object.entries(keys)
          .map(([field, kind]) => `${field}_${String(kind)}`)
          .join("_");
  const shown: Doc = { name, ...options };
  const statement = `db.getCollection(${quote(collection)}).createIndex(${JSON.stringify(keys)}, ${JSON.stringify(shown)})`;
  const text = Object.values(keys).includes("text");
  const stored: Doc = text ? { _fts: "text", _ftsx: 1 } : keys;
  if (text)
    options.weights = {
      ...Object.fromEntries(
        Object.entries(keys)
          .filter(([, kind]) => kind === "text")
          .map(([field]) => [field, 1]),
      ),
      ...(options.weights as Doc),
    };
  const same = list.find((index) => index.name === name);
  if (same) {
    if (JSON.stringify(same.key) === JSON.stringify(stored) && JSON.stringify(same.options) === JSON.stringify(options))
      return { name, created: false, statement, elapsedMs: 2 };
    throw mockError("ENGINE_ERROR", `An index with name '${name}' already exists with a different specification (IndexKeySpecsConflict)`);
  }
  if (list.some((index) => JSON.stringify(index.key) === JSON.stringify(stored) && JSON.stringify(index.options) === JSON.stringify(options)))
    return { name, created: false, statement, elapsedMs: 2 };
  list.push({ name, key: stored, options, ops: 0, since: new Date().toISOString() });
  return { name, created: true, statement, elapsedMs: 40 };
}

export function setHidden(host: IndexHost, args: Doc, hidden: boolean): Json {
  const { list, collection } = indexesOf(host, args);
  const name = nameArg(args);
  if (name === "_id_") throw mockError("ENGINE_ERROR", "The _id index cannot be hidden");
  const index = list.find((item) => item.name === name);
  if (!index) throw mockError("ENGINE_ERROR", `cannot find index ${name} (IndexNotFound)`);
  const changed = (index.options.hidden === true) !== hidden;
  if (hidden) index.options = { ...index.options, hidden: true };
  else {
    const { hidden: _drop, ...rest } = index.options;
    void _drop;
    index.options = rest;
  }
  return { name, hidden, changed, statement: `db.getCollection(${quote(collection)}).${hidden ? "hideIndex" : "unhideIndex"}(${quote(name)})` };
}

export function dropIndex(host: IndexHost, args: Doc): Json {
  const { list, collection } = indexesOf(host, args);
  const name = nameArg(args);
  if (name === "_id_" || name === "*")
    throw mockError("ENGINE_ERROR", name === "_id_" ? "The _id index cannot be dropped" : "Drop one index at a time, by name");
  const at = list.findIndex((item) => item.name === name);
  if (at < 0) throw mockError("ENGINE_ERROR", `index not found with name [${name}] (IndexNotFound)`);
  list.splice(at, 1);
  return { name, statement: `db.getCollection(${quote(collection)}).dropIndex(${quote(name)})`, elapsedMs: 4 };
}
