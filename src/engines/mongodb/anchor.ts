/**
 * The position kept across List, JSON and Table: the `_id` of the document at the top of the view. Each mode tells the
 * index of its top document and scrolls to one; this file finds that document again in a page (by `_id`, falling back
 * to the same index for documents without one) and maps the JSON text's lines to documents.
 */
import type { Doc } from "./documents";

/** The document a view showed at its top: its `_id` (as canonical text) and where it was. */
export interface Anchor {
  id: string | null;
  index: number;
}

/** The key of an `_id` value: the same id in two pages gives the same key. */
export function idKey(document: Doc | undefined): string | null {
  if (!document || !("_id" in document)) return null;
  return stableJson(document._id);
}

/** JSON with the keys of every object sorted, so `{ $oid }` read twice compares equal. */
function stableJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  if (typeof value === "object" && value !== null) {
    const entries = Object.entries(value as Record<string, unknown>).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return `{${entries.map(([key, inner]) => `${JSON.stringify(key)}:${stableJson(inner)}`).join(",")}}`;
  }
  return JSON.stringify(value ?? null);
}

export function anchorAt(documents: readonly Doc[], index: number): Anchor | null {
  if (documents.length === 0 || index < 0) return null;
  const at = Math.min(index, documents.length - 1);
  return { id: idKey(documents[at]), index: at };
}

/** The index of the anchored document in `documents`: by `_id`, else the same index (clamped); 0 without an anchor. */
export function indexOfAnchor(documents: readonly Doc[], anchor: Anchor | null): number {
  if (!anchor || documents.length === 0) return 0;
  if (anchor.id !== null) {
    const found = documents.findIndex((document) => idKey(document) === anchor.id);
    if (found >= 0) return found;
  }
  return Math.max(0, Math.min(anchor.index, documents.length - 1));
}

/**
 * The 0-based line each document starts on in `JSON.stringify(documents, null, 2)` (line 0 is the opening `[`), counted
 * from the text so it matches what the viewer shows.
 */
export function jsonDocumentLines(documents: readonly Doc[]): number[] {
  const starts: number[] = [];
  let line = 1;
  for (const document of documents) {
    starts.push(line);
    const text = JSON.stringify(document, null, 2);
    let breaks = 0;
    for (let i = 0; i < text.length; i++) if (text.charCodeAt(i) === 10) breaks++;
    line += breaks + 1;
  }
  return starts;
}

/** The document a line belongs to, given `jsonDocumentLines`; -1 for an empty page. */
export function documentAtLine(starts: readonly number[], line: number): number {
  if (starts.length === 0) return -1;
  let low = 0;
  let high = starts.length - 1;
  while (low < high) {
    const mid = (low + high + 1) >> 1;
    if (starts[mid] <= line) low = mid;
    else high = mid - 1;
  }
  return low;
}
