import { describe, expect, it } from "vitest";
import type { SchemaPath, SchemaResult } from "./schemaModel";
import {
  applyShell,
  applyWarnings,
  failingFilter,
  generateSchema,
  idFilter,
  isDirty,
  parseValidator,
  readRules,
  readValidationState,
  rulesBrief,
  sameValidator,
  savedValidationState,
  schemaText,
  setArgs,
  validatorText,
} from "./validationModel";

function path(path: string, count: number, types: string[], extra: Partial<SchemaPath> = {}): SchemaPath {
  const name = path.slice(path.lastIndexOf(".") + 1);
  return {
    path,
    name,
    depth: path.split(".").length - 1,
    count,
    percent: count,
    types: types.map((type) => ({ type, count })),
    mixed: types.filter((type) => type !== "null").length > 1,
    array: null,
    samples: [],
    distinct: null,
    topValues: null,
    min: null,
    max: null,
    reference: null,
    ...extra,
  };
}

const result = (paths: SchemaPath[], count = 100): SchemaResult => ({
  sample: { mode: "random", size: 1000, count },
  paths,
  truncated: false,
  depthCapped: false,
  references: { probed: false, collections: 0, note: null },
  elapsedMs: 1,
});

const array = (elementTypes: string[]) => ({
  count: 100,
  minLength: 0,
  avgLength: 2,
  maxLength: 4,
  elementTypes: elementTypes.map((type) => ({ type, count: 1 })),
});

describe("generateSchema", () => {
  const schema = result([
    path("_id", 100, ["objectId"]),
    path("status", 100, ["string"], {
      distinct: 3,
      topValues: [
        { value: "paid", count: 50 },
        { value: "open", count: 30 },
        { value: "void", count: 20 },
      ],
    }),
    path("qty", 90, ["int", "double"]),
    path("note", 100, ["string", "null"], {
      distinct: 2,
      topValues: [
        { value: "a", count: 1 },
        { value: "b", count: 1 },
      ],
    }),
    path("address", 80, ["object"]),
    path("address.city", 80, ["string"]),
    path("address.zip", 40, ["string"]),
    path("items", 100, ["array"], { array: array(["object"]) }),
    path("items.sku", 100, ["string"]),
    path("items.qty", 95, ["int"]),
    path("tags", 60, ["array"], {
      array: array(["string"]),
      distinct: 2,
      topValues: [
        { value: "red", count: 3 },
        { value: "blue", count: 2 },
      ],
    }),
  ]);

  it("gives types, mixed types, nested objects and array items, required at 100%", () => {
    const generated = generateSchema(schema);
    expect(generated).toEqual({
      $jsonSchema: {
        bsonType: "object",
        required: ["_id", "status", "note", "items"],
        properties: {
          _id: { bsonType: "objectId" },
          status: { bsonType: "string" },
          qty: { bsonType: ["int", "double"] },
          note: { bsonType: ["string", "null"] },
          address: { bsonType: "object", required: ["city"], properties: { city: { bsonType: "string" }, zip: { bsonType: "string" } } },
          items: {
            bsonType: "array",
            items: { bsonType: "object", required: ["sku"], properties: { sku: { bsonType: "string" }, qty: { bsonType: "int" } } },
          },
          tags: { bsonType: "array", items: { bsonType: "string" } },
        },
      },
    });
  });

  it("lowers the required share and adds enums for small string sets", () => {
    const generated = generateSchema(schema, { requiredPercent: 90, enumMax: 3 }) as { $jsonSchema: Record<string, Record<string, Record<string, unknown>>> };
    const root = generated.$jsonSchema;
    expect(root.required).toEqual(["_id", "status", "qty", "note", "items"]);
    expect(root.properties.status.enum).toEqual(["open", "paid", "void"]);
    expect(root.properties.note.enum).toEqual(["a", "b", null]);
    expect(root.properties.tags.items).toEqual({ bsonType: "string", enum: ["blue", "red"] });
    expect((root.properties.items.items as Record<string, unknown>).required).toEqual(["sku", "qty"]);
    // Too many values: no enum.
    const fewer = generateSchema(schema, { requiredPercent: 100, enumMax: 2 }) as typeof generated;
    expect(fewer.$jsonSchema.properties.status.enum).toBeUndefined();
  });

  it("round-trips through the mongosh text", () => {
    const text = validatorText(generateSchema(schema));
    const parsed = parseValidator(text);
    expect(parsed.ok).toBe(true);
    if (parsed.ok) expect(JSON.parse(parsed.json)).toEqual(generateSchema(schema));
  });
});

