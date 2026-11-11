import { describe, expect, it } from "vitest";
import { buildLines, lineId, type TreeState } from "./treeLines";

const state = (expanded: string[] = [], limits: [string, number][] = []): TreeState => ({ expanded: new Set(expanded), limits: new Map(limits) });
const kinds = (lines: ReturnType<typeof buildLines>) =>
  lines.map((line) => (line.type === "field" ? `${"  ".repeat(line.depth)}${line.key}` : line.type === "more" ? `more ${line.remaining}` : line.type));

describe("document tree lines", () => {
  it("shows each document's fields folded, between a head and a foot", () => {
    const lines = buildLines([{ a: 1, b: { c: 2 } }, { d: [] }], state());
    expect(kinds(lines)).toEqual(["head", "a", "b", "foot", "head", "d", "foot"]);
    const b = lines[2];
    expect(b.type === "field" && b.container && !b.expanded).toBe(true);
    // An empty array cannot be opened.
    const d = lines[5];
    expect(d.type === "field" && d.container).toBe(false);
  });

  it("opens only the containers in the state, and wrappers are values", () => {
    const documents = [{ a: { b: { c: 1 }, at: { $date: "2026-01-01T00:00:00Z" } } }];
    const lines = buildLines(documents, state([lineId(0, ["a"])]));
    expect(kinds(lines)).toEqual(["head", "a", "  b", "  at", "foot"]);
    const at = lines[3];
    expect(at.type === "field" && at.container).toBe(false);
    expect(kinds(buildLines(documents, state([lineId(0, ["a"]), lineId(0, ["a", "b"])])))).toEqual(["head", "a", "  b", "    c", "  at", "foot"]);
  });

  it("bounds what a large container shows until more is asked for", () => {
    const big = { items: Array.from({ length: 5000 }, (_, index) => index) };
    const open = lineId(0, ["items"]);
    const lines = buildLines([big], state([open]), { topFields: 12, childItems: 200 });
    expect(lines).toHaveLength(1 + 1 + 200 + 1 + 1);
    const more = lines[lines.length - 2];
    expect(more).toMatchObject({ type: "more", id: open, remaining: 4800, array: true, depth: 1 });
    expect(buildLines([big], state([open], [[open, 1200]]), { topFields: 12, childItems: 200 })).toHaveLength(1 + 1 + 1200 + 1 + 1);
    const wide = Object.fromEntries(Array.from({ length: 30 }, (_, index) => [`f${index}`, index]));
    const top = buildLines([wide], state(), { topFields: 12, childItems: 200 });
    expect(top[top.length - 2]).toMatchObject({ type: "more", remaining: 18, array: false, depth: 0 });
  });
});
