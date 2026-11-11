import { describe, expect, it } from "vitest";
import { lineColumn, parseMongoShell } from "./shell";

const json = (text: string) => {
  const result = parseMongoShell(text);
  if (!result.ok) throw new Error(`${text}: ${result.error} at ${result.position}`);
  return result.json;
};
const value = (text: string): unknown => JSON.parse(json(text));

describe("parseMongoShell: accepted syntax", () => {
  it.each<[string, unknown]>([
    // JSON stays JSON
    ['{ "status": "paid", "n": { "$gt": 2 } }', { status: "paid", n: { $gt: 2 } }],
    ["{}", {}],
    ["[]", []],
    ['"text"', "text"],
    ["true", true],
    ["null", null],
    // Unquoted keys, single quotes, trailing commas, comments
    ["{ status: 'paid', }", { status: "paid" }],
    ["{ $and: [ { a: 1 }, { _b$: 2 }, ], }", { $and: [{ a: 1 }, { _b$: 2 }] }],
    ["{ 'a.b': 1, \"c-d\": 2 }", { "a.b": 1, "c-d": 2 }],
    ["{ 1: 'x', 1.50: 'y' }", { "1": "x", "1.5": "y" }],
    ["{ new: 1, true: 2, function: 3 }", { new: 1, true: 2, function: 3 }],
    ["{ héllo: 1 }", { héllo: 1 }],
    ["// leading\n{ a: /* inline */ 1 } // trailing", { a: 1 }],
    ["/* block\n comment */ { a: 1 };", { a: 1 }],
    // String escapes
    ["'it\\'s'", "it's"],
    ['"say \\"hi\\""', 'say "hi"'],
    ["'\\n\\t\\\\\\/\\x41\\u0042\\u{1F600}\\0'", "\n\t\\/AB\u{1F600}\0"],
    ["'line\\\ncontinues'", "linecontinues"],
    ["'\"double\" inside single'", '"double" inside single'],
    // Numbers: int32 plain, everything else a double
    ["{ n: 5, m: -7, z: -0 }", { n: 5, m: -7, z: 0 }],
    ["2147483647", 2147483647],
    ["-2147483648", -2147483648],
    ["2147483648", { $numberDouble: "2147483648" }],
    ["1.5", 1.5],
    [".5", 0.5],
    ["+3", 3],
    ["2.0", 2],
    ["1e3", 1000],
    ["1e21", { $numberDouble: "1e+21" }],
    ["1.5e-7", 1.5e-7],
    ["0x1F", 31],
    ["0b101", 5],
    ["0o17", 15],
    ["NaN", { $numberDouble: "NaN" }],
    ["Infinity", { $numberDouble: "Infinity" }],
    ["-Infinity", { $numberDouble: "-Infinity" }],
    ["9007199254740991", { $numberDouble: "9007199254740991" }],
  ])("%s", (text, expected) => {
    expect(value(text)).toEqual(expected);
  });

  it("keeps key order, including integer-like keys", () => {
    expect(json("{ b: 1, 2: 2, a: 3 }")).toBe('{"b":1,"2":2,"a":3}');
  });
});

