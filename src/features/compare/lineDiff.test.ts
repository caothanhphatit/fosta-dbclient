import { describe, expect, it } from "vitest";
import { diffLines, sideBySide, textLines, type DiffOp } from "./lineDiff";

/** Applies an edit script to `a`: the result must be `b`, and its equal lines must be from both. */
function apply(ops: DiffOp[]): { a: string[]; b: string[] } {
  const a: string[] = [];
  const b: string[] = [];
  for (const op of ops) {
    if (op.kind !== "insert") a.push(op.a);
    if (op.kind !== "delete") b.push(op.b);
  }
  return { a, b };
}

const edits = (ops: DiffOp[]) => ops.filter((op) => op.kind !== "equal").length;

describe("diffLines", () => {
  it("handles empty sides", () => {
    expect(diffLines([], [])).toEqual([]);
    expect(diffLines([], ["a", "b"])).toEqual([
      { kind: "insert", b: "a" },
      { kind: "insert", b: "b" },
    ]);
    expect(diffLines(["a"], [])).toEqual([{ kind: "delete", a: "a" }]);
  });

  it("keeps equal text as one run", () => {
    const ops = diffLines(["a", "b", "c"], ["a", "b", "c"]);
    expect(ops.every((op) => op.kind === "equal")).toBe(true);
    expect(ops).toHaveLength(3);
  });

  it("finds a shortest edit script", () => {
    // The classic Myers example: ABCABBA → CBABAC takes 5 edits.
    const a = "ABCABBA".split("");
    const b = "CBABAC".split("");
    const ops = diffLines(a, b);
    expect(apply(ops)).toEqual({ a, b });
    expect(edits(ops)).toBe(5);
  });

  it("reconstructs both sides of random inputs with no more edits than a full rewrite", () => {
    let seed = 7;
    const random = () => {
      seed = (seed * 1103515245 + 12345) % 2 ** 31;
      return seed / 2 ** 31;
    };
    for (let round = 0; round < 200; round += 1) {
      const a = Array.from({ length: Math.floor(random() * 12) }, () => "xyz"[Math.floor(random() * 3)]);
      const b = Array.from({ length: Math.floor(random() * 12) }, () => "xyz"[Math.floor(random() * 3)]);
      const ops = diffLines(a, b);
      expect(apply(ops)).toEqual({ a, b });
      expect(edits(ops)).toBeLessThanOrEqual(a.length + b.length);
    }
  });

  it("puts deletions before insertions in a hunk", () => {
    const ops = diffLines(["a", "old", "c"], ["a", "new", "c"]);
    expect(ops.map((op) => op.kind)).toEqual(["equal", "delete", "insert", "equal"]);
  });
});

describe("sideBySide", () => {
  it("pairs a changed line and numbers both sides", () => {
    const rows = sideBySide(
      ["CREATE TABLE t (", "    a integer,", "    b text", ");"],
      ["CREATE TABLE t (", "    a bigint,", "    b text,", "    c date", ");"],
    );
    expect(rows.map((row) => row.kind)).toEqual(["equal", "change", "change", "added", "equal"]);
    expect(rows[1]).toMatchObject({ left: "    a integer,", right: "    a bigint,", leftNumber: 2, rightNumber: 2 });
    expect(rows[3]).toMatchObject({ left: null, right: "    c date", leftNumber: null, rightNumber: 4 });
    expect(rows[4]).toMatchObject({ leftNumber: 4, rightNumber: 5 });
  });

  it("shows removals on the left only", () => {
    const rows = sideBySide(["a", "b", "c"], ["a", "c"]);
    expect(rows.map((row) => [row.kind, row.left, row.right])).toEqual([
      ["equal", "a", "a"],
      ["removed", "b", null],
      ["equal", "c", "c"],
    ]);
  });
});

describe("textLines", () => {
  it("splits on any line break and drops the trailing empty line", () => {
    expect(textLines("a\r\nb\nc\n")).toEqual(["a", "b", "c"]);
    expect(textLines(null)).toEqual([]);
    expect(textLines("")).toEqual([]);
  });
});
