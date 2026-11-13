import { describe, expect, it } from "vitest";
import {
  dedent,
  filterQuestion,
  filterQuestionShown,
  lineDiff,
  needsReview,
  parseFilterFence,
  parsePipelineFence,
  pipelineText,
  sampledDocumentsText,
  splitArray,
  stageQuestion,
  stageQuestionShown,
} from "./assistantModel";

describe("the mongo-filter fence", () => {
  it("takes one filter document in mongosh syntax", () => {
    expect(parseFilterFence("  { status: 'paid', _id: ObjectId('64b7f0c2a1b2c3d4e5f60718') };\n")).toEqual({
      ok: true,
      text: "{ status: 'paid', _id: ObjectId('64b7f0c2a1b2c3d4e5f60718') }",
    });
  });

  it("shows where a parse error is instead of applying", () => {
    const parsed = parseFilterFence("{\n  status: 'paid',,\n}");
    expect(parsed.ok).toBe(false);
    if (!parsed.ok) expect(parsed.error).toMatch(/^Line 2, column \d+: /);
    expect(parseFilterFence("[1, 2]").ok).toBe(false);
    expect(parseFilterFence("").ok).toBe(false);
    // Server-side JavaScript never reaches the bar.
    expect(parseFilterFence("{ $where: 'this.a > 1' }").ok).toBe(false);
  });
});

describe("the mongo-pipeline fence", () => {
  it("keeps each stage's text as written", () => {
    const code = `[
  { $match: { status: 'paid', note: /a,b]/i } },  // paid only, ] and , inside
  {
    $group: { _id: '$customerId', total: { $sum: '$total' } }
  }
]`;
    const parsed = parsePipelineFence(code);
    expect(parsed).toEqual({
      ok: true,
      stages: [
        "{ $match: { status: 'paid', note: /a,b]/i } } // paid only, ] and , inside",
        "{\n  $group: { _id: '$customerId', total: { $sum: '$total' } }\n}",
      ],
    });
  });

  it("takes a single stage document", () => {
    expect(parsePipelineFence("{ $limit: 5 }")).toEqual({ ok: true, stages: ["{ $limit: 5 }"] });
  });

  it("reports the stage that does not parse", () => {
    const parsed = parsePipelineFence("[ { $match: {} }, { status: 1 } ]");
    expect(parsed.ok).toBe(false);
    if (!parsed.ok) expect(parsed.error).toMatch(/^Stage 2: /);
    const broken = parsePipelineFence("[ { $match: { a: } } ]");
    expect(broken.ok).toBe(false);
    if (!broken.ok) expect(broken.error).toMatch(/^Line 1, column/);
    expect(parsePipelineFence("[]").ok).toBe(false);
    expect(parsePipelineFence("[ { $match: { $where: 'x' } } ]").ok).toBe(false);
  });

  it("splits arrays only at their own commas", () => {
    expect(splitArray(`[ { a: "x, ]" }, { b: [1, 2] } /* , */ ]`)).toEqual([`{ a: "x, ]" }`, "{ b: [1, 2] } /* , */"]);
    expect(splitArray("[ 1, 2, ]")).toEqual(["1", "2"]);
    expect(splitArray("[1] [2]")).toBeNull();
    expect(splitArray("{ a: 1 }")).toBeNull();
    expect(dedent("{\n      a: 1\n    }")).toBe("{\n  a: 1\n}");
  });
});

describe("the diff shown before replacing", () => {
  it("marks removed and added lines", () => {
    expect(lineDiff("{\n  a: 1\n}", "{\n  a: 2\n}")).toEqual([
      { kind: "same", text: "{" },
      { kind: "removed", text: "  a: 1" },
      { kind: "added", text: "  a: 2" },
      { kind: "same", text: "}" },
    ]);
    expect(lineDiff("", "x")).toEqual([{ kind: "added", text: "x" }]);
    expect(lineDiff("x", "x")).toEqual([{ kind: "same", text: "x" }]);
  });

  it("is only needed when something of the user's changes", () => {
    expect(needsReview("", "{ a: 1 }")).toBe(false);
    expect(needsReview("{}", "{ a: 1 }")).toBe(false);
    expect(needsReview("{ a: 1 }", " { a: 1 } ")).toBe(false);
    expect(needsReview("{ a: 1 }", "{ a: 2 }")).toBe(true);
    expect(pipelineText(["{ $match: {} }", "{ $limit: 1 }"])).toBe("// 1\n{ $match: {} }\n\n// 2\n{ $limit: 1 }");
  });
});

describe("the inline Ask", () => {
  it("asks for an answer in the fence the workspace applies", () => {
    expect(filterQuestion("orders", " paid orders over 100 ")).toContain("orders: paid orders over 100\n");
    expect(filterQuestion("orders", "x")).toContain("`mongo-filter` code block");
    expect(stageQuestion("next", 1, "$match", "orders")).toContain("after stage 2 ($match)");
    expect(stageQuestion("fix", 0, "$group", "orders")).toMatch(/^Stage 1 \(\$group\).*`mongo-pipeline` code block/s);
    expect(stageQuestion("explain", 0, "$group", "orders")).not.toContain("```");
  });

  it("shows the user only their own words, not the instructions to the model", () => {
    expect(filterQuestionShown("orders", " paid orders over 100 ")).toBe("Filter orders: paid orders over 100");
    expect(filterQuestionShown("orders", "x")).not.toContain("code block");
    expect(stageQuestionShown("explain", 0, "$group")).toBe("Explain stage 1 ($group)");
    expect(stageQuestionShown("next", 1, "$match")).toBe("Suggest the stage after stage 2 ($match)");
    expect(stageQuestionShown("fix", 2, "$project")).toBe("Fix stage 3 ($project)");
  });
});

describe("sampled documents for the assistant", () => {
  it("are few, cut per field and bounded", () => {
    const documents = Array.from({ length: 10 }, (_, index) => ({ _id: index, note: "x".repeat(1000), tags: Array.from({ length: 30 }, (_, n) => n) }));
    const text = sampledDocumentsText(documents, "Documents reaching stage 2 (sampled)") ?? "";
    const lines = text.split("\n");
    expect(lines[0]).toBe("Documents reaching stage 2 (sampled):");
    expect(lines).toHaveLength(4);
    expect(text).not.toContain("x".repeat(201));
    expect(text).toContain("20 more item(s)");
    expect(sampledDocumentsText([], "None")).toBeNull();
    const deep = { a: { b: { c: { d: { e: { f: { g: 1 } } } } } } };
    expect(sampledDocumentsText([deep], "Deep")).toContain('"f":"{… 1 field(s)}"');
  });
});
