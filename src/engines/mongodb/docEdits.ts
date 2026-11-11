/**
 * Pending document edits of the Documents section, without DOM: what the user changed in the List and Table modes,
 * kept per document (by `_id`) and per dotted path until Save or Discard.
 *
 * - An edit is an operation on one path: set a value or remove the field. Paths never overlap within a document: an
 *   edit inside a value that is already being set changes that value; an edit of a container drops the edits inside
 *   it. Changes inside arrays (removing an item, adding one) set the array at its path, since `$unset` of an item
 *   would leave a null.
 * - A value set back to what was loaded is no longer an edit. Removing a field that was only added drops the edit.
 * - Deleted documents and inserted ones (new, or a duplicate without its `_id`) are kept as such.
 * - `changes()` builds the `apply_edits` changes: updates by dotted path with exact Extended JSON values
 *   (`{ t: "other", type: "bson" }`, written as given) or, for "Number", plain numbers the backend keeps in the stored
 *   numeric type; removed fields as `{ type: "unset" }`; the loaded value of every edited path as `original`, so a save
 *   conflicts only when someone changed one of those paths.
 */
import type { Field, RowChange, Value } from "../../core/contract";
import { isWrapper, type Doc, type Json } from "./documents";

export type EditOp = { kind: "set"; value: Json; keepType: boolean } | { kind: "unset" };

interface DocState {
  id: Json;
  original: Doc;
  ops: Map<string, EditOp>;
  deleted: boolean;
}

export interface PendingInsert {
  /** A local id for the inserted document (it has no `_id` yet, or the one the user gave). */
  id: string;
  document: Doc;
}

/** One change sent, with what it came from (for the conflict dialog). */
export interface BuiltChange {
  change: RowChange;
  /** The `_id` of an updated or deleted document. */
  id?: Json;
  /** The local id of an insert. */
  insert?: string;
}

export interface EditSummary {
  updated: number;
  deleted: number;
  inserted: number;
  /** Edited paths over all updated documents. */
  paths: number;
}

const SEP = ".";

export const exactValue = (value: Json): Value => ({ t: "other", type: "bson", v: JSON.stringify(value ?? null) });
export const UNSET_VALUE: Value = { t: "other", type: "unset", v: "" };

/** The key a document is kept under: its `_id` as loaded. */
export function idKey(id: Json): string {
  return JSON.stringify(id ?? null);
}

const isDoc = (value: Json): value is Doc => typeof value === "object" && value !== null && !Array.isArray(value) && !isWrapper(value);

/** Why a field name cannot be edited here, or null: names with a dot or starting with `$` are not paths. */
export function fieldNameProblem(name: string): string | null {
  if (!name) return "Enter a field name.";
  if (name.includes(".")) return "A field name cannot contain a dot here (it would be read as a path).";
  if (name.startsWith("$")) return "A field name cannot start with $.";
  if (name.includes("\0")) return "A field name cannot contain a NUL character.";
  return null;
}

/** Whether a path can be edited: every name a plain field name (or an array position). */
export function editablePath(path: readonly string[]): boolean {
  return path.length > 0 && path.every((part) => fieldNameProblem(part) === null) && !(path[0] === "_id");
}

export function getAt(value: Json, path: readonly string[]): { found: boolean; value: Json } {
  let current: Json = value;
  for (const part of path) {
    if (Array.isArray(current)) {
      const index = Number(part);
      if (!Number.isInteger(index) || index < 0 || index >= current.length) return { found: false, value: undefined };
      current = current[index];
    } else if (isDoc(current) && Object.hasOwn(current, part)) {
      current = current[part];
    } else {
      return { found: false, value: undefined };
    }
  }
  return { found: true, value: current };
}

/** `value` with `path` set (copied along the path; missing documents on the way are created). */
export function setAt(value: Json, path: readonly string[], next: Json): Json {
  if (path.length === 0) return next;
  const [head, ...rest] = path;
  if (Array.isArray(value)) {
    const index = Number(head);
    if (!Number.isInteger(index) || index < 0 || index > value.length) throw new Error(`No item ${head} in the array`);
    const copy = value.slice();
    copy[index] = setAt(value[index], rest, next);
    return copy;
  }
  const base: Doc = isDoc(value) ? value : {};
  return { ...base, [head]: setAt(base[head], rest, next) };
}

