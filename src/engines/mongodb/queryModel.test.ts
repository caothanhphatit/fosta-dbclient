import { describe, expect, it } from "vitest";
import {
  braced,
  buildQuery,
  commandText,
  EMPTY_QUERY,
  errorRange,
  firstCursor,
  nextCursor,
  oneLine,
  pageArgs,
  parseCommandText,
  rangeText,
  readSavedState,
  rememberFilter,
  uniqueFilters,
  type QueryState,
} from "./queryModel";

const TARGET = { database: "shop", collection: "orders" };
const state = (over: Partial<QueryState>): QueryState => ({ ...EMPTY_QUERY, ...over });

describe("query bar state to find arguments", () => {
  it("sends nothing for empty fields and an estimated count", () => {
    const built = buildQuery(EMPTY_QUERY, TARGET);
    expect(built).toEqual({
      ok: true,
      query: {
        find: { database: "shop", collection: "orders", limit: 20 },
        count: { database: "shop", collection: "orders", estimated: true },
        filtered: false,
      },
    });
    const empty = buildQuery(state({ filter: " {} " }), TARGET);
    expect(empty.ok && empty.query.filtered).toBe(false);
  });

  it("parses shell syntax to Extended JSON, braces optional", () => {
    const built = buildQuery(
      state({ filter: "status: 'paid', _id: ObjectId('68e783b8c0ffee0000000001')", project: "{ total: 1 }", sort: "{ total: -1 }", skip: 40, limit: 5 }),
      TARGET,
    );
    if (!built.ok) throw new Error(built.error.message);
    expect(built.query.find).toEqual({
      ...TARGET,
      filter: '{"status":"paid","_id":{"$oid":"68e783b8c0ffee0000000001"}}',
      projection: '{"total":1}',
      sort: '{"total":-1}',
      skip: 40,
      limit: 5,
    });
    expect(built.query.count).toEqual({ ...TARGET, filter: '{"status":"paid","_id":{"$oid":"68e783b8c0ffee0000000001"}}', estimated: false });
    expect(built.query.filtered).toBe(true);
  });

  it("passes the collation to find and count", () => {
    const built = buildQuery(state({ filter: "{ name: 'a' }", collation: "{ locale: 'en', strength: 2 }" }), TARGET);
    expect(built.ok && built.query.find.collation).toBe('{"locale":"en","strength":2}');
    expect(built.ok && built.query.count.collation).toBe('{"locale":"en","strength":2}');
  });

  it("reports the first field that does not parse, at a position in the typed text", () => {
    const braceless = buildQuery(state({ filter: "status: paid" }), TARGET);
    expect(braceless).toMatchObject({ ok: false, error: { field: "filter", position: 8 } });
    const sort = buildQuery(state({ sort: "{ total: -1, }}" }), TARGET);
    expect(sort).toMatchObject({ ok: false, error: { field: "sort" } });
    const js = buildQuery(state({ filter: "{ $where: 'this.a > 1' }" }), TARGET);
    expect(js).toMatchObject({ ok: false, error: { field: "filter", position: 2 } });
  });

  it("clamps limit and skip", () => {
    const built = buildQuery(state({ limit: 5000, skip: -3 }), TARGET);
    expect(built.ok && built.query.find).toEqual({ ...TARGET, limit: 1000 });
    const zero = buildQuery(state({ limit: 0 }), TARGET);
    expect(zero.ok && zero.query.find.limit).toBe(20);
  });

  it("underlines the word at an error, or the end", () => {
    expect(errorRange("{ a: paid }", 5)).toEqual({ from: 5, to: 9 });
    expect(errorRange("{ a: , }", 5)).toEqual({ from: 5, to: 6 });
    expect(errorRange("{ a: 1", 6)).toEqual({ from: 6, to: 6 });
    expect(braced("a: 1")).toEqual({ text: "{a: 1}", shift: 1 });
  });
});