describe("drafts", () => {
  const applied = { validator: "{ a: { $exists: true } }", level: "strict" as const, action: "error" as const };

  it("parses the text, empty meaning no validator", () => {
    expect(parseValidator("  ")).toEqual({ ok: true, json: "{}", empty: true });
    expect(parseValidator("{}")).toMatchObject({ ok: true, empty: true });
    expect(parseValidator("[1]").ok).toBe(false);
    expect(parseValidator("{ $where: 'true' }").ok).toBe(false);
    expect(sameValidator("{ a: { $exists: true } }", '{"a":{"$exists":true}}')).toBe(true);
  });

  it("sends only what changed", () => {
    const target = { database: "shop", collection: "orders" };
    expect(isDirty(applied, { ...applied, validator: "{a:{$exists:true}}" })).toBe(false);
    expect(setArgs(target, applied, { ...applied, level: "moderate" })).toEqual({ ...target, validationLevel: "moderate" });
    expect(setArgs(target, applied, { ...applied, validator: "" })).toEqual({ ...target, validator: "{}" });
    expect(setArgs(target, applied, { ...applied, validator: "{ broken" })).toMatchObject({ error: expect.any(String) });
    expect(applyShell("orders", { ...target, validator: '{"a":1}', validationAction: "warn" })).toBe(
      "db.runCommand({\n  collMod: 'orders',\n  validator: {\n    a: 1\n  },\n  validationAction: 'warn'\n})",
    );
  });

  it("warns about failing documents under strict rules and removed rules", () => {
    expect(applyWarnings(applied, { count: 3, capped: false })[0]).toMatch(/^3 existing documents fail.*Strict and Error/);
    expect(applyWarnings({ ...applied, level: "moderate" }, { count: 1, capped: false })[0]).toMatch(/document fails.*Moderate/);
    expect(applyWarnings({ ...applied, action: "warn" }, { count: 1, capped: true })[0]).toMatch(/^1\+ existing documents.*Warn/);
    expect(applyWarnings({ ...applied, validator: "" }, { count: 3, capped: false })).toEqual([
      "The validator is empty: applying removes the collection's validation rules.",
    ]);
    expect(applyWarnings(applied, { count: 0, capped: false })).toEqual([]);
  });

  it("builds Documents filters", () => {
    expect(failingFilter('{"a":{"$exists":true}}')).toBe("{ $nor: [ { a: { $exists: true } } ] }");
    expect(idFilter({ $oid: "65f1a2b3c4d5e6f708192a3b" })).toBe("{ _id: ObjectId('65f1a2b3c4d5e6f708192a3b') }");
    expect(idFilter(7)).toBe("{ _id: 7 }");
  });
});

describe("state and brief", () => {
  it("reads rules and saved state defensively", () => {
    expect(readRules({ validator: {}, validationLevel: "x" })).toMatchObject({ validator: null, validationLevel: "strict", validationAction: "error" });
    expect(readValidationState(null)).toEqual({ draft: null, generate: { requiredPercent: 100, enumMax: 0 } });
    expect(readValidationState({ draft: { validator: "{}", level: "off", action: "warn" }, generate: { requiredPercent: 150, enumMax: 99 } })).toEqual({
      draft: { validator: "{}", level: "off", action: "warn" },
      generate: { requiredPercent: 100, enumMax: 10 },
    });
    expect(savedValidationState({ validator: "x".repeat(9000), level: "strict", action: "error" }, { requiredPercent: 100, enumMax: 0 }).draft).toBeNull();
  });

  it("briefs the assistant", () => {
    const rules = readRules({ validator: { $jsonSchema: { required: ["a"] } }, validationLevel: "moderate", validationAction: "warn" });
    const brief = rulesBrief({ database: "shop", collection: "orders" }, rules, { validator: "{}", level: "strict", action: "error" }, null);
    expect(brief).toContain("level moderate, action warn");
    expect(brief).toContain("$jsonSchema");
    expect(brief).toContain("Unapplied draft (level strict, action error)");
    expect(rulesBrief({ database: "s", collection: "c" }, null, null, null)).toContain("not read yet");
  });
});

describe("schemaText", () => {
  it("spreads documents over lines and keeps short value lists on one", () => {
    expect(
      schemaText({
        $jsonSchema: { required: ["a", "b"], properties: { "a-b": { bsonType: ["int", "null"] }, at: { enum: [{ $oid: "65f1a2b3c4d5e6f708192a3b" }] } } },
      }),
    ).toBe(
      "{\n  $jsonSchema: {\n    required: ['a', 'b'],\n    properties: {\n      'a-b': {\n        bsonType: ['int', 'null']\n      },\n      at: {\n        enum: [\n          ObjectId('65f1a2b3c4d5e6f708192a3b')\n        ]\n      }\n    }\n  }\n}",
    );
  });
});
