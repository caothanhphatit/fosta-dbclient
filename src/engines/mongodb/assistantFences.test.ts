import { describe, expect, it } from "vitest";
import type { FenceTarget } from "../../core/assistantFences";
import type { ObjectRef } from "../../core/contract";
import { filterFenceActions, pipelineFenceActions } from "./assistantFences";

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
  });
});
