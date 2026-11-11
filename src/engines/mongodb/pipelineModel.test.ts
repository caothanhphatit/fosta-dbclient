import { describe, expect, it } from "vitest";
import { EMPTY_QUERY } from "./queryModel";
import {
  duplicateStage,
  enabledPipeline,
  insertStage,
  lookupField,
  MAX_SAVED_PIPELINE_CHARS,
  moveStage,
  moveStageBy,
  newStage,
  operatorOfText,
  parseStage,
  pipelineSummary,
  previewPlan,
  readAggregationsState,
  removeStage,
  sampledFields,
  savedStateOf,
  stagesFromQuery,
  stagesOfTexts,
  templateOf,
  textForOperator,
  toggleStage,
  type PipelineStage,
} from "./pipelineModel";

const stage = (id: string, text: string, enabled = true): PipelineStage => ({
  id,
  operator: operatorOfText(text) ?? "$match",
  text,
  enabled,
  collapsed: false,
});
const ids = (stages: PipelineStage[]) => stages.map((item) => item.id).join(",");

describe("pipeline edits", () => {
  const list = [stage("a", "{ $match: {} }"), stage("b", "{ $sort: { x: 1 } }"), stage("c", "{ $limit: 5 }")];

  it("insert, duplicate and remove", () => {
    expect(ids(insertStage(list, stage("d", "{ $skip: 1 }"), 1))).toBe("a,d,b,c");
    expect(ids(insertStage(list, stage("d", "{ $skip: 1 }")))).toBe("a,b,c,d");
    const copied = duplicateStage(list, "b", "b2");
    expect(ids(copied)).toBe("a,b,b2,c");
    expect(copied[2].text).toBe(list[1].text);
    expect(ids(removeStage(list, "b"))).toBe("a,c");
  });

  it("moves by position and by step, clamped", () => {
    expect(ids(moveStage(list, "a", 2))).toBe("b,c,a");
    expect(ids(moveStage(list, "c", 0))).toBe("c,a,b");
    expect(ids(moveStageBy(list, "a", -1))).toBe("a,b,c");
    expect(ids(moveStageBy(list, "b", 1))).toBe("a,c,b");
    expect(ids(moveStageBy(list, "c", 5))).toBe("a,b,c");
  });

  it("toggles", () => {
    expect(toggleStage(list, "b").map((item) => item.enabled)).toEqual([true, false, true]);
  });

  it("changes the operator: templates are replaced, typed bodies kept", () => {
    const fresh = newStage("x", "$match");
    expect(textForOperator(fresh, "$sort")).toBe(templateOf("$sort"));
    expect(textForOperator(stage("y", "{ $set: { a: 1 } }"), "$addFields")).toBe("{ $addFields: { a: 1 } }");
    expect(textForOperator(stage("z", "  "), "$limit")).toBe(templateOf("$limit"));
    expect(textForOperator(stage("o", "{ $out: 'copy' }"), "$limit")).toBe(templateOf("$limit"));
    expect(textForOperator(stage("l", "{ $limit: 5 }"), "$skip")).toBe("{ $skip: 5 }");
  });

  it("reads the operator of typed text", () => {
    expect(operatorOfText("{\n  $match: {}")).toBe("$match");
    expect(operatorOfText('{ "$group": {}')).toBe("$group");
    expect(operatorOfText("{ // note\n $sort: {}")).toBe("$sort");
    expect(operatorOfText("{ match: 1 }")).toBeNull();
  });
});

describe("parseStage", () => {
  it("parses one-operator documents to Extended JSON", () => {
    const parsed = parseStage("{ $match: { _id: ObjectId('65a1b2c3d4e5f60718293a4b') } }");
    expect(parsed).toMatchObject({ ok: true, operator: "$match" });
    if (parsed.ok) expect(parsed.json).toBe('{"$match":{"_id":{"$oid":"65a1b2c3d4e5f60718293a4b"}}}');
  });

  it("refuses empty, several operators and non-operators", () => {
    expect(parseStage("")).toMatchObject({ ok: false });
    expect(parseStage("{}")).toMatchObject({ ok: false, error: expect.stringContaining("empty") });
    expect(parseStage("{ $match: {}, $limit: 1 }")).toMatchObject({ ok: false, error: expect.stringContaining("one operator") });
    expect(parseStage("{ match: {} }")).toMatchObject({ ok: false, error: expect.stringContaining("not a stage operator"), position: 2 });
    expect(parseStage("{ $match: { a: } }")).toMatchObject({ ok: false });
  });
});

