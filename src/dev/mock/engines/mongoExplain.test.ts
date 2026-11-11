import { describe, expect, it } from "vitest";
import { classifyOperation, invoke } from "./mongodb";

type Doc = Record<string, unknown>;
const target = { database: "shop", collection: "orders" };
const explain = (args: Doc) => invoke("mongo.explain", { ...target, ...args }) as Doc;

describe("dev mock: explain", () => {
  it("plans a find on an index, with counters at executionStats", () => {
    const result = explain({ kind: "find", filter: '{"customerId":1}', limit: 5, verbosity: "executionStats" });
    const plan = result.plan as Doc;
    const summary = plan.summary as Doc;
    expect(summary.usedIndexes).toContain("customerId_1_placedAt_-1");
    expect(summary.isCollScan).toBe(false);
    expect(typeof summary.docsExamined).toBe("number");
    expect((result.explain as Doc).executionStats).toBeTruthy();
    expect(result.statement).toContain('.explain("executionStats").find(');
  });

  it("scans and sorts in memory without an index, counters only when run", () => {
    const result = explain({ kind: "find", filter: '{"note":"x"}', sort: '{"total":-1}' });
    const summary = (result.plan as Doc).summary as Doc;
    expect(summary).toMatchObject({ isCollScan: true, sortInMemory: true, nReturned: null, docsExamined: null });
    expect((result.explain as Doc).executionStats).toBeUndefined();
  });

  it("explains a pipeline with stages above the cursor and refuses writes", () => {
    const result = explain({ kind: "aggregate", pipeline: [{ $match: { status: "paid" } }, { $group: { _id: "$customerId" } }], verbosity: "executionStats" });
    const winning = (result.plan as Doc).winningPlan as Doc;
    expect(winning.stage).toBe("$group");
    expect(((result.explain as Doc).stages as Doc[])[0]).toHaveProperty("$cursor");
    expect(() => explain({ kind: "aggregate", pipeline: [{ $out: "copy" }] })).toThrow();
    expect(() => explain({ verbosity: "loud" })).toThrow();
    expect(classifyOperation("mongo.explain", { ...target, kind: "find" })?.[0].category).toBe("read");
  });
});
