import { describe, expect, it } from "vitest";
import type { ObjectNode } from "../../core/contract";
import {
  cellOf,
  editFact,
  formatBytes,
  formatMillis,
  isFailure,
  isReadOnlyNode,
  numberCell,
  oneLine,
  readOnlyReason,
  READ_ONLY_BADGE,
  shortCount,
  type OperationTable,
} from "./model";

const node = (badges: string[]): ObjectNode => ({
  key: { id: "db:d/folder:tables/table:t", kind: "table", object: null },
  label: "t",
  kind: "table",
  icon: "table",
  object: null,
  detail: null,
  badges: badges.map((text) => ({ text, tone: "neutral" })),
  expandable: false,
  defaultView: "data",
  actions: [],
});

describe("edit rights", () => {
  it("reads the read-only badge of a tree node", () => {
    expect(isReadOnlyNode(node([READ_ONLY_BADGE]))).toBe(true);
    expect(isReadOnlyNode(node([]))).toBe(false);
    expect(isReadOnlyNode(null)).toBe(false);
  });

  it("reads the edits fact of a description", () => {
    const read = {
      extra: [
        ["engine", "Log"],
        ["edits", "read-only: Log has no mutations."],
      ] as [string, string][],
    };
    expect(readOnlyReason(read)).toBe("Log has no mutations.");
    const mutable = { extra: [["edits", "asynchronous mutations: ALTER TABLE … UPDATE"]] as [string, string][] };
    expect(readOnlyReason(mutable)).toBeNull();
    expect(editFact(mutable)).toContain("asynchronous");
    expect(editFact({ extra: [] })).toBeNull();
  });
});

describe("operation tables", () => {
  const table: OperationTable = {
    columns: ["mutation_id", "is_done"],
    rows: [
      ["0000000001", "1"],
      ["0000000002", null],
    ],
  };

  it("finds cells by column name", () => {
    expect(cellOf(table, table.rows[0], "mutation_id")).toBe("0000000001");
    expect(cellOf(table, table.rows[1], "is_done")).toBe("");
    expect(cellOf(table, table.rows[0], "nope")).toBe("");
  });

  it("tells failures in the query log", () => {
    expect(isFailure("ExceptionWhileProcessing")).toBe(true);
    expect(isFailure("QueryFinish")).toBe(false);
  });
});

describe("formatting", () => {
  it("shortens counts and sizes", () => {
    expect(shortCount(999)).toBe("999");
    expect(shortCount(12_000)).toBe("12K");
    expect(shortCount(1_234_567)).toBe("1.2M");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(52_428_800)).toBe("50 MiB");
    expect(formatBytes(1536)).toBe("1.5 KiB");
  });

  it("formats durations and numbers", () => {
    expect(formatMillis(250)).toBe("250 ms");
    expect(formatMillis(1500)).toBe("1.5 s");
    expect(formatMillis(61_000)).toBe("1 min 1 s");
    expect(numberCell("1234567")).toBe("1,234,567");
    expect(numberCell("n/a")).toBe("n/a");
    expect(numberCell("")).toBe("");
  });

  it("puts statements on one line", () => {
    expect(oneLine("SELECT 1\n  FROM   t")).toBe("SELECT 1 FROM t");
    expect(oneLine("x".repeat(300), 10)).toBe("xxxxxxxxxx…");
  });
});