/** `value` without `path` (an array item is spliced out). */
export function removeAt(value: Json, path: readonly string[]): Json {
  if (path.length === 0) return value;
  const [head, ...rest] = path;
  if (Array.isArray(value)) {
    const index = Number(head);
    if (!Number.isInteger(index) || index < 0 || index >= value.length) return value;
    if (rest.length === 0) return value.filter((_, at) => at !== index);
    const copy = value.slice();
    copy[index] = removeAt(value[index], rest);
    return copy;
  }
  if (!isDoc(value) || !Object.hasOwn(value, head)) return value;
  if (rest.length === 0) {
    const copy = { ...value };
    delete copy[head];
    return copy;
  }
  return { ...value, [head]: removeAt(value[head], rest) };
}

/** Plain-number wrappers (`$numberInt`, finite `$numberDouble`) read as numbers; anything else as it is. */
function plainNumber(value: Json): Json {
  if (isDoc(value) || !isWrapper(value)) return value;
  const wrapper = value as Doc;
  const text = wrapper.$numberInt ?? wrapper.$numberDouble;
  if (typeof text === "string" && Object.keys(wrapper).length === 1 && Number.isFinite(Number(text))) return Number(text);
  return value;
}

/** Deep equality of Extended JSON values (key order ignored, plain-number wrappers equal to the number). */
export function sameJson(a: Json, b: Json): boolean {
  const x = plainNumber(a);
  const y = plainNumber(b);
  if (x === y) return true;
  if (typeof x !== "object" || typeof y !== "object" || x === null || y === null) return false;
  if (Array.isArray(x) !== Array.isArray(y)) return false;
  if (Array.isArray(x) && Array.isArray(y)) return x.length === y.length && x.every((item, index) => sameJson(item, y[index]));
  const keysX = Object.keys(x as Doc);
  const keysY = Object.keys(y as Doc);
  return keysX.length === keysY.length && keysX.every((key) => Object.hasOwn(y as Doc, key) && sameJson((x as Doc)[key], (y as Doc)[key]));
}

const joined = (path: readonly string[]) => path.join(SEP);
const split = (path: string) => path.split(SEP);
const inside = (outer: string, inner: string) => inner.length > outer.length && inner.startsWith(outer) && inner[outer.length] === SEP;

export class PendingEdits {
  private readonly docs = new Map<string, DocState>();
  private inserts: PendingInsert[] = [];
  private nextInsert = 1;

  constructor(private readonly changed: () => void = () => undefined) {}

  /** Documents with a change, plus inserts. */
  count(): number {
    let n = this.inserts.length;
    for (const state of this.docs.values()) if (state.deleted || state.ops.size > 0) n++;
    return n;
  }

  isEmpty(): boolean {
    return this.count() === 0;
  }

  summary(): EditSummary {
    const summary: EditSummary = { updated: 0, deleted: 0, inserted: this.inserts.length, paths: 0 };
    for (const state of this.docs.values()) {
      if (state.deleted) summary.deleted++;
      else if (state.ops.size > 0) {
        summary.updated++;
        summary.paths += state.ops.size;
      }
    }
    return summary;
  }

  /** The edits of a document by dotted path (empty when none). */
  opsOf(id: Json): ReadonlyMap<string, EditOp> {
    return this.docs.get(idKey(id))?.ops ?? new Map();
  }

  isDeleted(id: Json): boolean {
    return this.docs.get(idKey(id))?.deleted ?? false;
  }

  /** The document as loaded when it was first edited (what a save compares with). */
  originalOf(id: Json): Doc | null {
    return this.docs.get(idKey(id))?.original ?? null;
  }

  insertsList(): readonly PendingInsert[] {
    return this.inserts;
  }

  /** The edited documents (updated or deleted), with their originals. */
  editedDocuments(): { id: Json; original: Doc; deleted: boolean }[] {
    return [...this.docs.values()].filter((state) => state.deleted || state.ops.size > 0).map(({ id, original, deleted }) => ({ id, original, deleted }));
  }

  /** `document` with its pending edits (the loaded copy kept from the first edit is the base). */
  effective(document: Doc): Doc {
    const state = this.docs.get(idKey(document._id));
    if (!state || state.ops.size === 0) return state?.original ?? document;
    let result: Json = state.original;
    const ordered = [...state.ops.entries()].sort(([a], [b]) => a.length - b.length);
    for (const [path, op] of ordered) result = op.kind === "set" ? setAt(result, split(path), op.value) : removeAt(result, split(path));
    return result as Doc;
  }