describe("parseMongoShell: shell helpers to canonical Extended JSON", () => {
  it.each<[string, unknown]>([
    ['ObjectId("65A1F0C2E4B0A1B2C3D4E5F6")', { $oid: "65a1f0c2e4b0a1b2c3d4e5f6" }],
    ["new ObjectId('65a1f0c2e4b0a1b2c3d4e5f6')", { $oid: "65a1f0c2e4b0a1b2c3d4e5f6" }],
    ['ISODate("2024-01-31T12:30:00Z")', { $date: { $numberLong: "1706704200000" } }],
    ['ISODate("2024-01-31T12:30:00")', { $date: { $numberLong: "1706704200000" } }],
    ['ISODate("2024-01-31 12:30")', { $date: { $numberLong: "1706704200000" } }],
    ['ISODate("2024-01-31T12:30:00.123456Z")', { $date: { $numberLong: "1706704200123" } }],
    ['ISODate("2024-01-31T14:30:00+02:00")', { $date: { $numberLong: "1706704200000" } }],
    ['ISODate("2024-01-31T07:00:00-0530")', { $date: { $numberLong: "1706704200000" } }],
    ['ISODate("2024-01-31")', { $date: { $numberLong: "1706659200000" } }],
    ['ISODate("1969-12-31T23:59:59.999Z")', { $date: { $numberLong: "-1" } }],
    ['ISODate("1970-01-01T00:00:00Z")', { $date: { $numberLong: "0" } }],
    ['ISODate("0001-01-01T00:00:00Z")', { $date: { $numberLong: "-62135596800000" } }],
    ['new Date("2024-01-31T12:30:00Z")', { $date: { $numberLong: "1706704200000" } }],
    ['new Date("2024-01-31")', { $date: { $numberLong: "1706659200000" } }],
    ["new Date(1706704200000)", { $date: { $numberLong: "1706704200000" } }],
    ["new Date(-1)", { $date: { $numberLong: "-1" } }],
    ['NumberLong("9223372036854775807")', { $numberLong: "9223372036854775807" }],
    ['NumberLong("-9223372036854775808")', { $numberLong: "-9223372036854775808" }],
    ["NumberLong(5)", { $numberLong: "5" }],
    ['NumberLong("+007")', { $numberLong: "7" }],
    ["NumberInt(5)", { $numberInt: "5" }],
    ['NumberInt("-2147483648")', { $numberInt: "-2147483648" }],
    ['NumberDecimal("19.99")', { $numberDecimal: "19.99" }],
    ["NumberDecimal(0.1)", { $numberDecimal: "0.1" }],
    ['Decimal128("-1.5E+3")', { $numberDecimal: "-1.5E+3" }],
    ['NumberDecimal("NaN")', { $numberDecimal: "NaN" }],
    ['UUID("123e4567-e89b-12d3-a456-426614174000")', { $binary: { base64: "Ej5FZ+ibEtOkVkJmFBdAAA==", subType: "04" } }],
    ['UUID("123E4567E89B12D3A456426614174000")', { $binary: { base64: "Ej5FZ+ibEtOkVkJmFBdAAA==", subType: "04" } }],
    ['BinData(0, "AQID")', { $binary: { base64: "AQID", subType: "00" } }],
    ['BinData(128, "")', { $binary: { base64: "", subType: "80" } }],
    ["Timestamp(1700000000, 1)", { $timestamp: { t: 1700000000, i: 1 } }],
    ["Timestamp({ t: 1700000000, i: 2 })", { $timestamp: { t: 1700000000, i: 2 } }],
    ["MinKey", { $minKey: 1 }],
    ["MaxKey()", { $maxKey: 1 }],
    ["/^ab+c$/", { $regularExpression: { pattern: "^ab+c$", options: "" } }],
    ["/a\\/b[/]c/si", { $regularExpression: { pattern: "a\\/b[/]c", options: "is" } }],
    ["{ name: /^jo/i }", { name: { $regularExpression: { pattern: "^jo", options: "i" } } }],
    ['RegExp("^a", "xi")', { $regularExpression: { pattern: "^a", options: "ix" } }],
    ['new RegExp("^a")', { $regularExpression: { pattern: "^a", options: "" } }],
  ])("%s", (text, expected) => {
    expect(value(text)).toEqual(expected);
  });

  it("translates a realistic filter", () => {
    const text = `{
      _id: ObjectId('65a1f0c2e4b0a1b2c3d4e5f6'), // the order
      createdAt: { $gte: ISODate("2024-01-01"), $lt: new Date("2024-02-01T00:00:00Z") },
      total: { $gt: NumberDecimal("100.00") },
      tags: { $in: ['a', "b", ] },
      email: /@example\\.com$/i,
    }`;
    expect(value(text)).toEqual({
      _id: { $oid: "65a1f0c2e4b0a1b2c3d4e5f6" },
      createdAt: { $gte: { $date: { $numberLong: "1704067200000" } }, $lt: { $date: { $numberLong: "1706745600000" } } },
      total: { $gt: { $numberDecimal: "100.00" } },
      tags: { $in: ["a", "b"] },
      email: { $regularExpression: { pattern: "@example\\.com$", options: "i" } },
    });
  });

  it("translates a pipeline", () => {
    expect(parseMongoShell("[ { $match: { a: 1 } }, { $limit: 5 }, ]", { expect: "array" })).toEqual({
      ok: true,
      json: '[{"$match":{"a":1}},{"$limit":5}]',
    });
  });
});

