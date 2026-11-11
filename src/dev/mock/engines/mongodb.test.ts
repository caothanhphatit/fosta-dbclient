import { describe, expect, it } from "vitest";
import type { RowChangeSet } from "../../../core/contract";
import { exactValue, UNSET_VALUE } from "../../../engines/mongodb/docEdits";
import { apply, classifyOperation, invoke } from "./mongodb";

const target = { database: "shop", collection: "customers" };
const object = { database: "shop", schema: null, name: "customers", kind: "collection" };
type Doc = Record<string, unknown>;
const call = (name: string, args: Doc) => invoke(name, { ...target, ...args }) as Doc;
const first = () => (call("mongo.find", { limit: 1 }).documents as Doc[])[0];

describe("dev mock: document edits and bulk operations", () => {
  it("applies dotted-path edits and conflicts on a changed path", () => {
    const loaded = first();
    const address = loaded.address as Doc;
    const set = (city: string, original: string): RowChangeSet => ({
      object,
      changes: [
        {
          op: "update",
          key: [{ column: "_id", value: exactValue(loaded._id) }],
          values: [
            { column: "address.city", value: exactValue(city) },
            { column: "address.zip", value: UNSET_VALUE },
            { column: "vip", value: exactValue(true) },
          ],
          original: [
            { column: "address.city", value: exactValue(original) },
            { column: "address.zip", value: exactValue(address.zip) },
            { column: "vip", value: exactValue(null) },
          ],
        },
      ],
    });
    expect(apply!(set("Bergen", String(address.city)))).toEqual({ inserted: 0, updated: 1, deleted: 0 });
    const now = first();
    expect(now.address).toEqual({ city: "Bergen" });
    expect(now.vip).toBe(true);
    expect(() => apply!(set("Oslo", "Somewhere else"))).toThrow(expect.objectContaining({ code: "CONFLICT" }));
  });

  it("previews, updates and deletes the matching documents", () => {
    const filter = '{"loyaltyPoints":{"$lt":100}}';
    const preview = call("mongo.updatePreview", { filter, update: '{"$set":{"tier":"basic"},"$inc":{"loyaltyPoints":10}}', sample: 2 });
    const matched = preview.matched as number;
    expect(matched).toBeGreaterThan(0);
    const sample = preview.sample as { before: Doc; after: Doc; changed: boolean }[];
    expect(sample).toHaveLength(2);
    expect(sample[0].after.tier).toBe("basic");
    expect(sample[0].after.loyaltyPoints).toBe((sample[0].before.loyaltyPoints as number) + 10);
    expect(() => call("mongo.updatePreview", { filter, update: '{"$addToSet":{"tags":"x"}}' })).toThrow();
    expect(() => call("mongo.updateMany", { filter, update: '{"tier":"basic"}' })).toThrow();
    expect(call("mongo.updateMany", { filter, update: '{"$set":{"tier":"basic"}}' })).toMatchObject({ matched, modified: matched });
    expect(call("mongo.count", { filter: '{"tier":"basic"}', estimated: false }).count).toBe(matched);
    expect(call("mongo.deleteMany", { filter: '{"tier":"basic"}' })).toMatchObject({ deleted: matched });
    expect(call("mongo.count", { filter: '{"tier":"basic"}', estimated: false }).count).toBe(0);
  });

  it("classifies the operations like the backend", () => {
    const category = (name: string, args: Doc) => classifyOperation!(name, { ...target, ...args })?.[0].category;
    expect(category("mongo.updatePreview", { update: "{}" })).toBe("read");
    expect(category("mongo.updateMany", { filter: '{"a":1}' })).toBe("write");
    expect(category("mongo.updateMany", { filter: "{}" })).toBe("destructive");
    expect(category("mongo.deleteMany", { filter: '{"a":1}' })).toBe("destructive");
    expect(category("mongo.find", {})).toBe("read");
    expect(classifyOperation!("mongo.nope", {})).toBeNull();
    expect(() => call("mongo.updateMany", { filter: '{"$where":"true"}', update: '{"$set":{"a":1}}' })).toThrow();
  });
});
