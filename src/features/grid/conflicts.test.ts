import { describe, expect, it } from "vitest";
import fixture from "../../core/__fixtures__/contract/conflict-error.json";
import type { Field, RowConflict, Value } from "../../core/contract";
import { conflictPayload, conflictRows, keyText } from "./conflicts";

const text = (v: string): Value => ({ t: "text", v });
const int = (v: number): Value => ({ t: "int", v });

describe("conflictPayload", () => {
  it("reads the backend's CONFLICT error", () => {
    const payload = conflictPayload(fixture);
    expect(payload?.conflicts).toHaveLength(2);
    expect(payload?.object?.name).toBe("orders");
    expect(payload?.compensated).toBeNull();
    expect(conflictPayload({ ...fixture, compensated: false })?.compensated).toBe(false);
  });

  it("ignores other errors", () => {
    expect(conflictPayload({ code: "ENGINE_ERROR", message: "x" })).toBeNull();
    expect(conflictPayload("CONFLICT")).toBeNull();
    expect(conflictPayload({ code: "CONFLICT", message: "no payload" })).toBeNull();
  });
});

describe("conflictRows", () => {
  it("lays out an update conflict with what changed on each side", () => {
    const [row] = conflictRows(conflictPayload(fixture)!.conflicts.slice(0, 1));
    expect(row.keyText).toBe("id = 1");
    expect(row.missing).toBe(false);
    expect(row.cells).toEqual([
      { column: "status", original: text("PAID"), current: text("CANCELLED"), attempted: text("SHIPPED"), changedNow: true, changedByYou: true },
    ]);
    expect(row.otherChange).toBe(false);
  });

  it("marks a row that no longer exists", () => {
    const [, row] = conflictRows(conflictPayload(fixture)!.conflicts);
    expect(row.op).toBe("delete");
    expect(row.missing).toBe(true);
    expect(row.cells[0]).toMatchObject({ column: "status", current: undefined, attempted: undefined, changedNow: false });
  });

  it("adds the columns someone else changed when the loaded row is known", () => {
    const conflict: RowConflict = {
      index: 0,
      op: "update",
      key: [{ column: "id", value: int(3) }],
      original: [{ column: "name", value: text("Desk") }],
      current: [
        { column: "id", value: int(3) },
        { column: "name", value: text("Desk") },
        { column: "stock", value: int(9) },
      ],
      attempted: [{ column: "name", value: text("Oak desk") }],
    };
    const loaded: Field[] = [
      { column: "id", value: int(3) },
      { column: "name", value: text("Desk") },
      { column: "stock", value: int(4) },
    ];
    const [row] = conflictRows([conflict], () => loaded);
    expect(row.cells.map((cell) => [cell.column, cell.changedNow, cell.changedByYou])).toEqual([
      ["name", false, true],
      ["stock", true, false],
    ]);
    expect(row.cells[1].original).toEqual(int(4));
    expect(row.otherChange).toBe(false);
    // Without the loaded row only the edited column shows, and the dialog says something else changed (a version bump).
    expect(conflictRows([conflict])[0].otherChange).toBe(true);
  });

  it("spells out NULL in keys", () => {
    expect(
      keyText([
        { column: "a", value: { t: "null", v: null } },
        { column: "b", value: int(2) },
      ]),
    ).toBe("a = NULL, b = 2");
  });
});
