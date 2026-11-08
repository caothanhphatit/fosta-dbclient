import { describe, expect, it } from "vitest";
import type { ColumnInfo, StreamEvent, Value } from "../../core/contract";
import { describeResult, editorOffset, RunResults, statementExcerpt } from "./runResults";

const column = (name: string): ColumnInfo => ({
  name,
  typeName: "int",
  kind: { kind: "int" },
  nullable: false,
  default: null,
  comment: null,
  identity: null,
  generated: null,
  hints: {},
});
const int = (v: number): Value => ({ t: "int", v });
const done = (index: number, rows: number, affected: number | null = null, truncated = false): StreamEvent => ({
  event: "done",
  index,
  affected,
  rows,
  truncated,
  elapsedMs: 3,
});

function accumulate(events: StreamEvent[]): RunResults {
  const results = new RunResults();
  for (const event of events) results.apply(event);
  return results;
}

describe("run results", () => {
  it("collects a script's statements, rows in batches, and notices", () => {
    const results = accumulate([
      { event: "statement", index: 0, sql: "SELECT id FROM t" },
      { event: "columns", index: 0, columns: [column("id")] },
      { event: "rows", index: 0, rows: [[int(1)], [int(2)]] },
      { event: "rows", index: 0, rows: [[int(3)]] },
      done(0, 3),
      { event: "statement", index: 1, sql: "UPDATE t SET a = 1" },
      { event: "notice", index: 1, text: "3 rows matched" },
      done(1, 0, 3),
      { event: "statement", index: 2, sql: "USE shop" },
      done(2, 0),
    ]);
    expect(results.entries.map((entry) => entry.sql)).toEqual(["SELECT id FROM t", "UPDATE t SET a = 1", "USE shop"]);
    expect(results.resultSets()).toHaveLength(1);
    expect(results.resultSets()[0].rows).toEqual([[int(1)], [int(2)], [int(3)]]);
    expect(results.entries[1].notices).toEqual(["3 rows matched"]);
    expect(results.rowCount()).toBe(3);
    expect(results.history()).toEqual([
      { rowSet: true, rows: 3, affectedRows: 0, truncated: false },
      { rowSet: false, rows: 0, affectedRows: 3, truncated: false },
      { rowSet: false, rows: 0, affectedRows: 0, truncated: false },
    ]);
  });

  it("counts rows live before the statement is done", () => {
    const results = new RunResults();
    results.apply({ event: "statement", index: 0, sql: "SELECT 1" });
    results.apply({ event: "columns", index: 0, columns: [column("x")] });
    const entry = results.apply({ event: "rows", index: 0, rows: [[int(1)]] });
    expect(entry.done).toBeNull();
    expect(results.rowCount()).toBe(1);
    expect(describeResult(entry, 0)).toBe("#1: 1 row(s)");
  });

  it("gives a second result set of one statement its own entry", () => {
    const results = accumulate([
      { event: "statement", index: 0, sql: "EXEC two_sets" },
      { event: "columns", index: 0, columns: [column("a")] },
      { event: "rows", index: 0, rows: [[int(1)]] },
      { event: "columns", index: 0, columns: [column("b")] },
      { event: "rows", index: 0, rows: [[int(2)], [int(3)]] },
      done(0, 2),
    ]);
    const sets = results.resultSets();
    expect(sets.map((set) => set.columns?.[0].name)).toEqual(["a", "b"]);
    expect(sets.map((set) => set.rows.length)).toEqual([1, 2]);
    expect(sets[1].sql).toBe("EXEC two_sets");
    expect(sets[1].done?.rows).toBe(2);
  });

  it("accepts events without a statement event", () => {
    const results = accumulate([{ event: "notice", index: 0, text: "hello" }, done(0, 0, 1)]);
    expect(results.entries).toHaveLength(1);
    expect(results.entries[0].notices).toEqual(["hello"]);
    expect(describeResult(results.entries[0], 0)).toBe("#1: 1 row(s) affected · 3 ms");
  });

  it("describes truncated, plain and unfinished statements", () => {
    const truncated = accumulate([{ event: "columns", index: 0, columns: [column("x")] }, done(0, 1000, null, true)]).entries[0];
    expect(describeResult(truncated, 0)).toMatch(/^#1: 1,000 row\(s\) \(truncated: only the first 1,000 were read\) · 3 ms$/);
    expect(describeResult(accumulate([done(0, 0)]).entries[0], 1)).toBe("#2: OK · 3 ms");
    const unfinished = accumulate([{ event: "statement", index: 0, sql: "X" }]).entries[0];
    expect(describeResult(unfinished, 0)).toBe("#1: running…");
    expect(describeResult(unfinished, 0, "failed")).toBe("#1: failed");
  });

  it("cuts statement excerpts to their first line", () => {
    expect(statementExcerpt("  SELECT *\nFROM t")).toBe("SELECT *");
    expect(statementExcerpt("x".repeat(80), 10)).toBe(`${"x".repeat(9)}…`);
  });
});

describe("error positions in the editor", () => {
  it("adds the start of the selection that ran", () => {
    const text = "SELECT 1;\nSELEC 2";
    expect(editorOffset(text, 0, { line: 2, column: 1 })).toBe(10);
    expect(editorOffset(text, 25, { line: 2, column: 3 })).toBe(37);
    expect(editorOffset(text, 25, { offset: 4 })).toBe(29);
  });

  it("ignores positions outside the text", () => {
    expect(editorOffset("SELECT 1", 5, { line: 3, column: 1 })).toBeNull();
    expect(editorOffset("SELECT 1", 5, { offset: 99 })).toBeNull();
    expect(editorOffset("SELECT 1", 5, {})).toBeNull();
  });
});
