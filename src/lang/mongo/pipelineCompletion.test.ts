import { describe, expect, it } from "vitest";
import { cursor } from "../testSchema";
import type { CompletionSchema } from "../types";
import { stageCompletions, stageSchema } from "./pipelineCompletion";

const SCHEMA: CompletionSchema = {
  databases: ["shop"],
  schemas: [],
  tables: [
    {
      name: "orders",
      kind: "collection",
      columns: [
        { name: "_id", type: "objectId", pk: true },
        { name: "status", type: "string", pk: false },
        { name: "customerId", type: "objectId", pk: false },
      ],
    },
    {
      name: "customers",
      kind: "collection",
      columns: [
        { name: "_id", type: "objectId", pk: true },
        { name: "email", type: "string", pk: false },
      ],
    },
  ],
};

const complete = (text: string, fields: string[] = []) => {
  const { doc, pos } = cursor(text);
  return stageCompletions("orders", SCHEMA, fields, doc, pos, true);
};
const labels = (text: string, fields: string[] = []) => complete(text, fields)?.options.map((option) => option.label) ?? [];

describe("stage completion", () => {
  it("offers stage operators at the top of a stage", () => {
    expect(labels("{ $|")).toEqual(expect.arrayContaining(["$match", "$group", "$lookup"]));
    expect(complete("{ $ma|")?.from).toBe(2);
  });

  it("offers the fields reaching the stage, not the collection's", () => {
    const fields = ["_id", "count", "total"];
    expect(labels("{ $match: { |", fields)).toEqual(expect.arrayContaining(["count", "total", "$and"]));
    expect(labels("{ $match: { |", fields)).not.toContain("status");
    expect(labels('{ $sortByCount: "$|', fields)).toEqual(["_id", "count", "total"]);
    expect(labels("{ $match: { |")).toContain("status");
  });

  it("offers accumulators inside $group and expression operators in $project", () => {
    expect(labels("{ $group: { _id: '$status', n: { |")).toEqual(expect.arrayContaining(["$sum", "$avg", "$push"]));
    expect(labels("{ $project: { |")).toEqual(expect.arrayContaining(["$concat", "status"]));
  });

  it("offers collections where a stage names one", () => {
    expect(labels('{ $lookup: { from: "|')).toEqual(["orders", "customers"]);
    expect(labels("{ $unionWith: 'cu|")).toEqual(["orders", "customers"]);
    expect(complete("{ $unionWith: 'cu|")?.from).toBe(15);
    expect(labels('{ $out: "|')).toEqual(["orders", "customers"]);
    expect(labels('{ $merge: { into: "|')).toEqual(["orders", "customers"]);
  });

  it("offers input fields for localField and the joined collection's for foreignField", () => {
    expect(labels('{ $lookup: { from: "customers", localField: "|', ["customerId", "total"])).toEqual(["customerId", "total"]);
    expect(labels('{ $lookup: { from: "customers", localField: "customerId", foreignField: "|')).toEqual(["_id", "email"]);
  });

  it("closes the quote unless it is there", () => {
    const { doc, pos } = cursor('{ $out: "|" }');
    expect(stageCompletions("orders", SCHEMA, [], doc, pos, true)?.options[0].apply).toBe("orders");
    expect(complete('{ $out: "|')?.options[0].apply).toBe('orders"');
  });

  it("replaces only the collection's fields in the schema", () => {
    const schema = stageSchema(SCHEMA, "orders", ["a"]);
    expect(schema.tables.map((table) => [table.name, table.columns.map((column) => column.name)])).toEqual([
      ["orders", ["a"]],
      ["customers", ["_id", "email"]],
    ]);
    expect(stageSchema(SCHEMA, "orders", [])).toBe(SCHEMA);
  });
});