describe("parseMongoShell: errors with positions", () => {
  it.each<[string, string, number]>([
    // Server-side JavaScript
    ["{ $where: 'this.a > 1' }", "$where runs JavaScript", 2],
    ['{ "$where": "sleep(1)" }', "$where runs JavaScript", 2],
    ["{ $expr: { $function: { body: 'x', args: [], lang: 'js' } } }", "$function runs JavaScript", 11],
    ["{ $group: { _id: 1, x: { $accumulator: {} } } }", "$accumulator runs JavaScript", 25],
    ["{ a: function () { return 1 } }", "A function runs JavaScript", 5],
    ["{ a: () => 1 }", "A function runs JavaScript", 5],
    ["{ a: x => 1 }", "A function runs JavaScript", 5],
    ["{ a: async () => 1 }", "A function runs JavaScript", 5],
    ["{ a() { return 1 } }", "A method runs JavaScript", 2],
    ["Code('return 1')", "Code runs JavaScript", 0],
    // Ambiguous helpers
    ["{ t: Date() }", "without new returns a string", 5],
    ['Date("2024-01-01")', "without new returns a string", 0],
    ["ISODate()", "is the current time", 0],
    ["new Date()", "is the current time", 4],
    ["new Date(2024, 0, 1)", "local time", 4],
    ['new Date("2024-01-31T12:00:00")', "local time", 9],
    ['new Date("Jan 31 2024")', "Expected an ISO date", 9],
    ["new Date(1.5)", "whole milliseconds", 9],
    ["ObjectId()", "makes a new id", 0],
    ['ObjectId("abc")', "24 hex digits", 9],
    ["ObjectId(123)", "needs a 24-digit hex string", 9],
    ['ObjectId("65a1f0c2e4b0a1b2c3d4e5f6", 1)', "takes one 24-digit hex string", 37],
    ["UUID()", "new random UUID", 0],
    ['UUID("123")', "32 hex digits", 5],
    ['ISODate("2024-02-30")', "Invalid date", 8],
    ['ISODate("2024-01-31T24:00:00Z")', "Invalid date", 8],
    ['ISODate("2024-01-31T12:00:00+25:00")', "Invalid zone offset", 8],
    ['ISODate("2024-01-31Z")', "A zone needs a time", 8],
    ["ISODate(0)", "needs an ISO date string", 8],
    // Numbers
    ['NumberLong("9223372036854775808")', "out of the 64-bit range", 11],
    ['NumberLong("-9223372036854775809")', "out of the 64-bit range", 11],
    ["NumberLong(9007199254740993)", "beyond 2^53", 11],
    ["NumberLong(1.5)", "needs an integer", 11],
    ['NumberLong("12abc")', "needs an integer", 11],
    ["NumberInt(2147483648)", "out of the 32-bit range", 10],
    ["NumberInt(1.5)", "needs an integer", 10],
    ['NumberInt("x")', "needs an integer", 10],
    ['NumberDecimal("abc")', "not a decimal number", 14],
    ['NumberDecimal("1.2345678901234567890123456789012345")', "34 significant digits", 14],
    ['NumberDecimal("1e7000")', "exponent is out of range", 14],
    ["NumberDecimal(NaN)", "needs a decimal string", 14],
    ["{ id: 12345678901234567890 }", "beyond 2^53", 6],
    ["0x20000000000001", "beyond 2^53", 0],
    ["{ a: 010 }", "leading zeros", 5],
    ["{ a: 1abc }", "Invalid number", 5],
    ["{ a: 1.2.3 }", "Invalid number", 5],
    ["{ a: 1e }", "digits in the exponent", 6],
    ["{ a: - 1 }", "Expected a number after '-'", 5],
    ["0x", "digits after the number prefix", 0],
    // Other helpers
    ['BinData(256, "AA==")', "subtype must be an integer", 8],
    ['BinData(0, "A")', "valid base64", 11],
    ["BinData(0)", "takes a subtype and a base64 string", 0],
    ["Timestamp(-1, 0)", "integers from 0 to 4294967295", 10],
    ["Timestamp({ t: 1 })", "needs both t and i", 10],
    ["Timestamp({ t: 1, i: 1, x: 1 })", "takes only t and i", 10],
    ["MinKey(1)", "takes no arguments", 7],
    ["/a/g", "flag 'g' has no meaning in a query", 3],
    ["/a/ii", "flag 'i' is repeated", 3],
    ["/a/x", "not a MongoDB regex option", 3],
    ['RegExp("a", "q")', "not a MongoDB regex option", 12],
    ["/abc", "Unterminated regular expression", 0],
    ["ObjectId", "Expected '(' after ObjectId", 8],
    ["Foo(1)", "Unknown function Foo", 0],
    ["new Foo(1)", "Unknown function Foo", 4],
    ["new 5", "Expected a type name after new", 4],
    ["new ObjectId", "Expected '(' after new ObjectId", 12],
    // Names and literals
    ["{ status: paid }", 'Unknown name "paid"', 10],
    ["{ a: undefined }", "undefined is ambiguous", 5],
    ["{ a: b.c }", "Property access", 6],
    ["{ a: `x` }", "Template literals", 5],
    ["{ a: (1) }", "Parenthesized", 5],
    // Structure
    ["", "Nothing to parse", 0],
    ["  // only a comment", "Nothing to parse", 19],
    ["{ a: 1, a: 2 }", 'Duplicate key "a"', 8],
    ["{ 'a': 1, a: 2 }", 'Duplicate key "a"', 10],
    ["{ a.b: 1 }", "Quote keys that contain '.'", 2],
    ["{ a-b: 1 }", "Quote keys that contain '-'", 2],
    ["{ a }", 'Key "a" needs a value', 4],
    ["{ [k]: 1 }", "Computed keys", 2],
    ["{ ...x }", "Spread syntax", 2],
    ["{ a 1 }", "Expected ':' after the key", 4],
    ["{ a: 1 b: 2 }", "Expected ',' or '}'", 7],
    ["{ a: 1,, }", "Expected a key", 7],
    ["{ a: 1", "Unexpected end of input, expected '}'", 6],
    ["{ a: ", "Unexpected end of input, expected a value", 5],
    ["[1,,2]", "Empty array element", 3],
    ["[1 2]", "Expected ',' or ']'", 3],
    ["[1,", "Unexpected end of input, expected a value", 3],
    ["{ a: 1 } { b: 2 }", "Unexpected text after the value", 9],
    ["{ a: 'unterminated }", "Unterminated string", 5],
    ["{ a: 'line\nbreak' }", "Unterminated string", 5],
    ["'\\x4'", "Invalid escape sequence", 1],
    ["'\\u{110000}'", "Invalid \\u{…} escape", 1],
    ["'\\012'", "Octal escapes", 1],
    ["{ a: 1 /* open", "Unterminated comment", 7],
    ["{ a: # }", "Unexpected character", 5],
  ])("%s", (text, message, position) => {
    const result = parseMongoShell(text);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error).toContain(message);
    expect(result.position).toBe(position);
  });
});

describe("parseMongoShell: expect", () => {
  it("checks the top-level kind", () => {
    expect(parseMongoShell("{ a: 1 }", { expect: "document" }).ok).toBe(true);
    expect(parseMongoShell("  [ ]", { expect: "document" })).toEqual({ ok: false, error: "Expected a document, e.g. { status: 'paid' }", position: 2 });
    expect(parseMongoShell("{ }", { expect: "array" })).toMatchObject({ ok: false, position: 0 });
    expect(parseMongoShell("5", { expect: "any" }).ok).toBe(true);
  });
});

describe("lineColumn", () => {
  it("maps an offset to a 1-based line and column", () => {
    expect(lineColumn("abc", 0)).toEqual({ line: 1, column: 1 });
    expect(lineColumn("{\n  a: x\n}", 7)).toEqual({ line: 2, column: 6 });
  });
});
