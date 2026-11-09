/**
 * What is saved of a workspace's open tabs (backend `save_open_tabs`, restored on the next start): which tabs, in
 * order, the active one, and the text of query tabs. Pure (no DOM), so serialisation and the size bounds are unit-tested.
 */
import type { ObjectRef } from "../core/contract";

/** One open tab, enough to open it again. */
export type SavedTab =
  | {
      kind: "query";
      connectionId: string;
      title: string;
      database: string | null;
      language: string | null;
      text: string;
      /** The text that counts as saved (what last ran); other text asks before the tab closes. */
      saved: string;
      /** The text was too large to keep: the tab comes back empty. */
      textOmitted?: boolean;
    }
  | {
      kind: "object";
      connectionId: string;
      title: string;
      /** The view role ("data", "design", "diagram", …). */
      role: string;
      object: ObjectRef;
      filter?: string;
      columnFilters?: Record<string, string>;
      database?: string | null;
    }
  | { kind: "logs"; taskId: string; title: string };

export interface SavedWorkspaceTabs {
  version: 1;
  tabs: SavedTab[];
  /** Index of the active tab in `tabs`. */
  active: number | null;
}

/** The most text kept per query tab (characters). */
export const MAX_TAB_TEXT = 1_000_000;
/** The most text kept per workspace: the web server takes requests up to 2 MB; the desktop app has more room. */
export const WORKSPACE_TEXT_BUDGET = { desktop: 12_000_000, web: 1_500_000 } as const;
/** The most a saved workspace may take as UTF-8 JSON (the web request limit; the backend's own cap on the desktop). */
export const WORKSPACE_MAX_BYTES = { desktop: 15_000_000, web: 1_800_000 } as const;

const encoder = new TextEncoder();

/** The size of `state` as the request carries it (UTF-8 JSON). */
export function jsonBytes(state: unknown): number {
  return encoder.encode(JSON.stringify(state)).length;
}

/**
 * Leaves out the text of the last query tabs that have some until `state` takes at most `maxBytes` (non-ASCII text
 * and JSON escapes take more bytes than characters).
 */
export function fitToBytes(state: SavedWorkspaceTabs, maxBytes: number, size: (state: SavedWorkspaceTabs) => number = jsonBytes): SavedWorkspaceTabs {
  let current = state;
  for (let index = current.tabs.length - 1; index >= 0 && size(current) > maxBytes; index -= 1) {
    const tab = current.tabs[index];
    if (tab.kind !== "query" || (tab.text === "" && tab.saved === "")) continue;
    const tabs = [...current.tabs];
    tabs[index] = { ...tab, text: "", saved: "", textOmitted: tab.text.trim() !== "" || tab.textOmitted === true };
    current = { ...current, tabs };
  }
  return current;
}

/**
 * The state to save: each query tab keeps its text when it fits in `MAX_TAB_TEXT` and in what is left of `budget`
 * (earlier tabs first); otherwise its text is left out and marked `textOmitted`.
 */
export function serializeTabs(tabs: SavedTab[], active: number | null, budget: number): SavedWorkspaceTabs {
  let left = budget;
  const bounded = tabs.map((tab): SavedTab => {
    if (tab.kind !== "query") return tab;
    const size = tab.text.length + tab.saved.length;
    if (tab.text.length <= MAX_TAB_TEXT && size <= left) {
      left -= size;
      return tab;
    }
    return { ...tab, text: "", saved: "", textOmitted: tab.text.trim() !== "" || tab.textOmitted === true };
  });
  return { version: 1, tabs: bounded, active: active !== null && active >= 0 && active < tabs.length ? active : null };
}

const isString = (value: unknown): value is string => typeof value === "string";
const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);

function parseObject(value: unknown): ObjectRef | null {
  if (!isRecord(value) || !isString(value.name) || !isString(value.kind)) return null;
  const optional = (field: unknown) => (isString(field) ? field : null);
  const path = Array.isArray(value.path) && value.path.every(isString) ? (value.path as string[]) : undefined;
  return { database: optional(value.database), schema: optional(value.schema), name: value.name, kind: value.kind, ...(path ? { path } : {}) };
}

function parseTab(value: unknown): SavedTab | null {
  if (!isRecord(value) || !isString(value.title)) return null;
  if (value.kind === "query" && isString(value.connectionId) && isString(value.text)) {
    return {
      kind: "query",
      connectionId: value.connectionId,
      title: value.title,
      database: isString(value.database) ? value.database : null,
      language: isString(value.language) ? value.language : null,
      text: value.text,
      saved: isString(value.saved) ? value.saved : value.text,
      ...(value.textOmitted === true ? { textOmitted: true } : {}),
    };
  }
  if (value.kind === "object" && isString(value.connectionId) && isString(value.role)) {
    const object = parseObject(value.object);
    if (!object) return null;
    const columnFilters =
      isRecord(value.columnFilters) && Object.values(value.columnFilters).every(isString) ? (value.columnFilters as Record<string, string>) : undefined;
    return {
      kind: "object",
      connectionId: value.connectionId,
      title: value.title,
      role: value.role,
      object,
      ...(isString(value.filter) ? { filter: value.filter } : {}),
      ...(columnFilters ? { columnFilters } : {}),
      ...(isString(value.database) ? { database: value.database } : {}),
    };
  }
  if (value.kind === "logs" && isString(value.taskId)) return { kind: "logs", taskId: value.taskId, title: value.title };
  return null;
}

/** Reads what `load_open_tabs` returned for one workspace; malformed tabs are skipped, anything else gives null. */
export function parseSavedTabs(value: unknown): SavedWorkspaceTabs | null {
  if (!isRecord(value) || !Array.isArray(value.tabs)) return null;
  const tabs: SavedTab[] = [];
  let active: number | null = null;
  value.tabs.forEach((item, index) => {
    const tab = parseTab(item);
    if (!tab) return;
    if (index === value.active) active = tabs.length;
    tabs.push(tab);
  });
  return { version: 1, tabs, active };
}

/**
 * The tabs that can come back: a tab whose connection (or command) no longer exists is dropped and counted, and the
 * active index follows the tabs that stay (the nearest earlier one when the active tab itself was dropped).
 */
export function restorableTabs(
  saved: SavedWorkspaceTabs,
  connectionExists: (id: string) => boolean,
  taskExists: (id: string) => boolean,
): { tabs: SavedTab[]; active: number | null; dropped: number } {
  const tabs: SavedTab[] = [];
  let active: number | null = null;
  saved.tabs.forEach((tab, index) => {
    const keep = tab.kind === "logs" ? taskExists(tab.taskId) : connectionExists(tab.connectionId);
    if (keep) tabs.push(tab);
    if (index === saved.active) active = tabs.length > 0 ? (keep ? tabs.length - 1 : Math.max(0, tabs.length - 1)) : null;
  });
  return { tabs, active, dropped: saved.tabs.length - tabs.length };
}

/** "Query 7" → 7: new query tabs continue after the highest restored number. */
export function queryNumber(title: string): number {
  const match = /^Query (\d+)$/.exec(title);
  return match ? Number(match[1]) : 0;
}
