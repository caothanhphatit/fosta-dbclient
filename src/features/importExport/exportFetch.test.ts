import { describe, expect, it } from "vitest";
import { ExportCancelled, exportFileName, fetchAllRows, FETCH_PAGE } from "./exportFetch";

const pageOf = (count: number) => Array.from({ length: count }, (_, index) => [String(index)]);

describe("fetchAllRows", () => {
  it("reads every page", async () => {
    const offsets: number[] = [];
    const rows = await fetchAllRows(
      async (offset) => {
        offsets.push(offset);
        return pageOf(offset === 0 ? FETCH_PAGE : 5);
      },
      FETCH_PAGE + 5,
      new AbortController().signal,
    );
    expect(rows).toHaveLength(FETCH_PAGE + 5);
    expect(offsets).toEqual([0, FETCH_PAGE]);
  });

  it("stops when the dialog is closed while a page is loading", async () => {
    const controller = new AbortController();
    let calls = 0;
    const reading = fetchAllRows(
      async () => {
        calls += 1;
        controller.abort();
        return pageOf(FETCH_PAGE);
      },
      10 * FETCH_PAGE,
      controller.signal,
    );
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
