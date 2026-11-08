import { describe, expect, it } from "vitest";
import type { Value } from "../../core/contract";
import {
  checkJson,
  describeJsonError,
  editText,
  formatJson,
  hexDump,
  isJsonKind,
  lineColumn,
  looksLikeJson,
  minifyJson,
  pastedValue,
  splitPasted,
  textStats,
  vectorListing,
} from "./cellValue";

describe("cell value helpers", () => {
  it("detects JSON objects and arrays", () => {
    expect(looksLikeJson('  {"a":1}')).toBe(true);
    expect(looksLikeJson("[1,2]")).toBe(true);
    expect(looksLikeJson("42")).toBe(false);
    expect(looksLikeJson("hello {")).toBe(false);
  });

  it("formats and minifies JSON", () => {
    expect(formatJson('{"a":1,"b":[1,2]}')).toBe('{\n  "a": 1,\n  "b": [\n    1,\n    2\n  ]\n}');
    expect(minifyJson('{\n  "a": 1,\n  "b": [ 1, 2 ]\n}')).toBe('{"a":1,"b":[1,2]}');
    expect(() => formatJson("{bad")).toThrow();
  });

  it("validates JSON and reports a position", () => {
    expect(checkJson('{"a": 1}').valid).toBe(true);
    const check = checkJson('{\n  "a": 1,\n}');
    expect(check.valid).toBe(false);
    expect(check.message).not.toBe("");
  });

  it("reads V8, Firefox and end-of-input messages", () => {
    const text = '{\n  "a": 1,\n}';
    const v8 = describeJsonError(text, "Expected double-quoted property name in JSON at position 12 (line 3 column 1)");
    expect(v8).toEqual({ valid: false, message: "Expected double-quoted property name", position: 12, line: 3, column: 1 });
    const oldV8 = describeJsonError("[1,]", "Unexpected token ] in JSON at position 3");
    expect(oldV8).toMatchObject({ message: "Unexpected token ]", position: 3, line: 1, column: 4 });
    const firefox = describeJsonError(text, "JSON.parse: expected double-quoted property name at line 3 column 1 of the JSON data");
    expect(firefox).toMatchObject({ message: "expected double-quoted property name", position: 12, line: 3, column: 1 });
    const end = describeJsonError("[1,", "Unexpected end of JSON input");
    expect(end).toMatchObject({ position: 3, line: 1, column: 4 });
    expect(describeJsonError("x", "Something odd")).toMatchObject({ position: null, line: null });
  });

  it("converts offsets to line and column", () => {
    expect(lineColumn("ab\ncd", 0)).toEqual({ line: 1, column: 1 });
    expect(lineColumn("ab\ncd", 4)).toEqual({ line: 2, column: 2 });
  });

  it("counts lines and characters", () => {
    expect(textStats("")).toEqual({ lines: 1, chars: 0 });
    expect(textStats("a\nbc\n")).toEqual({ lines: 3, chars: 5 });
    expect(textStats("héllo 😀")).toEqual({ lines: 1, chars: 7 });
  });

  it("dumps binary values as hex", () => {
    expect(hexDump("0x48656c6c6f00")).toBe(`00000000  48 65 6c 6c 6f 00${" ".repeat(30)}  Hello.`);
    const long = hexDump(`0x${"41".repeat(17)}`).split("\n");
    expect(long).toHaveLength(2);
    expect(long[1].startsWith("00000010  41")).toBe(true);
    expect(hexDump("0x")).toBe("(empty)");
    expect(hexDump("not hex")).toBe("not hex");
  });

  it("lists vector components", () => {
    expect(vectorListing([0.5, -1, 2])).toBe("0  0.5\n1  -1\n2  2");
  });
});

describe("typed cell helpers", () => {
  it("starts editors with documents and arrays as JSON", () => {
    const doc: Value = { t: "doc", v: [["a", { t: "int", v: 1 }]] };
    expect(editText(doc)).toBe('{"a":1}');
    expect(editText(doc, true)).toBe('{\n  "a": 1\n}');
    expect(editText({ t: "json", v: '{"b":2}' }, true)).toBe('{\n  "b": 2\n}');
    expect(editText({ t: "null", v: null })).toBe("");
    expect(editText({ t: "decimal", v: "1.50" })).toBe("1.50");
  });

  it("parses pasted text for the column, NULL spelled out", () => {
    expect(pastedValue({ kind: "int" }, "7")).toEqual({ t: "int", v: 7 });
    expect(pastedValue({ kind: "text" }, "NULL")).toEqual({ t: "null", v: null });
    expect(pastedValue({ kind: "date" }, "nope")).toBeInstanceOf(Error);
  });

  it("splits a copied block into rows and fields", () => {
    expect(splitPasted("a\tb\r\nc\td\n")).toEqual([
      ["a", "b"],
      ["c", "d"],
    ]);
    expect(splitPasted("one")).toEqual([["one"]]);
  });

  it("knows which kinds open the JSON editor", () => {
    expect(isJsonKind({ kind: "document" })).toBe(true);
    expect(isJsonKind({ kind: "array", of: { kind: "int" } })).toBe(true);
    expect(isJsonKind({ kind: "text" })).toBe(false);
  });
});
