import { describe, expect, it } from "vitest";
import { detectDelimiter, parseCsv } from "./csv";

describe("parseCsv", () => {
  it("splits records and fields", () => {
    expect(parseCsv("a,b,c\n1,,3\n", ",")).toEqual([
      ["a", "b", "c"],
      ["1", "", "3"],
    ]);
  });

  it("handles CRLF line endings and a last line without a newline", () => {
    expect(parseCsv("a;b\r\n1;2", ";")).toEqual([
      ["a", "b"],
      ["1", "2"],
    ]);
  });

  it("keeps separators, newlines and doubled quotes inside quoted fields", () => {
    expect(parseCsv('id,note\n1,"x, y"\n2,"say ""hi"""\n3,"line 1\nline 2"', ",")).toEqual([
      ["id", "note"],
      ["1", "x, y"],
      ["2", 'say "hi"'],
      ["3", "line 1\nline 2"],
    ]);
  });

  it("treats a quote in the middle of a field as text", () => {
    expect(parseCsv('5" screen,ok', ",")).toEqual([['5" screen', "ok"]]);
  });

  it("returns no records for empty input", () => {
    expect(parseCsv("", ",")).toEqual([]);
  });
});

describe("detectDelimiter", () => {
  it("picks the separator that yields the most columns", () => {
    expect(detectDelimiter("a;b;c\n1;2;3")).toBe(";");
    expect(detectDelimiter("a\tb\n1\t2")).toBe("\t");
    expect(detectDelimiter("a|b|c|d\n1|2|3|4")).toBe("|");
    expect(detectDelimiter("name,city\nAnn,Hanoi")).toBe(",");
  });
});
