import { describe, expect, it } from "vitest";
import type { ColumnInfo, ValueKind } from "../../core/contract";
import { importChanges, importValue } from "./importValues";

const col = (name: string, kind: ValueKind["kind"]): ColumnInfo => ({
  name,
  typeName: kind,
  kind: { kind } as ValueKind,
  nullable: true,
  default: null,
  comment: null,
  identity: null,
  generated: null,
  hints: {},
});

const id = col("id", "int");
const name = col("name", "text");
const day = col("day", "date");
const meta = col("meta", "json");

describe("importValue", () => {
  it("parses each field for its column's kind", () => {
    expect(importValue(id, "42", true)).toEqual({ t: "int", v: 42 });
    expect(importValue(day, "2026-1-5", true)).toEqual({ t: "date", v: "2026-01-05" });
    expect(importValue(meta, '{"a":1}', true)).toEqual({ t: "json", v: '{"a":1}' });
    expect(importValue(name, " x ", true)).toEqual({ t: "text", v: " x " });
  });

  it("turns missing and (optionally) empty fields into NULL", () => {
    expect(importValue(id, null, false)).toEqual({ t: "null", v: null });
    expect(importValue(id, "", true)).toEqual({ t: "null", v: null });
    expect(importValue(name, "", false)).toEqual({ t: "text", v: "" });
    expect(importValue(id, "", false)).toBeInstanceOf(Error);
  });
});

describe("importChanges", () => {
  it("builds inserts and reports the first errors with row and column", () => {
    const rows = [
      ["1", "Desk", "2026-01-01"],
      ["x", "Chair", "2026-02-30"],
    ];
    const mapping = [
      { index: 0, column: id },
      { index: 1, column: name },
      { index: 2, column: day },
    ];
    const { changes, errors, errorCount } = importChanges(rows, mapping, true);
    expect(changes[0]).toEqual({
      op: "insert",
      values: [
        { column: "id", value: { t: "int", v: 1 } },
        { column: "name", value: { t: "text", v: "Desk" } },
        { column: "day", value: { t: "date", v: "2026-01-01" } },
      ],
    });
    expect(errorCount).toBe(2);
    expect(errors[0]).toMatch(/^Row 2, id: /);
    expect(errors[1]).toMatch(/^Row 2, day: /);
  });

  it("leaves an empty identity column to the database", () => {
    const auto = { ...id, identity: "AUTO_INCREMENT" };
    const { changes } = importChanges(
      [["", "Desk"]],
      [
        { index: 0, column: auto },
        { index: 1, column: name },
      ],
      true,
    );
    expect(changes).toEqual([{ op: "insert", values: [{ column: "name", value: { t: "text", v: "Desk" } }] }]);
  });
});
