import { describe, expect, it } from "vitest";
import type { ColumnInfo, Value, ValueKind } from "../../core/contract";
import { buildChanges, forced, isComparable, keyFields, rowChange, type PendingRow } from "./changeSet";

const col = (name: string, kind: ValueKind["kind"], hints: ColumnInfo["hints"] = {}): ColumnInfo => ({
  name,
  typeName: kind,
  kind: { kind } as ValueKind,
  nullable: true,
  default: null,
  comment: null,
  identity: null,
  generated: null,
  hints,
});

const int = (v: number): Value => ({ t: "int", v });
const text = (v: string): Value => ({ t: "text", v });
const NULL: Value = { t: "null", v: null };

const columns = [
  col("id", "int", { primary: true }),
  col("name", "text"),
  col("price", "decimal"),
  col("ratio", "float"),
  col("doc", "json"),
  col("photo", "bytes"),
  col("embedding", "other", { vectorDims: 4, binary: true }),
  col("updated", "timestamp"),
];

const loaded: Value[] = [
  int(7),
  text("Desk"),
  { t: "decimal", v: "19.50" },
  { t: "float", v: 0.5 },
  { t: "json", v: '{"a":1}' },
  { t: "bytes", v: "0xcafe" },
  { t: "other", type: "vector", v: "[0,1,2,3]" },
  { t: "timestamp", v: "2026-01-02 03:04:05" },
];

const row = (edit: (current: Value[]) => void = () => undefined, extra: Partial<PendingRow> = {}): PendingRow => {
  const current = [...loaded];
  edit(current);
  return { original: loaded, current, deleted: false, ...extra };
};

describe("rowChange", () => {
  it("returns null for an unchanged row", () => {
    expect(rowChange(columns, ["id"], row())).toBeNull();
  });

  it("sends an update with the edited columns' loaded values as original", () => {
    const change = rowChange(
      columns,
      ["id"],
      row((current) => (current[1] = text("Oak desk"))),
    );
    expect(change).toEqual({
      op: "update",
      key: [{ column: "id", value: int(7) }],
      values: [{ column: "name", value: text("Oak desk") }],
      original: [{ column: "name", value: text("Desk") }],
    });
  });

  it("merges several edits of one row into one update and ignores an edit that was put back", () => {
    const change = rowChange(
      columns,
      ["id"],
      row((current) => {
        current[1] = text("Oak desk");
        current[2] = { t: "decimal", v: "21.00" };
        current[7] = { t: "timestamp", v: "2026-01-02 03:04:05" };
      }),
    );
    expect(change?.op).toBe("update");
    if (change?.op !== "update") return;
    expect(change.values.map((field) => field.column)).toEqual(["name", "price"]);
    expect(change.original).toEqual([
      { column: "name", value: text("Desk") },
      { column: "price", value: { t: "decimal", v: "19.50" } },
    ]);
  });

  it("sends NULL explicitly, and a NULL that was loaded as the original", () => {
    const toNull = rowChange(
      columns,
      ["id"],
      row((current) => (current[1] = NULL)),
    );
    expect(toNull).toMatchObject({ values: [{ column: "name", value: NULL }], original: [{ column: "name", value: text("Desk") }] });
    const fromNull: PendingRow = { original: loaded.map((value, index) => (index === 1 ? NULL : value)), current: [...loaded], deleted: false };
    expect(rowChange(columns, ["id"], fromNull)).toMatchObject({
      values: [{ column: "name", value: text("Desk") }],
      original: [{ column: "name", value: NULL }],
    });
  });

  it("passes the row version through on updates and deletes", () => {
    const version: Value = { t: "bytes", v: "0x00000000000007d1" };
    const update = rowChange(
      columns,
      ["id"],
      row((current) => (current[1] = text("x")), { version }),
    );
    expect(update).toMatchObject({ op: "update", version });
    expect(rowChange(columns, ["id"], row(undefined, { deleted: true, version }))).toMatchObject({ op: "delete", version });
    expect(
      rowChange(
        columns,
        ["id"],
        row((current) => (current[1] = text("x"))),
      ),
    ).not.toHaveProperty("version");
  });

  it("sends only comparable columns as a delete's original", () => {
    const change = rowChange(columns, ["id"], row(undefined, { deleted: true }));
    expect(change).toEqual({
      op: "delete",
      key: [{ column: "id", value: int(7) }],
      original: [
        { column: "id", value: int(7) },
        { column: "name", value: text("Desk") },
        { column: "price", value: { t: "decimal", v: "19.50" } },
        { column: "updated", value: { t: "timestamp", v: "2026-01-02 03:04:05" } },
      ],
    });
  });

  it("skips read-only and binary-hinted columns in a delete's original", () => {
    const hinted = [col("id", "int"), col("code", "text", { binary: true }), col("total", "int", { readOnly: true })];
    const change = rowChange(hinted, ["id"], { original: [int(1), text("a"), int(3)], current: [int(1), text("a"), int(3)], deleted: true });
    expect(change).toMatchObject({ original: [{ column: "id", value: int(1) }] });
  });

  it("inserts the touched cells (an explicit NULL included) or, without that, the non-NULL ones", () => {
    const current = columns.map((): Value => NULL);
    current[1] = text("New");
    expect(rowChange(columns, ["id"], { original: null, current, deleted: false })).toEqual({ op: "insert", values: [{ column: "name", value: text("New") }] });
    expect(rowChange(columns, ["id"], { original: null, current, deleted: false, touched: new Set([1, 2]) })).toEqual({
      op: "insert",
      values: [
        { column: "name", value: text("New") },
        { column: "price", value: NULL },
      ],
    });
  });

  it("never writes a vector column", () => {
    const change = rowChange(
      columns,
      ["id"],
      row((current) => (current[6] = { t: "other", type: "vector", v: "[9]" })),
    );
    expect(change).toBeNull();
  });
});

describe("keyFields", () => {
  it("uses the comparable columns when the object has no key", () => {
    expect(keyFields(columns, [], loaded).map((field) => field.column)).toEqual(["id", "name", "price", "updated"]);
  });
});

describe("isComparable", () => {
  it("leaves out inexact kinds", () => {
    expect(columns.filter(isComparable).map((column) => column.name)).toEqual(["id", "name", "price", "updated"]);
  });
});

describe("buildChanges", () => {
  it("keeps the grid row index of each change and drops a deleted new row", () => {
    const rows: PendingRow[] = [
      row(),
      row((current) => (current[1] = text("x"))),
      { original: null, current: columns.map(() => NULL), deleted: true },
      row(undefined, { deleted: true }),
    ];
    expect(buildChanges(columns, ["id"], rows).map(({ row: index, change }) => [index, change.op])).toEqual([
      [1, "update"],
      [3, "delete"],
    ]);
  });

  it("forced() marks updates and deletes for Overwrite", () => {
    const change = rowChange(columns, ["id"], row(undefined, { deleted: true }));
    expect(change && forced(change)).toMatchObject({ op: "delete", force: true });
  });
});
