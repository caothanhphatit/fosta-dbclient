import { describe, expect, it } from "vitest";
import { parseMongoShell } from "../../lang/mongo/shell";
import { bsonType, dateOf, localDateText, objectIdTime, readable, shellText, uuidFromBase64 } from "./bson";

const UUID = "123e4567-e89b-12d3-a456-426614174000";
const UUID_BASE64 = "Ej5FZ+ibEtOkVkJmFBdAAA==";

describe("readable BSON", () => {
  it("names the type of relaxed Extended JSON values", () => {
    expect(bsonType("x")).toBe("string");
    expect(bsonType(1.5)).toBe("number");
    expect(bsonType({ $numberLong: "9007199254740993" })).toBe("long");
    expect(bsonType({ $numberDecimal: "1.50" })).toBe("decimal");
    expect(bsonType({ $oid: "68e783b8c0ffee0000000001" })).toBe("objectId");
    expect(bsonType({ $date: "2026-02-02T15:01:00Z" })).toBe("date");
    expect(bsonType({ $binary: { base64: UUID_BASE64, subType: "04" } })).toBe("uuid");
    expect(bsonType({ $binary: { base64: "AQ==", subType: "00" } })).toBe("binary");
    expect(bsonType({ $timestamp: { t: 1, i: 2 } })).toBe("timestamp");
    expect(bsonType({ a: 1 })).toBe("object");
    expect(bsonType([1])).toBe("array");
    expect(bsonType(null)).toBe("null");
  });

  it("tells when an ObjectId was made", () => {
    expect(objectIdTime("68e783b8c0ffee0000000001")?.toISOString()).toBe("2025-10-09T09:43:20.000Z");
    expect(objectIdTime("nope")).toBeNull();
    const shown = readable({ $oid: "68e783b8c0ffee0000000001" });
    expect(shown.text).toBe('ObjectId("68e783b8c0ffee0000000001")');
    expect(shown.title).toContain("2025-10-09T09:43:20.000Z UTC");
    expect(shown.title).toContain(localDateText(new Date("2025-10-09T09:43:20Z")));
  });

  it("shows dates in local time with UTC on hover", () => {
    const date = new Date("2026-02-02T15:01:00.250Z");
    const shown = readable({ $date: "2026-02-02T15:01:00.250Z" });
    expect(shown.text).toBe(localDateText(date));
    expect(shown.title).toMatch(/^2026-02-02T15:01:00\.250Z UTC · shown in local time \([+-]\d\d:\d\d\)$/);
    expect(localDateText(new Date(2026, 0, 5, 7, 8, 9))).toBe("2026-01-05 07:08:09");
    expect(localDateText(new Date(2026, 0, 5, 7, 8, 9, 42))).toBe("2026-01-05 07:08:09.042");
    expect(dateOf({ $numberLong: "0" })?.toISOString()).toBe("1970-01-01T00:00:00.000Z");
    expect(readable({ $date: { $numberLong: "99999999999999999" } }).text).toMatch(/^Date\(/);
  });

  it("reads numbers, binaries, regexes and timestamps", () => {
    expect(readable({ $numberDecimal: "19.990" })).toMatchObject({ text: "19.990", kind: "number", title: "Decimal128" });
    expect(readable({ $numberLong: "9007199254740993" })).toMatchObject({ text: "9007199254740993", title: "Int64 (Long)" });
    expect(uuidFromBase64(UUID_BASE64)).toBe(UUID);
    expect(readable({ $binary: { base64: UUID_BASE64, subType: "04" } }).text).toBe(`UUID("${UUID}")`);
    expect(readable({ $uuid: UUID }).text).toBe(`UUID("${UUID}")`);
    expect(readable({ $binary: { base64: "AQID", subType: "00" } })).toMatchObject({ text: "Binary(AQID)", title: "Binary subtype 0, 3 bytes" });
    expect(readable({ $regularExpression: { pattern: "^a", options: "i" } }).text).toBe("/^a/i");
    expect(readable({ $timestamp: { t: 1762000000, i: 3 } }).text).toBe("Timestamp(1762000000, 3)");
    expect(readable({ $minKey: 1 }).text).toBe("MinKey");
    expect(readable("x")).toMatchObject({ text: '"x"', kind: "string" });
  });
});

describe("mongosh syntax of a value", () => {
  it("writes each BSON type as mongosh does", () => {
    expect(shellText("it's")).toBe("'it\\'s'");
    expect(shellText({ $oid: "68e783b8c0ffee0000000001" })).toBe("ObjectId('68e783b8c0ffee0000000001')");
    expect(shellText({ $date: "2026-02-02T15:01:00Z" })).toBe("ISODate('2026-02-02T15:01:00.000Z')");
    expect(shellText({ $numberLong: "5" })).toBe("NumberLong('5')");
    expect(shellText({ $numberDecimal: "1.50" })).toBe("NumberDecimal('1.50')");
    expect(shellText({ $binary: { base64: UUID_BASE64, subType: "04" } })).toBe(`UUID('${UUID}')`);
    expect(shellText({ $binary: { base64: "AQ==", subType: "00" } })).toBe("BinData(0, 'AQ==')");
    expect(shellText({ $regularExpression: { pattern: "a/b", options: "i" } })).toBe("/a\\/b/i");
    expect(shellText({ $timestamp: { t: 1, i: 2 } })).toBe("Timestamp({ t: 1, i: 2 })");
    expect(shellText({ $numberDouble: "NaN" })).toBe("NaN");
    expect(shellText({ a: 1, "b-c": [true, null], $x: {} })).toBe("{ a: 1, 'b-c': [true, null], $x: {} }");
    expect(shellText({ a: { b: 1 } }, 2)).toBe("{\n  a: {\n    b: 1\n  }\n}");
  });

  it("is read back by the query bar's parser to the same Extended JSON", () => {
    const document = {
      _id: { $oid: "68e783b8c0ffee0000000001" },
      at: { $date: "2026-02-02T15:01:00.000Z" },
      n: { $numberLong: "9007199254740993" },
      price: { $numberDecimal: "19.99" },
      id: { $binary: { base64: UUID_BASE64, subType: "04" } },
      re: { $regularExpression: { pattern: "^a", options: "i" } },
      ts: { $timestamp: { t: 1, i: 2 } },
      name: 'say "hi"',
      tags: ["a", 2, false],
    };
    for (const indent of [0, 2]) {
      const parsed = parseMongoShell(shellText(document, indent), { expect: "document" });
      expect(parsed.ok).toBe(true);
      if (!parsed.ok) continue;
      const json = JSON.parse(parsed.json) as Record<string, unknown>;
      expect(json._id).toEqual(document._id);
      expect(json.price).toEqual(document.price);
      expect(json.n).toEqual(document.n);
      expect(json.re).toEqual(document.re);
      expect(json.name).toBe(document.name);
      expect(json.tags).toEqual(document.tags);
      expect(dateOf((json.at as { $date: unknown }).$date)?.toISOString()).toBe(document.at.$date);
    }
  });
});
