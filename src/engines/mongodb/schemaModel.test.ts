import { describe, expect, it } from "vitest";
import {
  compactResult,
  parents,
  pathFilters,
  readSchemaState,
  savedSchemaState,
  schemaSummary,
  visiblePaths,
  type SchemaPath,
  type SchemaResult,
} from "./schemaModel";

const path = (over: Partial<SchemaPath> & { path: string }): SchemaPath => ({
  name: over.path.slice(over.path.lastIndexOf(".") + 1),
  depth: over.path.split(".").length - 1,
  count: 10,
  percent: 100,
  types: [{ type: "string", count: 10 }],
  mixed: false,
  array: null,
  samples: [],
  distinct: null,
  topValues: null,
  min: null,
  max: null,
  reference: null,
  ...over,
});

const result = (paths: SchemaPath[]): SchemaResult => ({
  sample: { mode: "random", size: 1000, count: 10 },
  paths,
  truncated: false,
  depthCapped: false,
  references: { probed: true, collections: 3, note: null },
  elapsedMs: 5,
});

const target = { database: "shop", collection: "orders" };

describe("schema model", () => {
  const paths = [
    path({ path: "_id", types: [{ type: "objectId", count: 10 }] }),
    path({ path: "address" }),
    path({
      path: "address.city",
      topValues: [
        { value: "Oslo", count: 6 },
        { value: "Lisbon", count: 4 },
      ],
      distinct: 2,
    }),
    path({
      path: "age",
      percent: 80,
      mixed: true,
      types: [
        { type: "int", count: 6 },
        { type: "string", count: 2 },
        { type: "null", count: 1 },
      ],
      min: 18,
      max: 90,
    }),
    path({ path: "customerId", reference: { collection: "customers", matched: 5, probed: 5, inferred: true } }),
  ];

  it("shows the tree with collapsed parents hidden, or a flat search", () => {
    expect([...parents(paths)]).toEqual(["address"]);
    expect(visiblePaths(paths, new Set(["address"])).map((entry) => entry.path)).toEqual(["_id", "address", "age", "customerId"]);
    expect(visiblePaths(paths, new Set(["address"]), "CITY").map((entry) => entry.path)).toEqual(["address.city"]);
  });

  it("offers filters for presence, top values and each type of a mixed field", () => {
    expect(pathFilters(paths[2]).map((filter) => filter.filter)).toEqual([
      "{ 'address.city': { $exists: true } }",
      "{ 'address.city': { $exists: false } }",
      "{ 'address.city': 'Oslo' }",
      "{ 'address.city': 'Lisbon' }",
    ]);
    expect(
      pathFilters(paths[3])
        .map((filter) => filter.filter)
        .slice(2),
    ).toEqual(["{ 'age': { $type: 'int' } }", "{ 'age': { $type: 'string' } }"]);
    // A value cut by the analysis would not match: not offered.
    expect(pathFilters(path({ path: "note", topValues: [{ value: "long tex…", count: 2 }] }))).toHaveLength(2);
  });

  it("summarises paths and types for the assistant without values", () => {
    const text = schemaSummary(result(paths), target);
    expect(text).toContain("Schema of shop.orders, analysed on a random sample of 10 documents");
    expect(text).toContain("age: Int32|String (mixed), 80%, sometimes null");
    expect(text).toContain("customerId: String, 100%, likely references customers._id (inferred)");
    expect(text).not.toContain("Oslo");
    expect(text).not.toContain("90");
  });

  it("keeps the saved result compact and reads saved state defensively", () => {
    const many = Array.from({ length: 400 }, (_, i) =>
      path({ path: `field_${i}`, samples: ["x".repeat(50)], topValues: Array.from({ length: 10 }, (__, j) => ({ value: `value ${j}`, count: j })) }),
    );
    const compact = compactResult(result(many));
    expect(JSON.stringify(compact).length).toBeLessThanOrEqual(14_000);
    expect(compact.paths[0].samples).toEqual([]);
    expect(compact.truncated).toBe(true);
    const saved = savedSchemaState({ sampleMode: "filtered", sampleSize: 500, filter: "{ a: 1 }", result: result(paths), at: 1, collapsed: ["address"] });
    const read = readSchemaState(JSON.parse(JSON.stringify(saved)));
    expect(read.sampleMode).toBe("filtered");
    expect(read.result?.paths.map((entry) => entry.path)).toEqual(paths.map((entry) => entry.path));
    expect(read.result?.paths[4].reference?.collection).toBe("customers");
    expect(readSchemaState({ sampleMode: "nope", sampleSize: 1e9, result: { paths: "x" } })).toMatchObject({
      sampleMode: "random",
      sampleSize: 10_000,
      result: null,
    });
    expect(readSchemaState(null).result).toBeNull();
  });
});
