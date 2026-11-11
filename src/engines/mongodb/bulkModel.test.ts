import { describe, expect, it } from "vitest";
import { deleteArgs, matchesEverything, parseUpdate, previewArgs, previewModel, runLabel, updateArgs } from "./bulkModel";
import { defaultType, duplicateText, initialText, parseDocumentInput, parseInput } from "./valueInput";

const target = { database: "shop", collection: "orders" };

describe("bulk update model", () => {
  it("parses update operators and pipelines in mongosh syntax", () => {
    expect(parseUpdate("{ $set: { status: 'archived', at: ISODate('2026-01-02T00:00:00Z') } }")).toEqual({
      ok: true,
      json: '{"$set":{"status":"archived","at":{"$date":{"$numberLong":"1767312000000"}}}}',
    });
    expect(parseUpdate("[ { $set: { n: 1 } } ]").ok).toBe(true);
    for (const bad of ["", "{}", "{ status: 'x' }", "[]", "[ { $set: {}, $unset: 'a' } ]", "5", "{ $set: "]) {
      expect(parseUpdate(bad).ok, bad).toBe(false);
    }
    const replacement = parseUpdate("{ status: 'x' }");
    expect(!replacement.ok && replacement.error).toMatch(/replace whole documents/);
    const javascript = parseUpdate("{ $set: { a: function () { return 1 } } }");
    expect(javascript.ok).toBe(false);
  });

  it("builds the operation arguments from the bar's filter", () => {
    expect(previewArgs(target, '{"status":"new"}', '{"$set":{"a":1}}')).toEqual({
      ...target,
      filter: '{"status":"new"}',
      update: '{"$set":{"a":1}}',
      sample: 5,
    });
    expect(updateArgs(target, null, "{}")).toEqual({ ...target, filter: "", update: "{}" });
    expect(deleteArgs(target, '{"a":1}')).toEqual({ ...target, filter: '{"a":1}' });
    expect(matchesEverything(null)).toBe(true);
    expect(matchesEverything("{}")).toBe(true);
    expect(matchesEverything(" ")).toBe(true);
    expect(matchesEverything('{"a":1}')).toBe(false);
  });

  it("says what the preview shows", () => {
    const model = previewModel({
      matched: 12,
      sample: [
        { before: { _id: 1, status: "new" }, after: { _id: 1, status: "done" }, changed: true },
        { before: { _id: 2, status: "done" }, after: { _id: 2, status: "done" }, changed: false },
      ],
      elapsedMs: 3,
      statement: "",
    });
    expect(model.headline).toBe("12 documents match. The first 2 (by _id): 1 change.");
    expect(model.rows[0]).toEqual({ id: 1, diff: [{ path: "status", kind: "changed", before: "new", after: "done" }] });
    expect(model.rows[1].diff).toEqual([]);
    expect(previewModel({ matched: 0, sample: [], elapsedMs: 1, statement: "" }).headline).toBe("No documents match the filter.");
    expect(runLabel("update", 1)).toBe("Update 1 document");
    expect(runLabel("delete", 1200)).toBe("Delete 1,200 documents");
    expect(runLabel("delete", null)).toBe("Delete matching documents");
  });
});

describe("value editor types", () => {
  it("starts from the value's type and text", () => {
    expect(defaultType("x")).toBe("string");
    expect(defaultType(5)).toBe("number");
    expect(defaultType({ $numberLong: "5" })).toBe("long");
    expect(defaultType({ $date: "2026-01-01T00:00:00Z" })).toBe("date");
    expect(defaultType({ a: 1 })).toBe("json");
    expect(defaultType([1])).toBe("json");
    expect(initialText({ $numberDecimal: "1.50" }, "decimal")).toBe("1.50");
    expect(initialText({ $oid: "68e783b8c0ffee0000000001" }, "objectId")).toBe("68e783b8c0ffee0000000001");
    expect(initialText({ $date: "2026-01-01T00:00:00Z" }, "date")).toBe("2026-01-01T00:00:00.000Z");
    expect(initialText({ a: { $oid: "68e783b8c0ffee0000000001" } }, "json")).toBe("{\n  a: ObjectId('68e783b8c0ffee0000000001')\n}");
    expect(initialText("12", "int")).toBe("12");
  });

  it("types the text as Extended JSON or says why it does not fit", () => {
    expect(parseInput("string", " a ")).toEqual({ ok: true, value: " a ", keepType: false });
    expect(parseInput("number", "4.5")).toEqual({ ok: true, value: 4.5, keepType: true });
    expect(parseInput("double", "10")).toEqual({ ok: true, value: { $numberDouble: "10" }, keepType: false });
    expect(parseInput("int", "7")).toEqual({ ok: true, value: { $numberInt: "7" }, keepType: false });
    expect(parseInput("long", "9007199254740993")).toEqual({ ok: true, value: { $numberLong: "9007199254740993" }, keepType: false });
    expect(parseInput("decimal", "19.990")).toEqual({ ok: true, value: { $numberDecimal: "19.990" }, keepType: false });
    expect(parseInput("bool", "FALSE")).toEqual({ ok: true, value: false, keepType: false });
    expect(parseInput("date", "2026-01-02")).toEqual({ ok: true, value: { $date: "2026-01-02T00:00:00.000Z" }, keepType: false });
    expect(parseInput("date", "2026-01-02 08:30")).toEqual({ ok: true, value: { $date: "2026-01-02T08:30:00.000Z" }, keepType: false });
    expect(parseInput("date", "2026-01-02T08:30:00+02:00")).toEqual({ ok: true, value: { $date: "2026-01-02T06:30:00.000Z" }, keepType: false });
    expect(parseInput("objectId", "68E783B8C0FFEE0000000001")).toEqual({ ok: true, value: { $oid: "68e783b8c0ffee0000000001" }, keepType: false });
    expect(parseInput("null", "anything")).toEqual({ ok: true, value: null, keepType: false });
    expect(parseInput("json", "{ n: NumberLong('5'), tags: ['a'] }")).toEqual({ ok: true, value: { n: { $numberLong: "5" }, tags: ["a"] }, keepType: false });
    for (const [type, text] of [
      ["number", "x"],
      ["int", "2147483648"],
      ["int", "1.5"],
      ["long", "9223372036854775808"],
      ["decimal", "1,5"],
      ["bool", "yes"],
      ["date", "soon"],
      ["objectId", "abc"],
      ["json", "{ a: "],
      ["json", "{ $where: 'true' }"],
    ] as const) {
      expect(parseInput(type, text).ok, `${type} ${text}`).toBe(false);
    }
  });

  it("parses new documents and duplicates without _id", () => {
    expect(parseDocumentInput("{ name: 'x', at: ISODate('2026-01-02T00:00:00Z') }")).toEqual({
      ok: true,
      document: { name: "x", at: { $date: { $numberLong: "1767312000000" } } },
    });
    expect(parseDocumentInput("[1]").ok).toBe(false);
    expect(duplicateText({ _id: 1, name: "x" })).toBe("{\n  name: 'x'\n}");
  });
});
