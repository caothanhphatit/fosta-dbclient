/** Pure geometry of the ER diagram: box sizes, the layered auto layout, and the foreign-key links (declared or inferred). */
import type { Relation, SchemaGraph } from "../../core/contract";

export const HEADER = 28;
export const ROW = 20;
export const GAP_X = 110;
export const GAP_Y = 36;
export const STUB = 18;

export interface Link {
  name: string;
  from: string;
  fromColumns: string[];
  to: string;
  toColumns: string[];
  inferred: boolean;
  path: SVGPathElement | null;
  marks: SVGGElement | null;
}

/** What the layout needs to know about a table box. */
export interface LayoutBox {
  name: string;
  width: number;
  height: number;
}

export interface Position {
  x: number;
  y: number;
}

/** Height of a box showing `rowCount` column rows (an empty box keeps one row for its message). */
export function boxHeightFor(rowCount: number): number {
  return HEADER + Math.max(1, rowCount) * ROW + 6;
}

/**
 * Layered layout: a table sits one column right of every table it references,
 * rows inside a column are ordered by the average position of their neighbours.
 * Tables without links are packed in a grid below.
 */
export function layoutBoxes(boxes: LayoutBox[], links: { from: string; to: string }[]): Map<string, Position> {
  const byName = new Map(boxes.map((box) => [box.name, box]));
  const placed = new Map<string, Position>();
  const names = boxes.map((box) => box.name);
  const parents = new Map(names.map((name) => [name, new Set<string>()]));
  const neighbours = new Map(names.map((name) => [name, new Set<string>()]));
  for (const link of links) {
    if (link.from === link.to) continue;
    parents.get(link.from)?.add(link.to);
    neighbours.get(link.from)?.add(link.to);
    neighbours.get(link.to)?.add(link.from);
  }
  const rank = new Map<string, number>();
  const visiting = new Set<string>();
  const rankOf = (name: string): number => {
    const known = rank.get(name);
    if (known !== undefined) return known;
    if (visiting.has(name)) return 0;
    visiting.add(name);
    let value = 0;
    for (const parent of parents.get(name) ?? []) value = Math.max(value, rankOf(parent) + 1);
    visiting.delete(name);
    rank.set(name, value);
    return value;
  };
  const connected = names.filter((name) => (neighbours.get(name)?.size ?? 0) > 0);
  const isolated = names.filter((name) => (neighbours.get(name)?.size ?? 0) === 0).sort();
  const byRank = new Map<number, string[]>();
  for (const name of connected.sort()) {
    const index = rankOf(name);
    const column = byRank.get(index) ?? [];
    column.push(name);
    byRank.set(index, column);
  }
  // A cycle can leave gaps between ranks; keep only the used columns, in rank order.
  const columns = [...byRank.entries()].sort((a, b) => a[0] - b[0]).map(([, column]) => column);
  const order = new Map<string, number>();
  const index = () => columns.forEach((column) => column.forEach((name, position) => order.set(name, position)));
  index();
  for (let pass = 0; pass < 4; pass += 1) {
    for (const column of columns) {
      const score = (name: string) => {
        const around = [...(neighbours.get(name) ?? [])].map((other) => order.get(other) ?? 0);
        return around.length ? around.reduce((sum, value) => sum + value, 0) / around.length : (order.get(name) ?? 0);
      };
      column.sort((a, b) => score(a) - score(b));
    }
    index();
  }
  let x = 0;
  let bottom = 0;
  const heights = columns.map((column) => column.reduce((sum, name) => sum + byName.get(name)!.height + GAP_Y, -GAP_Y));
  const tallest = Math.max(0, ...heights);
  columns.forEach((column, columnIndex) => {
    const width = Math.max(...column.map((name) => byName.get(name)!.width));
    let y = (tallest - heights[columnIndex]) / 2;
    for (const name of column) {
      const box = byName.get(name)!;
      placed.set(name, { x, y });
      y += box.height + GAP_Y;
      bottom = Math.max(bottom, y);
    }
    x += width + GAP_X;
  });
  const gridWidth = Math.max(x - GAP_X, 1100);
  let gx = 0;
  let gy = connected.length ? bottom + GAP_Y * 2 : 0;
  let lineHeight = 0;
  for (const name of isolated) {
    const box = byName.get(name)!;
    if (gx > 0 && gx + box.width > gridWidth) {
      gx = 0;
      gy += lineHeight + GAP_Y;
      lineHeight = 0;
    }
    placed.set(box.name, { x: gx, y: gy });
    gx += box.width + GAP_Y;
    lineHeight = Math.max(lineHeight, box.height);
  }
  return placed;
}

export function buildLinks(graph: SchemaGraph, infer: boolean): Link[] {
  const links: Link[] = graph.relations.map((relation: Relation) => ({
    name: relation.constraint,
    from: relation.table,
    fromColumns: relation.columns,
    to: relation.referencedTable,
    toColumns: relation.referencedColumns,
    inferred: false,
    path: null,
    marks: null,
  }));
  if (!infer) return links;
  const byName = new Map(graph.tables.map((table) => [table.name.toLowerCase(), table]));
  const declared = new Set(links.map((link) => `${link.from}|${link.fromColumns[0]}`));
  for (const table of graph.tables) {
    for (const column of table.columns) {
      if (column.primary || declared.has(`${table.name}|${column.name}`)) continue;
      const match = /^(.+?)(?:_id|Id|ID)$/.exec(column.name);
      if (!match) continue;
      const base = match[1].replace(/_$/, "").toLowerCase();
      const candidates = [base, `${base}s`, `${base}es`, base.replace(/y$/, "ies")];
      const target = candidates.map((name) => byName.get(name)).find((item) => item && item.name !== table.name);
      const key = target?.columns.find((item) => item.primary);
      if (!target || !key) continue;
      links.push({ name: "", from: table.name, fromColumns: [column.name], to: target.name, toColumns: [key.name], inferred: true, path: null, marks: null });
    }
  }
  return links;
}
