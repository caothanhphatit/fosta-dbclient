import { describe, expect, it } from "vitest";
import { defaultLanguageSupport, formatJsonLike, jsonLanguageSupport, splitStatements, statementAt } from "./language";

const texts = (text: string, ranges: { from: number; to: number }[]) => ranges.map((range) => text.slice(range.from, range.to));

describe("splitStatements", () => {
  it("splits on semicolons and trims", () => {
    const text = "SELECT 1;\n  SELECT 2 ;\nSELECT 3";
    expect(texts(text, splitStatements(text))).toEqual(["SELECT 1", "SELECT 2", "SELECT 3"]);
  });

  it("ignores semicolons in strings, identifiers and comments", () => {
    const text = "SELECT ';', \"a;b\", `c;d` FROM t -- x; y\nWHERE a = 'it''s;'; /* one; two */ SELECT 2";
    expect(texts(text, splitStatements(text))).toEqual(["SELECT ';', \"a;b\", `c;d` FROM t -- x; y\nWHERE a = 'it''s;'", "/* one; two */ SELECT 2"]);
  });

  it("skips parts with only comments or whitespace", () => {
    const text = "SELECT 1;;  -- trailing note\n";
    expect(texts(text, splitStatements(text))).toEqual(["SELECT 1"]);
  });

  it("knows backslash escapes, hash comments and brackets when asked", () => {
    expect(texts("SELECT 'a\\';b'; SELECT 2", splitStatements("SELECT 'a\\';b'; SELECT 2"))).toEqual(["SELECT 'a\\';b'", "SELECT 2"]);
    const hash = "SELECT 1 # note; more\n; SELECT 2";
    expect(texts(hash, splitStatements(hash, { hashComments: true }))).toEqual(["SELECT 1 # note; more", "SELECT 2"]);
    const brackets = "SELECT [a;b] FROM t; SELECT 2";
    expect(texts(brackets, splitStatements(brackets, { bracketIdentifiers: true }))).toEqual(["SELECT [a;b] FROM t", "SELECT 2"]);
  });
});

describe("statementAt", () => {
  const text = "SELECT 1;\n\nSELECT 2;\nSELECT 3";
  const at = (offset: number) => {
    const range = statementAt(text, offset);
    return text.slice(range.from, range.to);
  };

  it("finds the statement under the cursor", () => {
    expect(at(0)).toBe("SELECT 1");
    expect(at(14)).toBe("SELECT 2");
    expect(at(text.length)).toBe("SELECT 3");
  });

  it("takes the statement before the cursor between statements", () => {
    expect(at(9)).toBe("SELECT 1");
    expect(at(10)).toBe("SELECT 1");
  });

  it("gives an empty range for empty text", () => {
    expect(statementAt("  ", 1)).toEqual({ from: 1, to: 1 });
  });

  it("is what the default language support uses", async () => {
    const support = defaultLanguageSupport();
    expect(support.statementAt("a; b", 3)).toEqual({ from: 3, to: 4 });
    expect(await support.editorExtensions({ language: { id: "x", label: "X", editor: "text", dialect: null, default: true, hint: "", fences: [] } })).toEqual(
      [],
    );
  });
});

describe("JSON command formatting", () => {
  it("re-indents without touching tokens", () => {
    expect(formatJsonLike('{"find":"products","filter":{"stock":0,"name":"a, b: {c}"},"limit":50}')).toBe(
      '{\n  "find": "products",\n  "filter": {\n    "stock": 0,\n    "name": "a, b: {c}"\n  },\n  "limit": 50\n}',
    );
  });

  it("keeps relaxed JSON, big numbers, empty containers and escaped quotes", () => {
    expect(formatJsonLike('{ find: "x", filter: { _id: ObjectId("65f0") }, n: 12345678901234567890, a: [], s: "q\\"x" }')).toBe(
      '{\n  find: "x",\n  filter: {\n    _id: ObjectId("65f0")\n  },\n  n: 12345678901234567890,\n  a: [],\n  s: "q\\"x"\n}',
    );
  });

  it("leaves text with comments alone", () => {
    const text = '{ "find": "x" } // all';
    expect(formatJsonLike(text)).toBe(text);
  });

  it("treats a JSON command as one statement", () => {
    const support = jsonLanguageSupport("mongo-command");
    expect(support.statementAt('  { "ping": 1 }\n', 3)).toEqual({ from: 2, to: 15 });
  });
});
