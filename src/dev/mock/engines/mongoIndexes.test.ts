import { describe, expect, it } from "vitest";
import { classifyOperation, invoke } from "./mongodb";

type Doc = Record<string, unknown>;
const target = { database: "shop", collection: "customers" };
const call = (name: string, args: Doc = {}) => invoke(name, { ...target, ...args }) as Doc;
const names = () => (call("mongo.indexes").indexes as Doc[]).map((index) => index.name);

describe("dev mock: index operations", () => {
  it("lists, creates, hides and drops indexes", () => {
    expect(names()).toEqual(["_id_"]);
    const listed = call("mongo.indexes");
    expect((listed.stats as Doc).usage).toBe(true);
    expect(typeof listed.totalSize).toBe("number");

    const created = call("mongo.createIndex", { keys: '{"email":1,"createdAt":-1}', unique: true, partialFilterExpression: '{"email":{"$exists":true}}' });
    expect(created).toMatchObject({ name: "email_1_createdAt_-1", created: true });
    expect(call("mongo.createIndex", { keys: '{"email":1,"createdAt":-1}', unique: true, partialFilterExpression: '{"email":{"$exists":true}}' }).created).toBe(
      false,
    );
    const index = (call("mongo.indexes").indexes as Doc[]).find((entry) => entry.name === "email_1_createdAt_-1") as Doc;
    expect(index.keys).toEqual([
      { field: "email", kind: 1 },
      { field: "createdAt", kind: -1 },
    ]);
    expect(index.options).toEqual({ unique: true, partialFilterExpression: { email: { $exists: true } } });

    expect(call("mongo.hideIndex", { name: "email_1_createdAt_-1" })).toMatchObject({ hidden: true, changed: true });
    expect(call("mongo.hideIndex", { name: "email_1_createdAt_-1" }).changed).toBe(false);
    expect(call("mongo.unhideIndex", { name: "email_1_createdAt_-1" })).toMatchObject({ hidden: false, changed: true });

    expect(() => call("mongo.dropIndex", { name: "_id_" })).toThrow(/cannot be dropped/);
    expect(() => call("mongo.hideIndex", { name: "_id_" })).toThrow(/cannot be hidden/);
    expect(call("mongo.dropIndex", { name: "email_1_createdAt_-1" }).statement).toBe('db.getCollection("customers").dropIndex("email_1_createdAt_-1")');
    expect(names()).toEqual(["_id_"]);
  });

  it("refuses bad keys and server-side JavaScript", () => {
    expect(() => call("mongo.createIndex", { keys: '{"a":2}' })).toThrow(/must be 1, -1/);
    expect(() => call("mongo.createIndex", { keys: "{}" })).toThrow(/at least one key/);
    expect(() => call("mongo.createIndex", { keys: '{"$a":1}' })).toThrow(/not a field path/);
    expect(() => call("mongo.createIndex", { keys: '{"a":1}', partialFilterExpression: '{"$where":"true"}' })).toThrow(/JavaScript/);
  });

  it("classifies like classify.rs", () => {
    const category = (name: string, args: Doc = {}) => classifyOperation!(name, { ...target, ...args })?.[0].category;
    expect(category("mongo.indexes")).toBe("read");
    expect(category("mongo.createIndex", { keys: { a: 1 } })).toBe("ddl");
    expect(category("mongo.hideIndex", { name: "a_1" })).toBe("ddl");
    expect(category("mongo.unhideIndex", { name: "a_1" })).toBe("ddl");
    expect(category("mongo.dropIndex", { name: "a_1" })).toBe("destructive");
  });

  it("seeds demo indexes with options and usage", () => {
    const orders = invoke("mongo.indexes", { database: "shop", collection: "orders" }) as Doc;
    const list = orders.indexes as Doc[];
    expect(list.map((index) => index.name)).toContain("status_1");
    expect((list.find((index) => index.name === "placedAt_1")?.options as Doc).hidden).toBe(true);
  });
});
