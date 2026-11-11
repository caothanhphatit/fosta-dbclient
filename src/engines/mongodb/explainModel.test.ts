import { describe, expect, it } from "vitest";
import {
  createIndexRequest,
  explainSummary,
  findExplainArgs,
  indexCovers,
  keyPatternFields,
  pipelineExplainArgs,
  planWarnings,
  queryShape,
  readExplainRequest,
  readExplainState,
  returnedShare,
  shapeOfArgs,
  suggestIndex,
  type ExplainResult,
  type NormalisedPlan,
  type PlanNode,
} from "./explainModel";
import type { IndexInfo } from "./indexModel";
import { EMPTY_QUERY } from "./queryModel";

const target = { database: "shop", collection: "orders" };

function index(name: string, key: Record<string, 1 | -1>, options: Record<string, unknown> = {}): IndexInfo {
  return { name, key, keys: Object.entries(key).map(([field, kind]) => ({ field, kind })), options, size: null, usage: null };
}

function plan(root: PlanNode, summary: Partial<NormalisedPlan["summary"]> = {}): NormalisedPlan {
  return {
    shape: "find",
    explainVersion: "1",
    winningPlan: root,
    rejectedPlans: [],
    summary: {
      nReturned: 20,
      keysExamined: 0,
      docsExamined: 20,
      executionTimeMillis: 1,
      isCollScan: false,
      usedIndexes: [],
      sortInMemory: false,
      fetchRatio: 1,
      ...summary,
    },
  };
}

const collscan: PlanNode = { stage: "COLLSCAN", nReturned: 20, totalDocsExamined: 5000, children: [] };

describe("explain arguments", () => {
  it("explains the Documents query as typed, with its page", () => {
    const built = findExplainArgs({ ...EMPTY_QUERY, filter: "{ status: 'paid' }", sort: "{ total: -1 }", skip: 40 }, target, "executionStats");
    expect(built.ok).toBe(true);
    if (!built.ok) return;
    expect(built.args).toMatchObject({ ...target, kind: "find", verbosity: "executionStats", skip: 40, limit: 20 });
    expect(JSON.parse(built.args.filter ?? "")).toEqual({ status: "paid" });
    expect(JSON.parse(built.args.sort ?? "")).toEqual({ total: -1 });
    expect(built.text).toBe("find({ status: 'paid' }).sort({ total: -1 }).skip(40).limit(20)");
    expect(findExplainArgs({ ...EMPTY_QUERY, filter: "{ status: " }, target, "queryPlanner").ok).toBe(false);
  });

  it("explains the enabled stages and refuses writes", () => {
    const built = pipelineExplainArgs(["{ $match: { status: 'paid' } }", "{ $group: { _id: '$customerId' } }"], target, "queryPlanner");
    expect(built.ok && built.args.pipeline).toEqual([{ $match: { status: "paid" } }, { $group: { _id: "$customerId" } }]);
    expect(built.ok && built.text).toBe("aggregate([$match, $group])");
    const writes = pipelineExplainArgs(["{ $match: {} }", "{ $out: 'copy' }"], target, "queryPlanner");
    expect(writes.ok === false && writes.error).toContain("$out");
    expect(pipelineExplainArgs([], target, "queryPlanner").ok).toBe(false);
    expect(pipelineExplainArgs(["{ $match: "], target, "queryPlanner").ok).toBe(false);
  });

  it("reads requests and saved state", () => {
    expect(readExplainRequest({ explain: { source: "pipeline", stages: ["{ $match: {} }", 3] } })).toEqual({
      source: "pipeline",
      query: undefined,
      stages: ["{ $match: {} }"],
    });
    expect(readExplainRequest({ explain: { source: "nope" } })).toBeNull();
    expect(readExplainRequest(null)).toBeNull();
    expect(readExplainState({ source: "pipeline", verbosity: "allPlansExecution" })).toEqual({ source: "pipeline", verbosity: "allPlansExecution" });
    expect(readExplainState({ source: 1, verbosity: "loud" })).toEqual({ source: "query", verbosity: "executionStats" });
  });
});

