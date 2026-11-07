/**
 * Which grid columns are shown. Pure helpers (no DOM) used by the grid and the Columns popover;
 * table views remember their hidden columns in localStorage per connection, database and table.
 */

const STORAGE_PREFIX = "db-studio.columns";

export function columnStorageKey(connectionId: string, database: string, table: string): string {
  return `${STORAGE_PREFIX}.${connectionId}.${database}.${table}`;
}

/** Hidden column names saved for a table; empty when nothing is saved or storage is unavailable. */
export function loadHiddenColumns(key: string): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((item): item is string => typeof item === "string") : [];
  } catch {
    return [];
  }
}

export function saveHiddenColumns(key: string, hidden: string[]): void {
  try {
    if (hidden.length === 0) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(hidden));
  } catch {
    // Storage can be unavailable; the choice then lasts until the tab closes.
  }
}

/**
 * Indexes of the columns to draw, in order. Names that are not columns are ignored, and when every
 * column would be hidden the first one stays visible.
 */
export function visibleColumnIndexes(columns: readonly string[], hidden: Iterable<string>): number[] {
  const hiddenSet = new Set(hidden);
  const visible = columns.flatMap((name, index) => (hiddenSet.has(name) ? [] : [index]));
  return visible.length === 0 && columns.length > 0 ? [0] : visible;
}

/** The hidden names that apply to `columns`, normalised so at least one column stays visible. */
export function normalizeHidden(columns: readonly string[], hidden: Iterable<string>): string[] {
  const visible = new Set(visibleColumnIndexes(columns, hidden).map((index) => columns[index]));
  return columns.filter((name) => !visible.has(name));
}

/** Hides or shows one column; hiding the last visible column is refused (returns `hidden` unchanged). */
export function setColumnHidden(columns: readonly string[], hidden: readonly string[], name: string, hide: boolean): string[] {
  if (!columns.includes(name)) return [...hidden];
  if (hide) {
    const visible = visibleColumnIndexes(columns, hidden);
    if (visible.length === 1 && columns[visible[0]] === name) return normalizeHidden(columns, hidden);
    return normalizeHidden(columns, [...hidden, name]);
  }
  return normalizeHidden(
    columns,
    hidden.filter((item) => item !== name),
  );
}

/** "Hide all": everything except the first column. */
export function hideAllButFirst(columns: readonly string[]): string[] {
  return columns.slice(1);
}

/**
 * The visible column reached by moving `delta` steps from `current` (a column index), clamped to the
 * first and last visible column. A hidden `current` snaps to the nearest visible column first.
 */
export function stepVisibleColumn(visible: readonly number[], current: number, delta: number): number {
  if (visible.length === 0) return current;
  const position = nearestVisiblePosition(visible, current);
  const target = Math.min(Math.max(position + delta, 0), visible.length - 1);
  return visible[target];
}

/** The visible column closest to `column` (preferring the one after it), or `column` if it is visible. */
export function nearestVisibleColumn(visible: readonly number[], column: number): number {
  if (visible.length === 0) return column;
  return visible[nearestVisiblePosition(visible, column)];
}

function nearestVisiblePosition(visible: readonly number[], column: number): number {
  const exact = visible.indexOf(column);
  if (exact >= 0) return exact;
  const after = visible.findIndex((index) => index > column);
  return after >= 0 ? after : visible.length - 1;
}

/** Columns whose name or type contains `query` (case-insensitive), as indexes. */
export function matchColumns(columns: readonly { name: string; typeName: string }[], query: string): number[] {
  const text = query.trim().toLowerCase();
  return columns.flatMap((column, index) => (!text || column.name.toLowerCase().includes(text) || column.typeName.toLowerCase().includes(text) ? [index] : []));
}
