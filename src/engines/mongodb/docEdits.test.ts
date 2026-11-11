import { describe, expect, it, vi } from "vitest";
import type { RowChange } from "../../core/contract";
import { exactValue, PendingEdits, UNSET_VALUE } from "./docEdits";
import type { Doc } from "./documents";

const doc = (): Doc => ({
  _id: { $oid: "68e783b8c0ffee0000000001" },
  name: "Lamp",
  stock: 3,
  address: { city: "Oslo", zip: "0150" },
  tags: ["a", "b", "c"],
});

const update = (edits: PendingEdits): Extract<RowChange, { op: "update" }> => {
  const change = edits.changes()[0].change;
  if (change.op !== "update") throw new Error(change.op);
  return change;
};

describe("pending document edits", () => {
  it("keeps one edit per dotted path with the loaded value as original", () => {
    const changed = vi.fn();
    const edits = new PendingEdits(changed);
    const loaded = doc();
    edits.setValue(loaded, ["address", "city"], "Bergen");
    edits.setValue(loaded, ["stock"], 4, true);
    edits.removeField(loaded, ["address", "zip"]);
    expect(changed).toHaveBeenCalledTimes(3);
    expect(edits.count()).toBe(1);
    expect(edits.summary()).toEqual({ updated: 1, deleted: 0, inserted: 0, paths: 3 });
    const change = update(edits);
    expect(change.key).toEqual([{ column: "_id", value: exactValue({ $oid: "68e783b8c0ffee0000000001" }) }]);
    expect(change.values).toEqual([
      { column: "address.city", value: exactValue("Bergen") },
      { column: "stock", value: { t: "int", v: 4 } },
      { column: "address.zip", value: UNSET_VALUE },
    ]);
    expect(change.original).toEqual([
      { column: "address.city", value: exactValue("Oslo") },
      { column: "stock", value: exactValue(3) },
      { column: "address.zip", value: exactValue("0150") },
    ]);
    expect(edits.effective(loaded)).toEqual({ ...loaded, stock: 4, address: { city: "Bergen" } });
    expect(edits.pathState(loaded._id, ["address", "city"])).toBe("set");
    expect(edits.pathState(loaded._id, ["address"])).toBe("inside");
    expect(edits.pathState(loaded._id, ["name"])).toBeNull();
  });

  it("drops an edit set back to the loaded value, and a removed added field", () => {
    const edits = new PendingEdits();
    const loaded = doc();
    edits.setValue(loaded, ["name"], "Desk");
    edits.setValue(loaded, ["name"], "Lamp");
    expect(edits.isEmpty()).toBe(true);
    edits.addField(loaded, [], "color", "red");
    expect(edits.pathState(loaded._id, ["color"])).toBe("added");
    edits.removeField(loaded, ["color"]);
    expect(edits.isEmpty()).toBe(true);
    // A plain-number wrapper equals the number.
    edits.setValue(loaded, ["stock"], { $numberInt: "3" });
    expect(edits.isEmpty()).toBe(true);
  });

  it("edits inside a value already being set change that value; a container edit drops the edits inside", () => {
    const edits = new PendingEdits();
    const loaded = doc();
    edits.setValue(loaded, ["address"], { city: "Rome" });
    edits.setValue(loaded, ["address", "zip"], "00100");
    expect(update(edits).values).toEqual([{ column: "address", value: exactValue({ city: "Rome", zip: "00100" }) }]);
    edits.discardAll();
    edits.setValue(loaded, ["address", "city"], "Rome");
    edits.removeField(loaded, ["address"]);
    expect(update(edits).values).toEqual([{ column: "address", value: UNSET_VALUE }]);
  });

  it("changes array items by setting the array at its path", () => {
    const edits = new PendingEdits();
    const loaded = doc();
    edits.removeField(loaded, ["tags", "1"]);
    expect(update(edits).values).toEqual([{ column: "tags", value: exactValue(["a", "c"]) }]);
    edits.addField(loaded, ["tags"], "", "d");
    expect(update(edits).values).toEqual([{ column: "tags", value: exactValue(["a", "c", "d"]) }]);
    edits.discardAll();
    edits.setValue(loaded, ["tags", "0"], "z");
    expect(update(edits).values).toEqual([{ column: "tags.0", value: exactValue("z") }]);
  });

  it("renames a field as a removal and a new field", () => {
    const edits = new PendingEdits();
    const loaded = doc();
    edits.renameField(loaded, ["address", "zip"], "postcode");
    expect(update(edits).values).toEqual([
      { column: "address.zip", value: UNSET_VALUE },
      { column: "address.postcode", value: exactValue("0150") },
    ]);
    expect(() => edits.renameField(loaded, ["name"], "stock")).toThrow(/already has a field stock/);
    expect(() => edits.renameField(loaded, ["name"], "a.b")).toThrow(/dot/);
    // Renaming an added field keeps one edit.
    edits.discardAll();
    edits.addField(loaded, [], "color", "red");
    edits.renameField(loaded, ["color"], "colour");
    expect(update(edits).values).toEqual([{ column: "colour", value: exactValue("red") }]);
  });

  it("refuses _id, names that are not paths and existing fields", () => {
    const edits = new PendingEdits();
    const loaded = doc();
    expect(() => edits.setValue(loaded, ["_id"], 1)).toThrow(/_id/);
    expect(() => edits.removeField(loaded, ["_id"])).toThrow(/_id/);
    expect(() => edits.addField(loaded, [], "$x", 1)).toThrow(/\$/);
    expect(() => edits.addField(loaded, [], "name", 1)).toThrow(/already/);
    expect(() => edits.addField(loaded, ["name"], "x", 1)).toThrow(/documents and arrays/);
    expect(() => edits.setValue({ name: "no id" }, ["name"], "x")).toThrow(/without _id/);
    expect(edits.isEmpty()).toBe(true);
  });

  it("deletes, restores and inserts documents", () => {
    const edits = new PendingEdits();
    const loaded = doc();
    edits.setValue(loaded, ["name"], "Desk");
    edits.deleteDocument(loaded);
    expect(edits.isDeleted(loaded._id)).toBe(true);
    expect(() => edits.setValue(loaded, ["name"], "x")).toThrow(/deleted/);
    const insert = edits.insertDocument({ name: "New", n: { $numberLong: "5" } });
    const built = edits.changes();
    expect(built.map((item) => item.change.op)).toEqual(["insert", "delete"]);
    expect(built[0]).toEqual({
      insert,
      change: {
        op: "insert",
        values: [
          { column: "name", value: exactValue("New") },
          { column: "n", value: exactValue({ $numberLong: "5" }) },
        ],
      },
    });
    const deletion = built[1].change;
    expect(deletion.op === "delete" && deletion.original.map((field) => field.column)).toEqual(["_id", "name", "stock", "address", "tags"]);
    expect(edits.summary()).toEqual({ updated: 0, deleted: 1, inserted: 1, paths: 0 });
    edits.discardDocument(loaded._id);
    edits.removeInsert(insert);
    expect(edits.isEmpty()).toBe(true);
  });

  it("keeps the copy loaded at the first edit as the base", () => {
    const edits = new PendingEdits();
    edits.setValue(doc(), ["name"], "Desk");
    const reloaded = { ...doc(), name: "Changed elsewhere", stock: 9 };
    expect(edits.effective(reloaded)).toEqual({ ...doc(), name: "Desk" });
    expect(update(edits).original).toEqual([{ column: "name", value: exactValue("Lamp") }]);
  });
});
