import { describe, expect, it } from "vitest";
import { diffDocuments, diffSummary, entryText } from "./docDiff";

describe("document diff", () => {
  it("names nested changes by dotted path", () => {
    const before = { _id: 1, name: "Lamp", address: { city: "Oslo", zip: "0150" }, tags: ["a", "b"], old: true };
    const after = { _id: 1, name: "Lamp", address: { city: "Bergen", zip: "0150" }, tags: ["a", "c"], color: "red" };
    const diff = diffDocuments(before, after);
    expect(diff).toEqual([
      { path: "address.city", kind: "changed", before: "Oslo", after: "Bergen" },
      { path: "tags.1", kind: "changed", before: "b", after: "c" },
      { path: "old", kind: "removed", before: true },
      { path: "color", kind: "added", after: "red" },
    ]);
    expect(diff.map(entryText)).toEqual([`~ address.city: 'Oslo' → 'Bergen'`, `~ tags.1: 'b' → 'c'`, "- old: true", `+ color: 'red'`]);
    expect(diffSummary(diff)).toBe("2 changed, 1 added, 1 removed");
  });

  it("reads wrappers as values and arrays of another length as one change", () => {
    expect(diffDocuments({ at: { $date: "2026-01-01T00:00:00Z" } }, { at: { $date: "2026-01-02T00:00:00Z" } })).toEqual([
      { path: "at", kind: "changed", before: { $date: "2026-01-01T00:00:00Z" }, after: { $date: "2026-01-02T00:00:00Z" } },
    ]);
    expect(diffDocuments({ tags: ["a"] }, { tags: ["a", "b"] })).toEqual([{ path: "tags", kind: "changed", before: ["a"], after: ["a", "b"] }]);
    expect(diffDocuments({ n: 5 }, { n: { $numberInt: "5" } })).toEqual([]);
    expect(diffDocuments({ n: 5 }, { n: { $numberLong: "5" } })).toHaveLength(1);
    expect(diffSummary([])).toBe("");
  });
});
