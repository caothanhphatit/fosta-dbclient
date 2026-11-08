import { describe, expect, it } from "vitest";
import { blocks, bodyContext, consoleCompletions, consoleStatementAt, formatConsole, indexOfPath, keysFor, type ConsoleNames } from "./console";

const names: ConsoleNames = {
  indices: ["products", "logs-app"],
  fields: (index) => (index === "products" ? ["name", "price", "brand"] : null),
};

const labels = (text: string, pos = text.length, explicit = false) =>
  consoleCompletions(text, pos, names, explicit)?.options.map((option) => option.label) ?? null;

describe("console blocks", () => {
  const text = '# comment\nGET /products/_search\n{\n  "size": 1\n}\n\n\nPOST _bulk\n{ "index": {} }\n{ "a": 1 }\n';

  it("split at request lines and cut trailing blank lines", () => {
    const list = blocks(text);
    expect(list.map((block) => [block.method, block.path])).toEqual([
      ["GET", "/products/_search"],
      ["POST", "_bulk"],
    ]);
    expect(text.slice(list[0].from, list[0].to)).toBe('GET /products/_search\n{\n  "size": 1\n}');
  });

  it("give the request under the cursor to Run current statement", () => {
    const inBody = text.indexOf('"size"');
    const range = consoleStatementAt(text, inBody);
    expect(text.slice(range.from, range.to).startsWith("GET /products/_search")).toBe(true);
    const second = consoleStatementAt(text, text.indexOf('"a"'));
    expect(text.slice(second.from, second.to)).toBe('POST _bulk\n{ "index": {} }\n{ "a": 1 }');
    // Before the first request (the comment line), the first request.
    expect(consoleStatementAt(text, 2).from).toBe(text.indexOf("GET"));
    expect(consoleStatementAt("", 0)).toEqual({ from: 0, to: 0 });
  });

  it("format JSON bodies and keep NDJSON one value per line", () => {
    const formatted = formatConsole('GET /p/_search\n{"query":{"match_all":{}}}\n\nPOST _bulk\n{"index":{}}\n  {"a":1}\n');
    expect(formatted).toBe('GET /p/_search\n{\n  "query": {\n    "match_all": {}\n  }\n}\n\nPOST _bulk\n{"index":{}}\n{"a":1}\n');
    const raw = 'GET /p/_search\n{ "query": { "match": { "a": """x""" } } }';
    expect(formatConsole(raw)).toBe(raw);
    expect(formatConsole("GET /p/_search\n{ broken")).toBe("GET /p/_search\n{ broken");
  });

  it("find the target index of a path", () => {
    expect(indexOfPath("/products/_search?q=x")).toBe("products");
    expect(indexOfPath("_cat/indices")).toBeNull();
    expect(indexOfPath("/logs%2Dapp/_count")).toBe("logs-app");
  });
});

describe("console completion", () => {
  it("offers methods at the start of a line", () => {
    expect(labels("G")).toEqual(["GET", "POST", "PUT", "DELETE", "HEAD"]);
    expect(labels("", 0, true)).toContain("DELETE");
  });

  it("offers index names and endpoints in the path", () => {
    const root = labels("GET /pro");
    expect(root).toContain("products");
    expect(root).toContain("_cat/indices?v");
    const answer = consoleCompletions("GET /products/_se", 17, names);
    expect(answer?.from).toBe(14);
    expect(answer?.options.map((option) => option.label)).toContain("_search");
    expect(labels("GET /products/_doc/1")).toBeNull();
  });

  it("offers Query DSL keys by context", () => {
    expect(labels('GET /products/_search\n{\n  "')).toContain("query");
    expect(labels('GET /products/_search\n{ "query": { "')).toContain("bool");
    expect(labels('GET /products/_search\n{ "query": { "bool": { "')).toEqual(["must", "filter", "should", "must_not", "minimum_should_match", "boost"]);
    expect(labels('GET /products/_search\n{ "query": { "bool": { "filter": [ { "')).toContain("range");
    expect(labels('GET /products/_search\n{ "query": { "term": { "')).toEqual(["name", "price", "brand"]);
    expect(labels('GET /products/_search\n{ "query": { "range": { "price": { "')).toContain("gte");
    expect(labels('GET /products/_search\n{ "aggs": { "by_brand": { "')).toContain("terms");
    expect(labels('GET /products/_search\n{ "aggs": { "by_brand": { "terms": { "field": "')).toEqual(["name", "price", "brand"]);
    // A value is not a key.
    expect(labels('GET /products/_search\n{ "size": ')).toBeNull();
  });

  it("quotes keys typed without a quote", () => {
    const answer = consoleCompletions("GET /products/_search\n{ qu", "GET /products/_search\n{ qu".length, names);
    expect(answer?.options.find((option) => option.label === "query")?.apply).toBe('"query"');
  });

  it("tracks the path through nested objects and arrays", () => {
    const text = '{ "query": { "bool": { "must": [ { "match": { "name": "x" } }, { "';
    expect(bodyContext(text, 0, text.length).path).toEqual(["", "query", "bool", "must", "[]"]);
    expect(keysFor(["", "query", "bool", "must", "[]"], []).detail).toBe("query");
  });
});