  /** How a path of a document reads now: "set" (changed or added), "unset", "inside" (a container with edits in it). */
  pathState(id: Json, path: readonly string[]): "set" | "added" | "inside" | null {
    const state = this.docs.get(idKey(id));
    if (!state || state.ops.size === 0) return null;
    const text = joined(path);
    for (const [opPath, op] of state.ops) {
      if (op.kind !== "set") continue;
      if (opPath === text || inside(opPath, text)) return getAt(state.original, path).found ? "set" : "added";
    }
    for (const opPath of state.ops.keys()) if (inside(text, opPath)) return "inside";
    return null;
  }

  /** Sets the value at `path` of `document` (as loaded). `keepType`: a plain number the backend keeps in the stored numeric type. */
  setValue(document: Doc, path: readonly string[], value: Json, keepType = false): void {
    if (!editablePath(path))
      throw new Error(path[0] === "_id" ? "_id cannot be changed; duplicate the document instead." : "This field cannot be edited here.");
    const state = this.state(document);
    this.setIn(state, path, value, keepType);
    this.changed();
  }

  /** Removes the field (or array item) at `path`. */
  removeField(document: Doc, path: readonly string[]): void {
    if (!editablePath(path)) throw new Error(path[0] === "_id" ? "_id cannot be removed." : "This field cannot be edited here.");
    const state = this.state(document);
    const parentPath = path.slice(0, -1);
    const parent = getAt(this.effective(state.original), parentPath);
    if (parentPath.length > 0 && Array.isArray(parent.value)) {
      this.setIn(state, parentPath, removeAt(parent.value, path.slice(-1)), false);
    } else {
      const text = joined(path);
      const ancestor = this.ancestorSet(state, text);
      if (ancestor) {
        const [opPath, op] = ancestor;
        this.putSet(state, opPath, removeAt(op.value, split(text).slice(split(opPath).length)), op.keepType);
      } else {
        for (const opPath of [...state.ops.keys()]) if (inside(text, opPath)) state.ops.delete(opPath);
        if (getAt(state.original, path).found) state.ops.set(text, { kind: "unset" });
        else state.ops.delete(text);
      }
    }
    this.cleanup(state);
    this.changed();
  }

  /** Adds a field `name` to the document at `parentPath` (or an item to the array there; `name` is then ignored). */
  addField(document: Doc, parentPath: readonly string[], name: string, value: Json, keepType = false): void {
    const state = this.state(document);
    const parent = getAt(this.effective(state.original), parentPath);
    if (Array.isArray(parent.value)) {
      if (parentPath.length === 0 || !editablePath(parentPath)) throw new Error("This array cannot be edited here.");
      this.setIn(state, parentPath, [...parent.value, value], false);
      this.changed();
      return;
    }
    const problem = fieldNameProblem(name);
    if (problem) throw new Error(problem);
    if (parentPath.length === 0 && name === "_id") throw new Error("_id cannot be added to an existing document.");
    if (parentPath.length > 0 && !isDoc(parent.value)) throw new Error("Fields can be added to documents and arrays only.");
    if (isDoc(parent.value) && Object.hasOwn(parent.value, name)) throw new Error(`The document already has a field ${name}.`);
    this.setIn(state, [...parentPath, name], value, keepType);
    this.changed();
  }

  /** Renames the field at `path` (it moves to the end of its document, as `$set` writes a new field last). */
  renameField(document: Doc, path: readonly string[], name: string): void {
    const problem = fieldNameProblem(name);
    if (problem) throw new Error(problem);
    if (!editablePath(path)) throw new Error("This field cannot be renamed.");
    const state = this.state(document);
    const current = this.effective(state.original);
    const parentPath = path.slice(0, -1);
    const parent = getAt(current, parentPath).value;
    if (!isDoc(parent)) throw new Error("Only fields of a document can be renamed.");
    if (name === path[path.length - 1]) return;
    if (Object.hasOwn(parent, name)) throw new Error(`The document already has a field ${name}.`);
    if (parentPath.length === 0 && name === "_id") throw new Error("A field cannot be renamed to _id.");
    const value = getAt(current, path).value;
    this.removeField(document, path);
    // Removing an added field may have left no edit (and dropped the document's state): take it again.
    this.setIn(this.state(document), [...parentPath, name], value, false);
    this.changed();
  }