describe("query shape and the ESR suggestion", () => {
  it("splits equality, sort and range fields", () => {
    const shape = queryShape(
      {
        status: "paid",
        customerId: { $oid: "65f1a2b3c4d5e6f708192a3b" },
        total: { $gt: 100 },
        $and: [{ region: { $eq: "eu" } }, { tags: { $in: ["a"] } }],
        $expr: {},
      },
      { placedAt: -1 },
    );
    expect(shape.equality).toEqual(["status", "customerId", "region"]);
    expect(shape.range).toEqual(["total", "tags"]);
    expect(shape.sort).toEqual([{ field: "placedAt", direction: -1 }]);
    expect(shape.hasOr).toBe(false);
    expect(queryShape({ tags: { $in: ["a"] } }, null).equality).toEqual(["tags"]);
    expect(queryShape({ $or: [{ a: 1 }, { b: 2 }] }, null).hasOr).toBe(true);
  });

  it("takes a pipeline's leading $match and $sort", () => {
    const shape = shapeOfArgs({
      ...target,
      kind: "aggregate",
      verbosity: "queryPlanner",
      pipeline: [{ $match: { a: 1 } }, { $match: { b: { $lt: 2 } } }, { $sort: { c: 1 } }, { $match: { d: 1 } }],
    });
    expect(shape).toEqual({ equality: ["a"], sort: [{ field: "c", direction: 1 }], range: ["b"], hasOr: false });
  });

  it("suggests equality, then sort, then range keys", () => {
    const shape = queryShape({ status: "paid", total: { $gt: 100 } }, { placedAt: -1 });
    const suggestion = suggestIndex(shape, [index("_id_", { _id: 1 }), index("status_1", { status: 1 })]);
    expect(suggestion?.keys).toEqual([
      { field: "status", kind: 1 },
      { field: "placedAt", kind: -1 },
      { field: "total", kind: 1 },
    ]);
    expect(suggestion?.coveredBy).toBeNull();
    expect(suggestion?.reason).toContain("Equality, Sort, Range");
    expect(createIndexRequest(suggestion!)).toEqual({ createIndex: { keys: { status: 1, placedAt: -1, total: 1 }, options: {} } });
    expect(suggestIndex(queryShape({ $or: [{ a: 1 }] }, null), null)).toBeNull();
    expect(suggestIndex(queryShape({}, null), null)).toBeNull();
    expect(suggestIndex(queryShape({ _id: 5 }, null), null)?.coveredBy).toBe("_id_");
  });

  it("skips the suggestion when an index covers the shape", () => {
    const shape = queryShape({ status: "paid", region: "eu" }, { placedAt: 1 });
    // Equality fields in any order, the sort reversed as a whole, extra keys after.
    expect(indexCovers(index("x", { region: 1, status: 1, placedAt: -1, total: 1 }).keys, { ...shape })).toBe(true);
    expect(indexCovers(index("x", { status: 1, placedAt: 1 }).keys, shape)).toBe(false);
    expect(indexCovers(index("x", { placedAt: 1, status: 1, region: 1 }).keys, shape)).toBe(false);
    const covered = suggestIndex(shape, [index("status_1_region_1_placedAt_1", { status: 1, region: 1, placedAt: 1 })]);
    expect(covered?.coveredBy).toBe("status_1_region_1_placedAt_1");
    // Hidden and partial indexes do not count; a hidden one is pointed out.
    const hidden = suggestIndex(shape, [
      index("h", { status: 1, region: 1, placedAt: 1 }, { hidden: true }),
      index("p", { status: 1, region: 1, placedAt: 1 }, { partialFilterExpression: { a: 1 } }),
    ]);
    expect(hidden).toMatchObject({ coveredBy: null, hidden: "h" });
  });
});

describe("plan warnings", () => {
  const shape = queryShape({ status: "paid" }, { placedAt: -1 });

  it("flags scans, in-memory sorts, high ratios and unindexed fields", () => {
    const warnings = planWarnings(
      plan({ stage: "SORT", children: [collscan] }, { isCollScan: true, sortInMemory: true, docsExamined: 5000, fetchRatio: 250 }),
      shape,
    );
    expect(warnings.map((warning) => [warning.id, warning.tone])).toEqual([
      ["collscan", "warn"],
      ["sort", "warn"],
      ["ratio", "warn"],
      ["unindexed", "warn"],
    ]);
    expect(warnings.find((warning) => warning.id === "unindexed")?.detail).toContain("status");
  });

  it("is calm for a good plan and for a scan without a filter", () => {
    const good = plan(
      {
        stage: "FETCH",
        nReturned: 20,
        totalDocsExamined: 20,
        children: [{ stage: "IXSCAN", indexName: "status_1", keyPattern: { status: 1, placedAt: -1 }, totalKeysExamined: 20, children: [] }],
      },
      { usedIndexes: ["status_1"] },
    );
    expect(planWarnings(good, shape)).toEqual([]);
    const scan = planWarnings(plan(collscan, { isCollScan: true, fetchRatio: 1 }), queryShape({}, null));
    expect(scan).toEqual([expect.objectContaining({ id: "collscan", tone: "info" })]);
    // Few documents examined: no ratio warning.
    expect(planWarnings(plan(collscan, { docsExamined: 50, nReturned: 1, fetchRatio: 50 }), null).map((warning) => warning.id)).toEqual([]);
    expect(planWarnings(null, shape)[0].id).toBe("unknown");
  });

  it("reads key patterns given as text", () => {
    expect(keyPatternFields("{ _id: 1 }")).toEqual(["_id"]);
    expect(keyPatternFields({ "a.b": 1, c: -1 })).toEqual(["a.b", "c"]);
    expect(keyPatternFields(undefined)).toEqual([]);
  });

  it("measures what a stage returned against what it examined", () => {
    expect(returnedShare({ stage: "FETCH", nReturned: 20, totalDocsExamined: 200, children: [] })).toBe(0.1);
    expect(returnedShare({ stage: "IXSCAN", nReturned: 0, totalKeysExamined: 0, children: [] })).toBe(1);
    expect(returnedShare({ stage: "SORT", nReturned: 5, children: [] })).toBeNull();
  });

  it("sums the plan up for the assistant", () => {
    const result: ExplainResult = {
      explain: {},
      plan: plan({ stage: "SORT", children: [collscan] }, { isCollScan: true, sortInMemory: true }),
      verbosity: "executionStats",
      statement: "",
      elapsedMs: 3,
    };
    const warnings = planWarnings(result.plan, shape);
    const text = explainSummary(result, "find({ status: 'paid' })", warnings, suggestIndex(shape, []));
    expect(text).toContain("SORT\n  COLLSCAN");
    expect(text).toContain("Warning: Collection scan.");
    expect(text).toContain("Suggested index: { status: 1, placedAt: -1 }");
    expect(text).not.toContain("paid'}\n");
  });
});
