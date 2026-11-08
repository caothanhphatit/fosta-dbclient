import { describe, expect, it } from "vitest";
import type { ColumnInfo, Value, ValueKind } from "../../core/contract";
import { parseCsv } from "./csv";
import { excelSeparator, exportDelimited, exportRows } from "./exporters";

const column = (name: string, kind: ValueKind["kind"] = "text", vectorDims?: number): ColumnInfo => ({
  name,
  typeName: kind,
  kind: { kind } as ValueKind,
  nullable: true,
  default: null,
  comment: null,
  identity: null,
  generated: null,
  hints: vectorDims ? { vectorDims } : {},
});

const text = (v: string): Value => ({ t: "text", v });
const int = (v: number): Value => ({ t: "int", v });
const NULL: Value = { t: "null", v: null };
const vector: Value = { t: "other", type: "vector", v: "[0.1,0.2,0.3,0.4]" };

describe("exportDelimited", () => {
  const columns = [column("id", "int"), column("note"), column("embedding", "other", 4)];
  const rows = [
    [int(1), text("Line 1\r\nLine 2"), vector],
    [int(2), text('say "hi"; bye'), NULL],
    [int(3), NULL, NULL],
  ];

  it("keeps multi-line cells in one record and quotes the separator", () => {
    const csv = exportDelimited(columns, rows, ";");
    expect(parseCsv(csv, ";")).toEqual([
      ["id", "note"],
      ["1", "Line 1\r\nLine 2"],
      ["2", 'say "hi"; bye'],
      ["3", ""],
    ]);
  });

  it("matches the comma CSV export when the separator is a comma", () => {
    expect(exportDelimited(columns, rows, ",")).toBe(exportRows("csv", columns, rows, "t", "mysql"));
  });

  it("leaves a comma unquoted when it is not the separator", () => {
    expect(exportDelimited([column("a")], [[text("x,y")]], ";")).toBe("a\r\nx,y");
  });
});

describe("JSON export", () => {
  it("keeps numbers, booleans, NULL and documents typed", () => {
    const columns = [column("id", "int"), column("ok", "bool"), column("meta", "json"), column("note")];
    const rows: Value[][] = [[int(1), { t: "bool", v: true }, { t: "json", v: '{"a":[1,2]}' }, NULL]];
    expect(JSON.parse(exportRows("json", columns, rows, null, "ansi"))).toEqual([{ id: 1, ok: true, meta: { a: [1, 2] }, note: null }]);
  });
});

describe("excelSeparator", () => {
  it("follows the locale's decimal separator", () => {
    expect(excelSeparator("en-US")).toBe(",");
    expect(excelSeparator("en-GB")).toBe(",");
    expect(excelSeparator("vi-VN")).toBe(";");
    expect(excelSeparator("de-DE")).toBe(";");
  });
});

describe("INSERT export per dialect", () => {
  const columns = [column("Id", "int"), column("Name")];
  const rows = [[int(1), text("it's")]];

  it("quotes for MySQL", () => {
    expect(exportRows("insert", columns, rows, "orders", "mysql")).toBe("INSERT INTO `orders` (`Id`, `Name`) VALUES (1, 'it''s');");
  });

  it("quotes for SQL Server, splitting schema.table", () => {
    expect(exportRows("insert", columns, rows, "sales.Invoices", "tsql")).toBe("INSERT INTO [sales].[Invoices] ([Id], [Name]) VALUES (1, N'it''s');");
  });

  it("quotes for ANSI engines", () => {
    expect(exportRows("insert", columns, rows, "public.orders", "ansi")).toBe(`INSERT INTO "public"."orders" ("Id", "Name") VALUES (1, 'it''s');`);
  });

  it("writes typed literals: NULL, booleans, bytes", () => {
    const typed = [column("a", "bool"), column("b", "bytes"), column("c")];
    const values: Value[][] = [[{ t: "bool", v: false }, { t: "bytes", v: "0xcafe" }, NULL]];
    expect(exportRows("insert", typed, values, "t", "mysql")).toBe("INSERT INTO `t` (`a`, `b`, `c`) VALUES (0, 0xcafe, NULL);");
    expect(exportRows("insert", typed, values, "t", "ansi")).toBe(`INSERT INTO "t" ("a", "b", "c") VALUES (FALSE, X'cafe', NULL);`);
  });
});
