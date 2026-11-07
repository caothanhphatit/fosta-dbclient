import { describe, expect, it } from "vitest";
import { firstLine, formatDuration, historyStatus, relativeTime, resultSummary, summarizeSets } from "./historyFormat";
import type { ColumnMeta, HistoryResult } from "../../core/types";

const column: ColumnMeta = { name: "id", typeName: "int", numeric: true, binary: false, primary: true, vectorDims: null };
const rows = (count: number, truncated = false): HistoryResult => ({ rowSet: true, rows: count, affectedRows: 0, truncated });
const affected = (count: number): HistoryResult => ({ rowSet: false, rows: 0, affectedRows: count, truncated: false });

describe("query history formatting", () => {
  it("summarises result sets", () => {
    const sets = [
      { columns: [column], rows: [["1"], ["2"]], affectedRows: 0, truncated: false },
      { columns: [], rows: [], affectedRows: 7, truncated: false },
    ];
    expect(summarizeSets(sets)).toEqual([rows(2), affected(7)]);
  });

  it("describes the outcome", () => {
    expect(resultSummary({ results: [rows(1)], error: null, cancelled: false })).toBe("1 row");
    expect(resultSummary({ results: [rows(1200), affected(3)], error: null, cancelled: false })).toBe("1,200 rows · 3 affected");
    expect(resultSummary({ results: [rows(1000, true)], error: null, cancelled: false })).toBe("1,000+ rows");
    expect(resultSummary({ results: [], error: null, cancelled: false })).toBe("No result");
    expect(resultSummary({ results: [], error: "Table 'x' doesn't exist\nmore", cancelled: false })).toBe("Table 'x' doesn't exist");
    expect(resultSummary({ results: [], error: "Query execution was interrupted", cancelled: true })).toBe("Cancelled");
  });

  it("derives the status", () => {
    expect(historyStatus({ error: null, cancelled: false })).toBe("ok");
    expect(historyStatus({ error: "boom", cancelled: false })).toBe("error");
    expect(historyStatus({ error: "interrupted", cancelled: true })).toBe("cancelled");
  });

  it("formats relative times", () => {
    const now = Date.UTC(2026, 10, 7, 12, 0, 0);
    expect(relativeTime(now - 3_000, now)).toBe("now");
    expect(relativeTime(now - 45_000, now)).toBe("45 s ago");
    expect(relativeTime(now - 5 * 60_000, now)).toBe("5 min ago");
    expect(relativeTime(now - 3 * 3_600_000, now)).toBe("3 h ago");
    expect(relativeTime(now - 2 * 86_400_000, now)).toBe("2 d ago");
    expect(relativeTime(Date.UTC(2026, 0, 15, 12), now)).toBe("2026-01-15");
  });

  it("formats durations", () => {
    expect(formatDuration(850)).toBe("850 ms");
    expect(formatDuration(1234)).toBe("1.2 s");
    expect(formatDuration(42_000)).toBe("42 s");
    expect(formatDuration(125_000)).toBe("2 min 5 s");
    expect(formatDuration(120_000)).toBe("2 min");
  });

  it("takes the first non-empty line", () => {
    expect(firstLine("\n  SELECT *\nFROM t")).toBe("SELECT *");
    expect(firstLine("x".repeat(10), 5)).toBe("xxxx…");
    expect(firstLine("")).toBe("");
  });
});
