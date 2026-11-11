import { describe, expect, it } from "vitest";
import { anchorAt, documentAtLine, idKey, indexOfAnchor, jsonDocumentLines } from "./anchor";

const oid = (n: number) => ({ _id: { $oid: `68e783b8c0ffee00000000${String(n).padStart(2, "0")}` }, n });

describe("the document kept at the top across modes", () => {
  it("finds the anchored document again by _id", () => {
    const page = [oid(1), oid(2), oid(3), oid(4)];
    const anchor = anchorAt(page, 2);
    expect(anchor).toEqual({ id: idKey(page[2]), index: 2 });
    expect(indexOfAnchor(page, anchor)).toBe(2);
    // The same page read again (another order of keys in the id) or reordered.
    const reread = [oid(4), oid(3), oid(2), oid(1)];
    expect(indexOfAnchor(reread, anchor)).toBe(1);
    expect(idKey({ _id: { b: 1, a: 2 } })).toBe(idKey({ _id: { a: 2, b: 1 } }));
  });

  it("falls back to the same index, clamped, without an _id or when it is gone", () => {
    expect(indexOfAnchor([{ a: 1 }, { a: 2 }, { a: 3 }], anchorAt([{ a: 1 }, { a: 2 }, { a: 3 }], 1))).toBe(1);
    expect(indexOfAnchor([oid(7), oid(8)], { id: idKey(oid(1)), index: 5 })).toBe(1);
    expect(indexOfAnchor([oid(1)], null)).toBe(0);
    expect(anchorAt([], 0)).toBeNull();
    expect(anchorAt([oid(1)], 9)).toEqual({ id: idKey(oid(1)), index: 0 });
  });

  it("maps the JSON text's lines to documents", () => {
    const page = [{ a: 1 }, { b: { c: [1, 2] } }, {}];
    const starts = jsonDocumentLines(page);
    const lines = JSON.stringify(page, null, 2).split("\n");
    for (const [index, start] of starts.entries()) {
      expect(lines[start].trim().startsWith("{")).toBe(true);
      expect(documentAtLine(starts, start)).toBe(index);
    }
    expect(starts).toEqual([1, 4, 12]);
    expect(documentAtLine(starts, 0)).toBe(0);
    expect(documentAtLine(starts, 7)).toBe(1);
    expect(documentAtLine(starts, 99)).toBe(2);
    expect(documentAtLine([], 3)).toBe(-1);
  });
});
