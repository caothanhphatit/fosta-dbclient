/**
 * The page exporters and the backend's streaming export (`src-tauri/src/export.rs`) must write the same bytes: both
 * check themselves against the expected text in `core/__fixtures__/export/rows.json`. After a deliberate format change,
 * update the expected text there from this test's failure output and review the diff.
 */
import { describe, expect, it } from "vitest";
import type { ColumnInfo, Value } from "../../core/contract";
import fixture from "../../core/__fixtures__/export/rows.json";
import { exportDelimited, exportRows } from "./exporters";

const BOM = String.fromCharCode(0xfeff);
const columns = fixture.columns as unknown as ColumnInfo[];
const rows = fixture.rows as unknown as Value[][];

describe("export fixture", () => {
  it("matches the text the backend streams", () => {
    expect(exportRows("csv", columns, rows, "t", "ansi")).toBe(fixture.csv);
    expect(`${BOM}${exportDelimited(columns, rows, ";")}`).toBe(fixture.excel);
    expect(exportRows("json", columns, rows, "t", "ansi")).toBe(fixture.json);
    expect(exportRows("json", columns, [], "t", "ansi")).toBe(fixture.jsonEmpty);
  });
});