  deleteDocument(document: Doc): void {
    const state = this.state(document);
    state.ops.clear();
    state.deleted = true;
    this.changed();
  }

  /** Drops every pending change of the document with `_id` `id` (edits and deletion). */
  discardDocument(id: Json): void {
    if (this.docs.delete(idKey(id))) this.changed();
  }

  /** Adds a new document (Extended JSON, as parsed from mongosh syntax); returns its local id. */
  insertDocument(document: Doc): string {
    if (!isDoc(document)) throw new Error("A document is an object, e.g. { name: 'x' }.");
    const id = `new-${this.nextInsert++}`;
    this.inserts.push({ id, document });
    this.changed();
    return id;
  }

  removeInsert(id: string): void {
    const before = this.inserts.length;
    this.inserts = this.inserts.filter((item) => item.id !== id);
    if (this.inserts.length !== before) this.changed();
  }

  discardAll(): void {
    if (this.isEmpty()) return;
    this.docs.clear();
    this.inserts = [];
    this.changed();
  }

  /** The changes for `apply_edits`: inserts first, then updates and deletes in the order they were first edited. */
  changes(): BuiltChange[] {
    const built: BuiltChange[] = this.inserts.map((item) => ({
      insert: item.id,
      change: { op: "insert", values: Object.entries(item.document).map(([column, value]) => ({ column, value: exactValue(value) })) },
    }));
    for (const state of this.docs.values()) {
      const key: Field[] = [{ column: "_id", value: exactValue(state.id) }];
      if (state.deleted) {
        const original = Object.entries(state.original).map(([column, value]) => ({ column, value: exactValue(value) }));
        built.push({ id: state.id, change: { op: "delete", key, original } });
        continue;
      }
      if (state.ops.size === 0) continue;
      const values: Field[] = [];
      const original: Field[] = [];
      for (const [path, op] of state.ops) {
        values.push({ column: path, value: op.kind === "unset" ? UNSET_VALUE : valueOf(op) });
        const loaded = getAt(state.original, split(path));
        original.push({ column: path, value: exactValue(loaded.found ? loaded.value : null) });
      }
      built.push({ id: state.id, change: { op: "update", key, values, original } });
    }
    return built;
  }

  private state(document: Doc): DocState {
    if (!Object.hasOwn(document, "_id")) throw new Error("A document without _id cannot be edited.");
    const key = idKey(document._id);
    let state = this.docs.get(key);
    if (!state) {
      state = { id: document._id, original: document, ops: new Map(), deleted: false };
      this.docs.set(key, state);
    }
    if (state.deleted) throw new Error("The document is deleted (pending); restore it first.");
    return state;
  }

  private ancestorSet(state: DocState, path: string): [string, Extract<EditOp, { kind: "set" }>] | null {
    for (const [opPath, op] of state.ops) if (op.kind === "set" && inside(opPath, path)) return [opPath, op];
    return null;
  }

  private setIn(state: DocState, path: readonly string[], value: Json, keepType: boolean): void {
    const text = joined(path);
    const ancestor = this.ancestorSet(state, text);
    if (ancestor) {
      const [opPath, op] = ancestor;
      this.putSet(state, opPath, setAt(op.value, path.slice(split(opPath).length), value), op.keepType);
    } else {
      for (const opPath of [...state.ops.keys()]) if (inside(text, opPath)) state.ops.delete(opPath);
      this.putSet(state, text, value, keepType);
    }
    this.cleanup(state);
  }

  /** Sets the op at `path`, or drops it when the value is what was loaded. */
  private putSet(state: DocState, path: string, value: Json, keepType: boolean): void {
    const loaded = getAt(state.original, split(path));
    if (loaded.found && sameJson(loaded.value, value)) state.ops.delete(path);
    else state.ops.set(path, { kind: "set", value, keepType: keepType && typeof value === "number" });
  }

  private cleanup(state: DocState): void {
    if (!state.deleted && state.ops.size === 0) this.docs.delete(idKey(state.id));
  }
}

function valueOf(op: Extract<EditOp, { kind: "set" }>): Value {
  if (op.keepType && typeof op.value === "number") return Number.isInteger(op.value) ? { t: "int", v: op.value } : { t: "float", v: op.value };
  return exactValue(op.value);
}
