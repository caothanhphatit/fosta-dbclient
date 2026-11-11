/**
 * What changed between two versions of a document, without DOM: one entry per dotted path that was added, removed or
 * changed, walking into embedded documents (and into arrays of the same length), so a nested edit reads as
 * `address.city: "Oslo" → "Bergen"` and not as a whole new `address`. Used by the pending-changes review and by the
 * bulk update preview.
 */
import { shellText } from "./bson";
import { sameJson } from "./docEdits";
import { isWrapper, type Doc, type Json } from "./documents";

export type DiffKind = "added" | "removed" | "changed";

export interface DiffEntry {
  path: string;
  kind: DiffKind;
  before?: Json;
  after?: Json;
}

const isDoc = (value: Json): value is Doc => typeof value === "object" && value !== null && !Array.isArray(value) && !isWrapper(value);

/** The differences from `before` to `after`, in the order of `before`'s fields, then the new ones. */
export function diffDocuments(before: Json, after: Json, prefix = ""): DiffEntry[] {
  const at = (key: string) => (prefix ? `${prefix}.${key}` : key);
  if (isDoc(before) && isDoc(after)) {
    const entries: DiffEntry[] = [];
    for (const key of Object.keys(before)) {
      if (!Object.hasOwn(after, key)) entries.push({ path: at(key), kind: "removed", before: before[key] });
      else entries.push(...diffDocuments(before[key], after[key], at(key)));
    }
    for (const key of Object.keys(after)) if (!Object.hasOwn(before, key)) entries.push({ path: at(key), kind: "added", after: after[key] });
    return entries;
  }
  if (Array.isArray(before) && Array.isArray(after) && before.length === after.length && prefix) {
    return before.flatMap((item, index) => diffDocuments(item, after[index], at(String(index))));
  }
  if (sameJson(before, after)) return [];
  return [{ path: prefix, kind: "changed", before, after }];
}

const LIMIT = 160;

function short(value: Json): string {
  const text = shellText(value);
  return text.length > LIMIT ? `${text.slice(0, LIMIT)}…` : text;
}

/** One entry as a line: `+ path: value`, `- path: value`, `~ path: before → after`. */
export function entryText(entry: DiffEntry): string {
  switch (entry.kind) {
    case "added":
      return `+ ${entry.path}: ${short(entry.after)}`;
    case "removed":
      return `- ${entry.path}: ${short(entry.before)}`;
    case "changed":
      return `~ ${entry.path}: ${short(entry.before)} → ${short(entry.after)}`;
  }
}

/** "2 changed, 1 added, 1 removed" (empty when nothing differs). */
export function diffSummary(entries: readonly DiffEntry[]): string {
  const counts: Record<DiffKind, number> = { changed: 0, added: 0, removed: 0 };
  for (const entry of entries) counts[entry.kind]++;
  return (["changed", "added", "removed"] as const)
    .filter((kind) => counts[kind] > 0)
    .map((kind) => `${counts[kind]} ${kind}`)
    .join(", ");
}
