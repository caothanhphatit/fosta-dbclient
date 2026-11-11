import { describe, expect, it } from "vitest";
import {
  defaultIndexName,
  displayKeys,
  EMPTY_FORM,
  formArgs,
  formOfRequest,
  formShell,
  indexBadges,
  indexesSummary,
  indexShell,
  indexType,
  isUnused,
  readIndexesResult,
  readIndexesState,
  visibleIndexes,
  type IndexForm,
  type IndexInfo,
} from "./indexModel";

const target = { database: "shop", collection: "orders" };
const DAY = 86_400_000;
const now = Date.parse("2026-11-11T00:00:00Z");

function index(name: string, key: Record<string, unknown>, options: Record<string, unknown> = {}, extra: Partial<IndexInfo> = {}): IndexInfo {
  return {
    name,
    key,
    keys: Object.entries(key).map(([field, kind]) => ({ field, kind: kind as 1 })),
    options,
    size: 1000,
    usage: { ops: 5, since: "2026-10-01T00:00:00Z" },
    ...extra,
  };
}

const form = (patch: Partial<IndexForm>): IndexForm => ({ ...structuredClone(EMPTY_FORM), ...patch });

describe("indexes model", () => {
  it("reads results defensively", () => {
    const result = readIndexesResult({
      indexes: [{ name: "a_1", key: { a: { $numberInt: "-1" } }, options: { unique: true }, size: 10, usage: { ops: 2, since: "x" } }, { nope: 1 }],
      totalSize: 10,
      stats: { sizes: true, usage: false, note: "Usage is not available" },
      elapsedMs: 4,
    });
    expect(result?.indexes).toHaveLength(1);
    expect(result?.indexes[0].keys).toEqual([{ field: "a", kind: -1 }]);
    expect(result?.stats).toEqual({ sizes: true, usage: false, note: "Usage is not available" });
    expect(readIndexesResult(null)).toBeNull();
  });

  it("shows text keys by their fields and names the type", () => {
    const text = index("t", { _fts: "text", _ftsx: 1, owner: 1 }, { weights: { title: 3, body: 1 } });
    expect(displayKeys(text)).toEqual([
      { field: "title", kind: "text" },
      { field: "body", kind: "text" },
      { field: "owner", kind: 1 },
    ]);
    expect(indexType(text)).toBe("text");
    expect(indexType(index("w", { "meta.$**": 1 }))).toBe("wildcard");
    expect(indexType(index("c", { a: 1, b: -1 }))).toBe("compound");
    expect(indexType(index("s", { a: 1 }))).toBe("single");
  });

  it("badges the properties", () => {
    const badges = indexBadges(
      index(
        "x",
        { at: 1 },
        { unique: true, sparse: true, partialFilterExpression: { s: "paid" }, expireAfterSeconds: 86_400, collation: { locale: "fr" }, hidden: true },
      ),
    ).map((badge) => badge.label);
    expect(badges).toEqual(["unique", "sparse", "partial", "TTL 1 d", "collation fr", "hidden"]);
    expect(indexBadges(index("_id_", { _id: 1 })).map((badge) => badge.id)).toEqual(["id"]);
  });

  it("hints unused indexes after a week without operations", () => {
    const idle = (since: string, ops = 0) => index("a_1", { a: 1 }, {}, { usage: { ops, since } });
    expect(isUnused(idle(new Date(now - 8 * DAY).toISOString()), now)).toBe(true);
    expect(isUnused(idle(new Date(now - 2 * DAY).toISOString()), now)).toBe(false);
    expect(isUnused(idle(new Date(now - 30 * DAY).toISOString(), 3), now)).toBe(false);
    expect(isUnused(index("_id_", { _id: 1 }, {}, { usage: { ops: 0, since: "2020-01-01T00:00:00Z" } }), now)).toBe(false);
    expect(isUnused(index("a_1", { a: 1 }, {}, { usage: null }), now)).toBe(false);
  });

  it("sorts and searches the list, _id_ first by name", () => {
    const list = [
      index("b_1", { b: 1 }, {}, { size: 10, usage: { ops: 9, since: null } }),
      index("_id_", { _id: 1 }, {}, { size: 50 }),
      index("a_1", { status: 1 }, {}, { size: 30, usage: null }),
    ];
    const names = (state: Parameters<typeof visibleIndexes>[1]) => visibleIndexes(list, state).map((entry) => entry.name);
    expect(names({ sort: "name", descending: false, search: "" })).toEqual(["_id_", "a_1", "b_1"]);
    expect(names({ sort: "size", descending: true, search: "" })).toEqual(["_id_", "a_1", "b_1"]);
    expect(names({ sort: "usage", descending: true, search: "" })).toEqual(["b_1", "_id_", "a_1"]);
    expect(names({ sort: "name", descending: false, search: "stat" })).toEqual(["a_1"]);
    expect(readIndexesState({ sort: "bogus", descending: true, search: 3 })).toEqual({ sort: "name", descending: true, search: "" });
  });

  it("copies an index as mongosh createIndex", () => {
    const partial = index("status_1", { status: 1, "items.sku": -1 }, { unique: true, partialFilterExpression: { status: "paid" }, textIndexVersion: 3 });
    expect(indexShell("orders", partial)).toBe(
      "db.orders.createIndex({ status: 1, 'items.sku': -1 }, { name: 'status_1', unique: true, partialFilterExpression: { status: 'paid' } })",
    );
    expect(indexShell("my-orders", index("t", { _fts: "text", _ftsx: 1 }, { weights: { title: 1 } }))).toBe(
      "db.getCollection('my-orders').createIndex({ title: 'text' }, { name: 't', weights: { title: 1 } })",
    );
  });

  it("builds the createIndex arguments from the form", () => {
    const built = formArgs(
      form({
        keys: [
          { field: "status", kind: 1 },
          { field: " placedAt ", kind: -1 },
          { field: "", kind: 1 },
        ],
        unique: true,
        partial: "status: { $in: ['paid', 'shipped'] }",
        collation: "{ locale: 'en', strength: 2 }",
      }),
      target,
    );
    expect(built).toEqual({
      ok: true,
      args: {
        ...target,
        keys: '{"status":1,"placedAt":-1}',
        unique: true,
        partialFilterExpression: '{"status":{"$in":["paid","shipped"]}}',
        collation: '{"locale":"en","strength":2}',
      },
    });
    const ttl = formArgs(form({ keys: [{ field: "at", kind: 1 }], ttl: "3600", name: " expire " }), target);
    expect(ttl.ok && ttl.args).toMatchObject({ name: "expire", expireAfterSeconds: 3600 });
  });

  it("points at the field that is wrong", () => {
    const field = (patch: Partial<IndexForm>) => {
      const built = formArgs(form(patch), target);
      return built.ok ? null : built.field;
    };
    expect(field({ keys: [{ field: "", kind: 1 }] })).toBe("keys");
    expect(field({ keys: [{ field: "$bad", kind: 1 }] })).toBe("keys");
    expect(
      field({
        keys: [
          { field: "a", kind: 1 },
          { field: "a", kind: -1 },
        ],
      }),
    ).toBe("keys");
    expect(field({ keys: [{ field: "$**", kind: "text" }] })).toBe("keys");
    expect(field({ keys: [{ field: "a", kind: 1 }], partial: "{ broken" })).toBe("partial");
    expect(field({ keys: [{ field: "a", kind: 1 }], ttl: "-5" })).toBe("ttl");
    expect(
      field({
        keys: [
          { field: "a", kind: 1 },
          { field: "b", kind: 1 },
        ],
        ttl: "60",
      }),
    ).toBe("ttl");
    expect(field({ keys: [{ field: "a", kind: 1 }], wildcardProjection: "{ a: 1 }" })).toBe("wildcardProjection");
    expect(field({ keys: [{ field: "$**", kind: 1 }], wildcardProjection: "{ a: 1 }" })).toBeNull();
  });

  it("previews the mongosh command live, with the default name", () => {
    expect(
      defaultIndexName([
        { field: "a", kind: 1 },
        { field: "b", kind: -1 },
      ]),
    ).toBe("a_1_b_-1");
    expect(formShell(form({ keys: [{ field: "a", kind: 1 }], unique: true, partial: "{ s: 'x' }" }), "orders")).toBe(
      "db.orders.createIndex({ a: 1 }, { name: 'a_1', unique: true, partialFilterExpression: { s: 'x' } })",
    );
    // Text that does not parse yet is shown as typed.
    expect(formShell(form({ keys: [{ field: "t", kind: "text" }], partial: "s: ", defaultLanguage: "spanish" }), "orders")).toBe(
      "db.orders.createIndex({ t: 'text' }, { name: 't_text', partialFilterExpression: { s: }, default_language: 'spanish' })",
    );
  });

  it("opens the form from another section's request", () => {
    const prefilled = formOfRequest({
      createIndex: { keys: { status: 1, placedAt: -1 }, options: { unique: true, partialFilterExpression: { status: "paid" } } },
    });
    expect(prefilled?.keys).toEqual([
      { field: "status", kind: 1 },
      { field: "placedAt", kind: -1 },
    ]);
    expect(prefilled?.unique).toBe(true);
    expect(prefilled?.partial).toBe("{ status: 'paid' }");
    expect(formOfRequest({ createIndex: { keys: [{ field: "loc", kind: "2dsphere" }] } })?.keys).toEqual([{ field: "loc", kind: "2dsphere" }]);
    expect(formOfRequest({ createIndex: { keys: {} } })).toBeNull();
    expect(formOfRequest({ filter: "x" })).toBeNull();
  });

  it("summarises the indexes for the assistant", () => {
    const summary = indexesSummary(
      {
        indexes: [index("_id_", { _id: 1 }), index("s_1", { s: 1 }, { partialFilterExpression: { s: "paid" } })],
        totalSize: 2000,
        stats: { sizes: true, usage: true, note: null },
        elapsedMs: 1,
      },
      target,
    );
    expect(summary).toContain("Indexes of shop.orders (2):");
    expect(summary).toContain("- s_1: { s: 1 }; partial { s: 'paid' }; 1000 B; 5 ops since 2026-10-01");
  });
});
