import { describe, expect, it } from "vitest";
import { classifyOperation, invoke } from "./mongodb";

type Doc = Record<string, unknown>;
const call = (name: string, args: Doc = {}, collection = "customers") => invoke(name, { database: "shop", collection, ...args }) as Doc;

describe("dev mock: validation operations", () => {
  it("reads, checks, applies and removes rules", () => {
    expect(call("mongo.validation")).toMatchObject({ validator: null, validationLevel: "strict", validationAction: "error", type: "collection" });
    const demo = call("mongo.validation", {}, "orders");
    expect(demo).toMatchObject({ validationLevel: "moderate", validationAction: "warn" });
    // Returned orders (one in four) are outside the demo enum.
    const failing = call("mongo.validationCheck", { validator: JSON.stringify(demo.validator), limit: 3 }, "orders");
    expect(failing).toMatchObject({ failing: 125, total: 500, note: null });
    expect((failing.samples as Doc[]).length).toBe(3);
    expect((failing.samples as Doc[])[0].status).toBe("returned");

    const schema = { $jsonSchema: { bsonType: "object", required: ["email"], properties: { email: { bsonType: "string", pattern: "^customer[0-9]+@" } } } };
    expect(call("mongo.validationCheck", { validator: schema }).failing).toBe(0);
    const strict = { $jsonSchema: { required: ["phone"], anyOf: [{}] } };
    const checked = call("mongo.validationCheck", { validator: strict });
    expect(checked.failing).toBe(checked.total);
    expect(checked.note).toContain("Not evaluated in mock: anyOf");

    call("mongo.setValidation", { validator: JSON.stringify(schema), validationAction: "warn" });
    expect(call("mongo.validation")).toMatchObject({ validator: schema, validationLevel: "strict", validationAction: "warn" });
    call("mongo.setValidation", { validator: "{}" });
    expect(call("mongo.validation").validator).toBeNull();
    expect(() => call("mongo.setValidation", { validationLevel: "lax" })).toThrow();
    expect(() => call("mongo.setValidation", {})).toThrow();
  });

  it("classifies and refuses server-side JavaScript", () => {
    expect(classifyOperation("mongo.setValidation", { database: "shop", collection: "c", validator: {} })?.[0].category).toBe("ddl");
    expect(classifyOperation("mongo.validationCheck", { database: "shop", collection: "c", validator: {} })?.[0].category).toBe("read");
    expect(() => call("mongo.validationCheck", { validator: '{"$expr":{"$function":{}}}' })).toThrow(/JavaScript/);
  });
});