describe("history command text", () => {
  it("writes one line and reads it back", () => {
    const s = state({ filter: "status: 'paid',\n  total: { $gt: 5 }", project: "{ total: 1 }", sort: "{ total: -1 }", skip: 10, limit: 50 });
    const text = commandText(s, "orders");
    expect(text).toBe(`db.getCollection("orders").find({status: 'paid', total: { $gt: 5 }}, { total: 1 }).sort({ total: -1 }).skip(10).limit(50)`);
    expect(parseCommandText(text, "orders")).toEqual({ ...s, filter: "{status: 'paid', total: { $gt: 5 }}" });
    expect(commandText(EMPTY_QUERY, 'a"b')).toBe('db.getCollection("a\\"b").find({}).limit(20)');
    expect(parseCommandText(commandText(EMPTY_QUERY, "orders"), "orders")).toEqual(EMPTY_QUERY);
  });

  it("keeps line breaks inside strings and ignores commas and parens in them", () => {
    expect(oneLine("{ a: 'x\\n y',\n  b: 1 }")).toBe("{ a: 'x\\n y', b: 1 }");
    const text = 'db.getCollection("orders").find({ note: "a, (b)" }).collation({ locale: "en" }).limit(20)';
    expect(parseCommandText(text, "orders")).toMatchObject({ filter: '{ note: "a, (b)" }', collation: '{ locale: "en" }' });
  });

  it("refuses commands of other collections or shapes", () => {
    expect(parseCommandText('db.getCollection("customers").find({}).limit(20)', "orders")).toBeNull();
    expect(parseCommandText('db.getCollection("orders").find({}).explain()', "orders")).toBeNull();
    expect(parseCommandText('db.getCollection("orders").find({', "orders")).toBeNull();
  });
});

describe("saved state and recent filters", () => {
  it("reads saved state defensively", () => {
    expect(readSavedState(undefined)).toEqual({ ...EMPTY_QUERY, mode: "list", options: false, recent: [] });
    expect(readSavedState({ filter: "a: 1", mode: "json", limit: "7", options: true, recent: ["x", 3], skip: 2.7 })).toEqual({
      ...EMPTY_QUERY,
      filter: "a: 1",
      mode: "json",
      limit: 7,
      skip: 2,
      options: true,
      recent: ["x"],
    });
    expect(readSavedState({ mode: "table", filter: "x".repeat(5000) })).toMatchObject({ mode: "list", filter: "" });
  });

  it("keeps recent filters newest first without repeats", () => {
    let recent: string[] = [];
    recent = rememberFilter(recent, "a: 1");
    recent = rememberFilter(recent, "b: 2");
    recent = rememberFilter(recent, " a: 1 ");
    recent = rememberFilter(recent, "{}");
    expect(recent).toEqual(["a: 1", "b: 2"]);
    expect(uniqueFilters(["a: 1", "{a: 1}", "{ a:1 }", "b: 2"])).toEqual(["a: 1", "b: 2"]);
  });
});

describe("paging", () => {
  const page = (n: number, extra: Partial<{ hasMore: boolean; keyset: boolean; nextAfter: unknown }>) => ({
    documents: Array.from({ length: n }, (_, i) => ({ i })),
    hasMore: true,
    keyset: false,
    nextAfter: null,
    ...extra,
  });

  it("pages by _id when the backend gives nextAfter", () => {
    const first = firstCursor({ skip: 5 });
    expect(first).toEqual({ after: null, skip: 5, offset: 5 });
    const second = nextCursor(first, page(20, { keyset: true, nextAfter: { $oid: "x" } }));
    expect(second).toEqual({ after: { $oid: "x" }, skip: 0, offset: 25 });
    const find = { ...TARGET, limit: 20, skip: 5 };
    expect(pageArgs(find, first)).toEqual(find);
    expect(pageArgs(find, second!)).toEqual({ ...TARGET, limit: 20, after: { $oid: "x" } });
  });

  it("pages by skip otherwise, and stops at the last page", () => {
    const first = firstCursor({ skip: 0 });
    const second = nextCursor(first, page(20, {}));
    expect(second).toEqual({ after: null, skip: 20, offset: 20 });
    expect(pageArgs({ ...TARGET, limit: 20 }, second!)).toEqual({ ...TARGET, limit: 20, skip: 20 });
    expect(nextCursor(second!, page(3, { hasMore: false }))).toBeNull();
  });

  it("describes the range shown", () => {
    const f = (n: number) => n.toLocaleString("en-US");
    expect(rangeText(0, 20, 1234, false, f)).toBe("1–20 of 1,234");
    expect(rangeText(20, 20, 5000, true, f)).toBe("21–40 of ~5,000");
    expect(rangeText(0, 3, null, false, f)).toBe("1–3");
    expect(rangeText(0, 0, 0, false, f)).toBe("No documents");
  });
});
