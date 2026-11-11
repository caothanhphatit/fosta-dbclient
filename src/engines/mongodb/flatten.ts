/**
 * The Table mode's model, without DOM: a page of documents flattened to columns of dotted paths (`address.city`).
 * Embedded documents open into one column per field, down to `maxDepth`; arrays stay one cell (`[ 3 items ]`, opened
 * from the cell); columns come in first-seen order up to `maxColumns` (the rest are counted, not drawn). A column whose
 * documents hold different BSON types (not counting null and missing) is flagged `mixed`. Past the cap the paths most
 * documents have are kept.
 */
import { bsonType, BSON_TYPE_LABELS, type BsonType } from "./bson";
import { isNested, type Doc, type Json } from "./documents";

export interface FlatColumn {
  path: string;
  /** The types the column holds, most common first (null not counted). */
  types: BsonType[];
  mixed: boolean;
  /** The documents that have the path (null included). */
  present: number;
}

/** A cell: the value at the path, or missing (the document has no such field). */
export type FlatCell = { missing: true } | { missing: false; value: Json };

export interface FlatTable {
  columns: FlatColumn[];
  /** One row per document, one cell per column. */
  rows: FlatCell[][];
  /** Paths left out past `maxColumns`. */
  hiddenPaths: number;
}

export interface FlattenOptions {
  maxColumns?: number;
  /** Levels of embedded documents opened into columns; a deeper document stays one cell (`{ 3 fields }`). */
  maxDepth?: number;
}

export const MAX_COLUMNS = 150;
export const MAX_DEPTH = 4;

const MISSING: FlatCell = { missing: true };

interface PathInfo {
  path: string;
  /** Some document has a value here that is not opened into columns (a scalar, an array, a document past the depth). */
  leaf: boolean;
  counts: Map<BsonType, number>;
  present: number;
}

export function flatten(documents: readonly Doc[], options: FlattenOptions = {}): FlatTable {
  const maxColumns = Math.max(1, options.maxColumns ?? MAX_COLUMNS);
  const maxDepth = Math.max(1, options.maxDepth ?? MAX_DEPTH);
  // Every path seen, in first-seen order; parents come before their fields.
  const infos = new Map<string, PathInfo>();
  const cells: Map<string, Json>[] = [];
  for (const document of documents) {
    const row = new Map<string, Json>();
    const walk = (value: Doc, prefix: string, depth: number) => {
      for (const [key, inner] of Object.entries(value)) {
        const path = prefix ? `${prefix}.${key}` : key;
        let info = infos.get(path);
        if (!info) {
          info = { path, leaf: false, counts: new Map(), present: 0 };
          infos.set(path, info);
        }
        info.present += 1;
        const type = bsonType(inner);
        if (type !== "null") info.counts.set(type, (info.counts.get(type) ?? 0) + 1);
        const opens = isNested(inner) && !Array.isArray(inner) && depth < maxDepth && Object.keys(inner).length > 0;
        if (opens) {
          walk(inner as Doc, path, depth + 1);
        } else {
          info.leaf = true;
          row.set(path, inner);
        }
      }
    };
    walk(document, "", 1);
    cells.push(row);
  }
  const leaves = [...infos.values()].filter((info) => info.leaf);
  // Past the cap, the paths most documents have win (one document with thousands of fields does not push out the
  // fields every document has); the columns keep their first-seen order.
  const order = new Map(leaves.map((info, index) => [info, index]));
  const kept = new Set([...leaves].sort((a, b) => b.present - a.present || order.get(a)! - order.get(b)!).slice(0, maxColumns));
  const shown = leaves.filter((info) => kept.has(info));
  const columns: FlatColumn[] = shown.map((info) => {
    const types = [...info.counts.entries()].sort((a, b) => b[1] - a[1]).map(([type]) => type);
    return { path: info.path, types, mixed: types.length > 1, present: info.present };
  });
  const rows = cells.map((row) => shown.map((info): FlatCell => (row.has(info.path) ? { missing: false, value: row.get(info.path) } : MISSING)));
  return { columns, rows, hiddenPaths: leaves.length - shown.length };
}

/** What a column's header says under its name: `ObjectId`, `String`, or `Mixed: String, Number`. */
export function columnTypeText(column: FlatColumn): string {
  if (column.types.length === 0) return "Null";
  const labels = column.types.map((type) => BSON_TYPE_LABELS[type]);
  return column.mixed ? `Mixed: ${labels.join(", ")}` : labels[0];
}

/** Whether a cell's value is of another type than most of its column (flagged in mixed columns). */
export function isOddCell(column: FlatColumn, cell: FlatCell): boolean {
  if (!column.mixed || cell.missing) return false;
  const type = bsonType(cell.value);
  return type !== "null" && type !== column.types[0];
}
