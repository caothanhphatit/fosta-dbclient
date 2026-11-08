import { describe, expect, it } from "vitest";
import type { Value } from "../../core/contract";
import { ExportCancelled, exportFileName, fetchAllRows, FETCH_PAGE, type PageCursor } from "./exportFetch";

const rowsFrom = (start: number, count: number): Value[][] => Array.from({ length: count }, (_, index) => [{ t: "int", v: start + index }]);

describe("fetchAllRows", () => {
  it("follows the keyset the engine hands out", async () => {
    const cursors: PageCursor[] = [];
    const total = 2 * FETCH_PAGE + 5;
    const rows = await fetchAllRows(async (cursor, limit) => {
      cursors.push(cursor);
      const start = cursor.after ? Number((cursor.after[0] as { v: number }).v) + 1 : 0;
      const count = Math.min(limit, total - start);
      const page = rowsFrom(start, count);
      const hasMore = start + count < total;
      return { rows: page, hasMore, nextKeyset: hasMore ? page[page.length - 1] : null };
    }, new AbortController().signal);
    expect(rows).toHaveLength(total);
    expect(cursors.map((cursor) => cursor.after?.[0] ?? null)).toEqual([null, { t: "int", v: FETCH_PAGE - 1 }, { t: "int", v: 2 * FETCH_PAGE - 1 }]);
  });

  it("falls back to offsets without a keyset", async () => {
    const offsets: number[] = [];
    const rows = await fetchAllRows(async (cursor) => {
      offsets.push(cursor.offset);
      expect(cursor.after).toBeNull();
      const count = cursor.offset === 0 ? FETCH_PAGE : 5;
      return { rows: rowsFrom(cursor.offset, count), hasMore: cursor.offset === 0, nextKeyset: null };
    }, new AbortController().signal);
    expect(rows).toHaveLength(FETCH_PAGE + 5);
    expect(offsets).toEqual([0, FETCH_PAGE]);
  });

  it("stops when the dialog is closed while a page is loading", async () => {
    const controller = new AbortController();
    let calls = 0;
    const reading = fetchAllRows(async () => {
      calls += 1;
      controller.abort();
      return { rows: rowsFrom(0, FETCH_PAGE), hasMore: true, nextKeyset: null };
    }, controller.signal);
    await expect(reading).rejects.toBeInstanceOf(ExportCancelled);
    expect(calls).toBe(1);
  });
});

describe("exportFileName", () => {
  it("falls back to the table name with the format's extension", () => {
    expect(exportFileName("  ", "orders", "json")).toBe("orders.json");
    expect(exportFileName("mine.csv", "orders", "json")).toBe("mine.csv");
  });
});
