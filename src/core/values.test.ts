import { describe, expect, it } from "vitest";
import type { Value, ValueKind } from "./contract";
import { displayValue, formatFloat, parseInput, valueToJson } from "./values";
import fixture from "./__fixtures__/contract/values.json";

const k = (kind: ValueKind["kind"]): ValueKind => ({ kind }) as ValueKind;

describe("displayValue", () => {
  it("follows Rust Value::display()", () => {
    expect(displayValue({ t: "null", v: null })).toBeNull();
    expect(displayValue({ t: "bool", v: true })).toBe("true");
    expect(displayValue({ t: "int", v: "9223372036854775807" })).toBe("9223372036854775807");
    expect(displayValue({ t: "float", v: 1.5 })).toBe("1.5");
    expect(displayValue({ t: "float", v: "NaN" })).toBe("NaN");
    expect(displayValue({ t: "bytes", v: "0xdead" })).toBe("0xdead");
    expect(displayValue({ t: "timestamp", v: "2026-02-28 13:45:30.500" })).toBe("2026-02-28 13:45:30.500");
    expect(
      displayValue({
        t: "array",
        v: [
          { t: "int", v: 1 },
          { t: "null", v: null },
          { t: "text", v: 'a"b' },
        ],
      }),
    ).toBe('[1, null, "a\\"b"]');
    expect(
      displayValue({
        t: "doc",
        v: [
          ["_id", { t: "objectId", v: "65f1c0ffee00000000000001" }],
          ["n", { t: "int", v: 2 }],
          ["tags", { t: "json", v: '["a"]' }],
        ],
      }),
    ).toBe('{"_id":{"$oid":"65f1c0ffee00000000000001"},"n":2,"tags":["a"]}');
    expect(displayValue({ t: "other", type: "inet", v: "10.0.0.1" })).toBe("10.0.0.1");
  });

  it("writes floats like Rust (no exponent)", () => {
    expect(formatFloat(1e21)).toBe("1000000000000000000000");
    expect(formatFloat(1.5e-7)).toBe("0.00000015");
    expect(formatFloat(-Infinity)).toBe("-Infinity");
    expect(formatFloat(3)).toBe("3");
  });

  it("displays every fixture value", () => {
    for (const value of fixture as Value[]) {
      const shown = displayValue(value);
      if (value.t === "null") expect(shown).toBeNull();
      else expect(typeof shown).toBe("string");
    }
  });

  it("turns documents into relaxed JSON", () => {
    expect(valueToJson({ t: "decimal", v: "12.50" })).toBe("12.50");
    expect(valueToJson({ t: "array", v: [{ t: "bool", v: false }] })).toEqual([false]);
  });
});

describe("parseInput", () => {
  const ok = (kind: ValueKind, text: string) => {
    const value = parseInput(kind, text);
    if (value instanceof Error) throw value;
    return value;
  };
  const fails = (kind: ValueKind, text: string) => expect(parseInput(kind, text)).toBeInstanceOf(Error);

  it("keeps text as typed and refuses empty non-text input", () => {
    expect(ok(k("text"), "  hi ")).toEqual({ t: "text", v: "  hi " });
    expect(ok(k("text"), "")).toEqual({ t: "text", v: "" });
    fails(k("int"), "");
    fails(k("date"), "");
  });

  it("parses booleans and numbers", () => {
    expect(ok(k("bool"), "Yes")).toEqual({ t: "bool", v: true });
    expect(ok(k("bool"), "off")).toEqual({ t: "bool", v: false });
    fails(k("bool"), "maybe");
    expect(ok(k("int"), "+42")).toEqual({ t: "int", v: 42 });
    expect(ok(k("int"), "9007199254740993")).toEqual({ t: "int", v: "9007199254740993" });
    expect(ok(k("int"), "99999999999999999999")).toEqual({ t: "decimal", v: "99999999999999999999" });
    fails(k("int"), "1.5");
    expect(ok(k("float"), "1e3")).toEqual({ t: "float", v: 1000 });
    expect(ok(k("float"), "-inf")).toEqual({ t: "float", v: "-Infinity" });
    fails(k("float"), "0x10");
    expect(ok(k("decimal"), "+12.50")).toEqual({ t: "decimal", v: "12.50" });
    fails(k("decimal"), "12,5");
  });

  it("parses temporals to their display form", () => {
    expect(ok(k("date"), "2026-2-8")).toEqual({ t: "date", v: "2026-02-08" });
    fails(k("date"), "2026-02-30");
    expect(ok(k("time"), "09:30")).toEqual({ t: "time", v: "09:30:00" });
    expect(ok(k("time"), "09:30:00.25")).toEqual({ t: "time", v: "09:30:00.250" });
    expect(ok(k("timestamp"), "2026-11-08T09:30")).toEqual({ t: "timestamp", v: "2026-11-08 09:30:00" });
    expect(ok(k("timestamp"), "2026-11-08")).toEqual({ t: "timestamp", v: "2026-11-08 00:00:00" });
    expect(ok(k("timestampTz"), "2026-11-08T09:30:00Z")).toEqual({ t: "timestamptz", v: "2026-11-08 09:30:00+00:00" });
    expect(ok(k("timestampTz"), "2026-11-08 09:30:00.5+0530")).toEqual({ t: "timestamptz", v: "2026-11-08 09:30:00.500+05:30" });
    expect(ok(k("timestampTz"), "2026-11-08 09:30")).toEqual({ t: "timestamptz", v: "2026-11-08 09:30:00+00:00" });
  });

  it("parses identifiers, bytes and JSON", () => {
    expect(ok(k("uuid"), "{A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11}")).toEqual({ t: "uuid", v: "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" });
    fails(k("uuid"), "abc");
    expect(ok(k("objectId"), 'ObjectId("65F1C0FFEE00000000000001")')).toEqual({ t: "objectId", v: "65f1c0ffee00000000000001" });
    expect(ok(k("bytes"), "0xDEAD")).toEqual({ t: "bytes", v: "0xdead" });
    expect(ok(k("bytes"), "hi")).toEqual({ t: "bytes", v: "0x6869" });
    expect(ok(k("json"), " [1, 2] ")).toEqual({ t: "json", v: "[1, 2]" });
    fails(k("json"), "{nope");
    fails(k("document"), "[1]");
    expect(ok({ kind: "array", of: { kind: "int" } }, '[1, null, "3"]')).toEqual({
      t: "array",
      v: [
        { t: "int", v: 1 },
        { t: "null", v: null },
        { t: "int", v: 3 },
      ],
    });
    fails({ kind: "array", of: { kind: "int" } }, '[1, "x"]');
  });
});
