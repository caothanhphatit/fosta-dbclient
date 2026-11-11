import { describe, expect, it } from "vitest";
import type { FenceTarget } from "../../core/assistantFences";
import type { ObjectRef } from "../../core/contract";
import { filterFenceActions, pipelineFenceActions, pipelineWarning } from "./assistantFences";

function target(object: ObjectRef | null) {
  const opened: { section: string; request: unknown }[] = [];
  const fence: FenceTarget = { connectionId: "c1", database: "shop", object, openWorkspace: (_object, section, request) => opened.push({ section, request }) };
  return { fence, opened };
}

const orders: ObjectRef = { database: "shop", schema: null, name: "orders", kind: "collection" };

describe("applying the assistant's fences", () => {
  it("fills the query bar of the collection with a filter, without running it", async () => {
    const { fence, opened } = target(orders);
    const [apply] = filterFenceActions("{ status: 'paid' }", fence);
    expect(apply.label).toBe("Apply to query bar");
    expect(await apply.run()).toBeNull();
    expect(opened).toEqual([{ section: "documents", request: { assistant: { filter: "{ status: 'paid' }" } } }]);
  });

  it("shows the parse error instead of applying", async () => {
    const { fence, opened } = target(orders);
    expect(await filterFenceActions("{ status: }", fence)[0].run()).toMatch(/^Line 1, column/);
    expect(await pipelineFenceActions("[ { status: 1 } ]", fence)[0].run()).toMatch(/^Stage 1: /);
    expect(opened).toEqual([]);
  });

  it("needs a collection workspace", async () => {
    const { fence, opened } = target({ database: "shop", schema: null, name: "orders", kind: "table" });
    expect(await filterFenceActions("{}", fence)[0].run()).toContain("Open the collection first");
    expect(await pipelineFenceActions("[ { $match: {} } ]", target(null).fence)[1].run()).toContain("Open the collection first");
    expect(opened).toEqual([]);
  });

  it("opens, inserts or replaces stages in Aggregations", async () => {
    const { fence, opened } = target(orders);
    const [open, insert, replace] = pipelineFenceActions("[ { $match: { a: 1 } }, { $limit: 5 } ]", fence);
    expect([open.label, insert.label, replace.label]).toEqual(["Open in Aggregations", "Insert as stage(s)", "Replace stage"]);
    await open.run();
    await insert.run();
    expect(await replace.run()).toContain("one stage");
    expect(await pipelineFenceActions("{ $limit: 5 }", fence)[2].run()).toBeNull();
    expect(opened).toEqual([
      { section: "aggregations", request: { assistant: { pipeline: ["{ $match: { a: 1 } }", "{ $limit: 5 }"], mode: "replace" } } },
      { section: "aggregations", request: { assistant: { pipeline: ["{ $match: { a: 1 } }", "{ $limit: 5 }"], mode: "insert" } } },
      { section: "aggregations", request: { assistant: { pipeline: ["{ $limit: 5 }"], mode: "replaceActive" } } },
    ]);
    expect(open.warning).toBeUndefined();
  });

  it("flags pipelines that write or name another database and asks before applying them", async () => {
    for (const code of [
      "[ { $match: {} }, { $out: 'orders' } ]",
      "[ { $merge: { into: 'totals' } } ]",
      "[ { $facet: { a: [ { $merge: 'x' } ] } } ]",
      "[ { $lookup: { from: { db: 'billing', coll: 'cards' }, localField: 'c', foreignField: '_id', as: 'c' } } ]",
      "{ $unionWith: { db: 'admin', coll: 'system.users' } }",
    ]) {
      const { fence, opened } = target(orders);
      const asked: string[] = [];
      let answer = false;
      const confirm = (message: string) => {
        asked.push(message);
        return Promise.resolve(answer);
      };
      const [open] = pipelineFenceActions(code, fence, confirm);
      expect(open.warning, code).toMatch(/^Check before applying: /);
      expect(open.title, code).toContain("Check before applying");
      // Declined: nothing is applied.
      expect(await open.run()).toBeNull();
      expect(opened, code).toEqual([]);
      // Confirmed: applied as a suggestion (still not run).
      answer = true;
      expect(await open.run()).toBeNull();
      expect(asked).toHaveLength(2);
      expect(opened).toHaveLength(1);
    }
  });

  it("does not flag field names that look like write stages, or the chat's own database", () => {
    const quiet = (code: string) => pipelineFenceActions(code, target(orders).fence)[0].warning;
    expect(quiet("[ { $project: { outcome: '$outcome', merge: 1 } } ]")).toBeUndefined();
    expect(quiet("[ { $lookup: { from: { db: 'shop', coll: 'customers' }, localField: 'c', foreignField: '_id', as: 'c' } } ]")).toBeUndefined();
    expect(quiet("[ { $match: { db: 'x' } } ]")).toBeUndefined();
    expect(pipelineWarning("[ { $out: 'x' }, { $unionWith: { db: 'other', coll: 'y' } } ]", target(orders).fence)).toContain("$out drops the target");
  });
});