describe("previewPlan and enabledPipeline", () => {
  it("skips disabled stages and maps the others to preview positions", () => {
    const plan = previewPlan([stage("a", "{ $match: {} }"), stage("b", "{ $skip: 1 }", false), stage("c", "{ $limit: 2 }")]);
    expect(plan.pipeline).toEqual([{ $match: {} }, { $limit: 2 }]);
    expect(plan.entries).toEqual([{ kind: "run", previewIndex: 0 }, { kind: "disabled" }, { kind: "run", previewIndex: 1 }]);
  });

  it("stops at a stage that does not parse and blocks the ones after it", () => {
    const plan = previewPlan([stage("a", "{ $match: {} }"), stage("b", "{ $sort: { x: } }"), stage("c", "{ $limit: 2 }")]);
    expect(plan.pipeline).toHaveLength(1);
    expect(plan.entries[1].kind).toBe("invalid");
    expect(plan.entries[2]).toEqual({ kind: "blocked", by: 1 });
    expect(plan.firstInvalid).toBe(1);
  });

  it("never previews $out / $merge", () => {
    const plan = previewPlan([stage("a", "{ $match: {} }"), stage("b", '{ $out: "copy" }')]);
    expect(plan.pipeline).toHaveLength(1);
    expect(plan.entries[1]).toEqual({ kind: "writes", operator: "$out" });
  });

  it("runs the enabled stages with writes, or names the first that does not parse", () => {
    expect(enabledPipeline([stage("a", "{ $match: {} }"), stage("b", '{ $out: "copy" }')])).toMatchObject({
      ok: true,
      pipeline: [{ $match: {} }, { $out: "copy" }],
    });
    expect(enabledPipeline([stage("a", "{ $match: {} }", false), stage("b", "{ bad }")])).toMatchObject({ ok: false, index: 1 });
  });
});

describe("stagesFromQuery", () => {
  it("turns the bar's fields into stages in find order", () => {
    const stages = stagesFromQuery({ ...EMPTY_QUERY, filter: "status: 'paid'", sort: "{ total: -1 }", project: "{ total: 1 }", skip: 5, limit: 50 });
    expect(stages).toEqual(["{ $match: {status: 'paid'} }", "{ $sort: { total: -1 } }", "{ $skip: 5 }", "{ $limit: 50 }", "{ $project: { total: 1 } }"]);
    for (const text of stages) expect(parseStage(text).ok).toBe(true);
  });

  it("leaves out empty fields and the default page size", () => {
    expect(stagesFromQuery(EMPTY_QUERY)).toEqual([]);
    expect(stagesFromQuery({ ...EMPTY_QUERY, filter: "{}" })).toEqual([]);
  });
});

describe("saved state", () => {
  it("round-trips and takes defaults", () => {
    const stages = [stage("a", "{ $match: {} }", false)];
    const saved = savedStateOf(stages, { sampleSize: 500, sampleMode: "random", autoPreview: false, active: 0 });
    const read = readAggregationsState(JSON.parse(JSON.stringify(saved)));
    expect(read).toEqual({
      stages: [{ text: "{ $match: {} }", enabled: false, collapsed: false }],
      sampleSize: 500,
      sampleMode: "random",
      autoPreview: false,
      active: 0,
    });
    expect(readAggregationsState(null)).toEqual({ stages: [], sampleSize: 1000, sampleMode: "first", autoPreview: true, active: 0 });
    expect(readAggregationsState({ sampleSize: 99_999, stages: [{ nope: 1 }] }).sampleSize).toBe(10_000);
  });

  it("leaves out a pipeline over the bound", () => {
    const big = [stage("a", `{ $match: { a: "${"x".repeat(MAX_SAVED_PIPELINE_CHARS)}" } }`)];
    expect(savedStateOf(big, { sampleSize: 1, sampleMode: "first", autoPreview: true, active: 0 }).stages).toEqual([]);
  });

  it("makes stages of saved texts", () => {
    let n = 0;
    const stages = stagesOfTexts(["{ $group: { _id: 1 } }", { text: "{ $limit: 1 }", enabled: false, collapsed: true }], () => `s${++n}`);
    expect(stages.map((item) => [item.id, item.operator, item.enabled, item.collapsed])).toEqual([
      ["s1", "$group", true, false],
      ["s2", "$limit", false, true],
    ]);
  });
});

describe("sampled fields and summaries", () => {
  it("lists paths with their types, through arrays", () => {
    const fields = sampledFields([
      { _id: { $oid: "65a1b2c3d4e5f60718293a4b" }, total: 5, items: [{ sku: "a" }] },
      { _id: { $oid: "65a1b2c3d4e5f60718293a4c" }, total: "n/a" },
    ]);
    expect(fields).toEqual([
      { path: "_id", types: ["ObjectId"] },
      { path: "total", types: ["Number", "String"] },
      { path: "items", types: ["Array"] },
      { path: "items.sku", types: ["String"] },
    ]);
  });

  it("finds the joined field of a lookup", () => {
    expect(lookupField({ $lookup: { from: "x", as: "joined" } })).toBe("joined");
    expect(lookupField({ $match: {} })).toBeNull();
  });

  it("summarises a pipeline", () => {
    expect(pipelineSummary(["{ $match: {} }", "{ $group: {} }"])).toBe("$match → $group (2 stages)");
    expect(pipelineSummary([])).toBe("empty pipeline");
  });
});
