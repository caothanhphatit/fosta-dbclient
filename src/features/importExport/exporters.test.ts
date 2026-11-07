import { describe, expect, it } from "vitest";
import type { ColumnMeta } from "../../core/types";
import { parseCsv } from "./csv";
import { exportDelimited, exportRows } from "./exporters";

const column = (name: string, vectorDims: number | null = null): ColumnMeta => ({
  name,
  typeName: "",
  numeric: false,
  binary: false,
  primary: false,
  vectorDims,
});

describe("exportDelimited", () => {
  const columns = [column("id"), column("note"), column("embedding", 4)];
  const rows = [
    ["1", "Line 1\r\nLine 2", "[0.1,0.2,0.3,0.4]"],
    ["2", 'say "hi"; bye', null],
    ["3", null, null],
  ];

  it("keeps multi-line cells in one record and quotes the separator", () => {
    const text = exportDelimited(columns, rows, ";");
    expect(parseCsv(text, ";")).toEqual([
      ["id", "note"],
      ["1", "Line 1\r\nLine 2"],
      ["2", 'say "hi"; bye'],
      ["3", ""],
    ]);
  });

  it("matches the comma CSV export when the separator is a comma", () => {
    expect(exportDelimited(columns, rows, ",")).toBe(exportRows("csv", columns, rows, "t"));
  });

  it("leaves a comma unquoted when it is not the separator", () => {
    expect(exportDelimited([column("a")], [["x,y"]], ";")).toBe("a\r\nx,y");
  });
});
