/**
 * The lines of the document tree (`docTree.ts`), without DOM: each document is a head, its fields (the opened
 * containers' items under them, indented), "Show N more" where a container shows only its first items, and a foot.
 */
import { isNested, type Doc, type Json } from "./documents";

export type TreeLine =
  | { type: "head"; doc: number }
  | {
      type: "field";
      doc: number;
      depth: number;
      key: string;
      value: Json;
      path: string[];
      id: string;
      /** A document or array with items (it can be opened). */
      container: boolean;
      expanded: boolean;
      inArray: boolean;
    }
  | { type: "more"; doc: number; depth: number; id: string; remaining: number; array: boolean }
  | { type: "foot"; doc: number };

export interface TreeState {
  /** Ids (`lineId`) of the opened containers. */
  expanded: Set<string>;
  /** Items shown per container id, past the default. */
  limits: Map<string, number>;
}

export interface TreeLimits {
  /** Fields a document shows before "Show N more fields". */
  topFields: number;
  /** Items an opened container shows at first. */
  childItems: number;
}

export const DEFAULT_LIMITS: TreeLimits = { topFields: 12, childItems: 200 };

export function lineId(doc: number, path: readonly string[]): string {
  return [String(doc), ...path].join("\u001f");
}

function entriesOf(value: Doc | Json[]): [string, Json][] {
  return Array.isArray(value) ? value.map((item, index) => [String(index), item]) : Object.entries(value);
}

const hasItems = (value: Json) => isNested(value) && (Array.isArray(value) ? value.length > 0 : Object.keys(value).length > 0);

/** The lines of `documents` with the containers in `state` opened. */
export function buildLines(documents: readonly Doc[], state: TreeState, limits: TreeLimits = DEFAULT_LIMITS): TreeLine[] {
  const lines: TreeLine[] = [];
  const container = (doc: number, value: Doc | Json[], path: string[], depth: number, limit: number) => {
    const id = lineId(doc, path);
    const entries = entriesOf(value);
    const shown = Math.min(entries.length, state.limits.get(id) ?? limit);
    const array = Array.isArray(value);
    for (let index = 0; index < shown; index++) {
      const [key, inner] = entries[index];
      const childPath = [...path, key];
      const childId = lineId(doc, childPath);
      const opens = hasItems(inner);
      const expanded = opens && state.expanded.has(childId);
      lines.push({ type: "field", doc, depth, key, value: inner, path: childPath, id: childId, container: opens, expanded, inArray: array });
      if (expanded) container(doc, inner as Doc | Json[], childPath, depth + 1, limits.childItems);
    }
    if (shown < entries.length) lines.push({ type: "more", doc, depth, id, remaining: entries.length - shown, array });
  };
  documents.forEach((document, doc) => {
    lines.push({ type: "head", doc });
    container(doc, document, [], 0, limits.topFields);
    lines.push({ type: "foot", doc });
  });
  return lines;
}
