/** Types and pure helpers of the ClickHouse views: operation results, the edit notice, formatting. */
import type { ObjectDescription, ObjectNode } from "../../core/contract";

/** What every `clickhouse.*` operation answers: a grid of text cells (null for NULL) and an optional note. */
export interface OperationTable {
  columns: string[];
  rows: (string | null)[][];
  note?: string | null;
}

/** The text of the badge the backend puts on tables whose engine has no mutations (`engines/clickhouse/tree.rs`). */
export const READ_ONLY_BADGE = "read-only";

/** What the edit notice says for an editable table: mutations are asynchronous, applied one by one, and guarded. */
export const MUTATION_NOTICE =
  "Edits are asynchronous mutations, applied one by one without a transaction; a row that changed since you loaded it is a conflict.";

/** Whether the tree node marks a table the grid must not edit. */
export function isReadOnlyNode(node: ObjectNode | null): boolean {
  return node?.badges.some((badge) => badge.text === READ_ONLY_BADGE) ?? false;
}

/** The "edits" fact of a description (`read-only: <why>` or `asynchronous mutations …`); null when it has none. */
export function editFact(description: Pick<ObjectDescription, "extra">): string | null {
  return description.extra.find(([name]) => name === "edits")?.[1] ?? null;
}

/** The reason a described table is read-only, or null when it can be edited. */
export function readOnlyReason(description: Pick<ObjectDescription, "extra">): string | null {
  const fact = editFact(description);
  return fact?.startsWith("read-only: ") ? fact.slice("read-only: ".length) : null;
}

/** The index of a result column, or -1. */
export function columnIndex(table: OperationTable, name: string): number {
  return table.columns.indexOf(name);
}

/** A cell by column name; "" when the column or the value is missing. */
export function cellOf(table: OperationTable, row: (string | null)[], name: string): string {
  const index = columnIndex(table, name);
  return index < 0 ? "" : (row[index] ?? "");
}

/** `1234567` as `1.2M`; plain below ten thousand. */
export function shortCount(value: number): string {
  if (value < 10_000) return String(value);
  const units: [number, string][] = [
    [1e9, "B"],
    [1e6, "M"],
    [1e3, "K"],
  ];
  for (const [size, suffix] of units) if (value >= size) return `${(value / size).toFixed(1).replace(/\.0$/, "")}${suffix}`;
  return String(value);
}

/** `52428800` as `50 MiB`. */
export function formatBytes(value: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${value} B` : `${size.toFixed(1).replace(/\.0$/, "")} ${units[unit]}`;
}

/** `1234` ms as `1.2 s`; `61000` as `1 min 1 s`. */
export function formatMillis(value: number): string {
  if (value < 1000) return `${Math.round(value)} ms`;
  const seconds = value / 1000;
  if (seconds < 60) return `${seconds.toFixed(1).replace(/\.0$/, "")} s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes} min ${Math.round(seconds - minutes * 60)} s`;
}

/** A number cell as a formatted number; the text itself when it is not one. */
export function numberCell(text: string): string {
  const value = Number(text);
  return text.trim() === "" || Number.isNaN(value) ? text : new Intl.NumberFormat("en-US").format(value);
}

/** A statement on one line, cut at `limit` characters. */
export function oneLine(sql: string, limit = 160): string {
  const text = sql.replace(/\s+/g, " ").trim();
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

/** Whether a query log row is a failure (`ExceptionBeforeStart` and `ExceptionWhileProcessing`). */
export function isFailure(type: string): boolean {
  return type.startsWith("Exception");
}
