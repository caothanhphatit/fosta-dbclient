/**
 * What is saved of a workspace's open tabs (backend `save_open_tabs`, restored on the next start): which tabs, in
 * order, the active one, and the text of query tabs. Pure (no DOM), so serialisation, merging and the size bounds are
 * unit-tested.
 */
import type { ObjectRef } from "../core/contract";
import { boundViewState, parseViewState, type WorkspaceViewState } from "../features/workspace/workspaceState";

/** One open tab, enough to open it again (without its id). */
export type SavedTabBody =
  | {
      kind: "query";
      connectionId: string;
      title: string;
      database: string | null;
      language: string | null;
      text: string;
      /** The text that counts as saved (what last ran); other text asks before the tab closes. Absent: `text` itself. */
      saved?: string;
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
      /** An engine workspace tab's section and section states (`features/workspace`), at most `MAX_VIEW_BYTES`. */
      view?: WorkspaceViewState;
    }
  | { kind: "logs"; taskId: string; title: string };

/** One open tab; `id` stays the same across saves and restarts, so another window's save can be merged tab by tab. */
export type SavedTab = SavedTabBody & { id: string };

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

/** The text that counts as saved for a query tab. */
export function savedText(tab: Extract<SavedTab, { kind: "query" }>): string {
  return tab.saved ?? tab.text;
}

/** A query tab without its text (marked `textOmitted` when it had some). */
function withoutText(tab: Extract<SavedTab, { kind: "query" }>): SavedTab {
  const { id, connectionId, title, database, language } = tab;
  return { id, kind: "query", connectionId, title, database, language, text: "", textOmitted: tab.text.trim() !== "" || tab.textOmitted === true };
}

/** A query tab as stored: `saved` only when it differs from `text`. */
function compact(tab: SavedTab): SavedTab {
  if (tab.kind !== "query" || tab.saved === undefined || tab.saved !== tab.text) return tab;
  const copy = { ...tab };
  delete copy.saved;
  return copy;
}

/** A workspace tab without its section states (it opens on the same section, with their defaults). */
function withoutViewState(tab: Extract<SavedTab, { kind: "object" }>): SavedTab {
  return tab.view ? { ...tab, view: { section: tab.view.section, state: {} } } : tab;
}

/**
 * Leaves out the text of the last query tabs that have some until `state` takes at most `maxBytes` (non-ASCII text
 * and JSON escapes take more bytes than characters); still too large, the section states of the last workspace tabs
 * too. The whole state is measured once; each dropped part then only subtracts what its tab shrank by.
 */
export function fitToBytes(state: SavedWorkspaceTabs, maxBytes: number): SavedWorkspaceTabs {
  let size = jsonBytes(state);
  if (size <= maxBytes) return state;
  const tabs = [...state.tabs];
  for (let index = tabs.length - 1; index >= 0 && size > maxBytes; index -= 1) {
    const tab = tabs[index];
    if (tab.kind !== "query" || (tab.text === "" && savedText(tab) === "")) continue;
    const emptied = withoutText(tab);
    size -= jsonBytes(tab) - jsonBytes(emptied);
    tabs[index] = emptied;
  }
  for (let index = tabs.length - 1; index >= 0 && size > maxBytes; index -= 1) {
    const tab = tabs[index];
    if (tab.kind !== "object" || !tab.view || Object.keys(tab.view.state).length === 0) continue;
    const emptied = withoutViewState(tab);
    size -= jsonBytes(tab) - jsonBytes(emptied);
    tabs[index] = emptied;
  }
  return { ...state, tabs };
}

/**
 * The state to save: each query tab keeps its text when it fits in `MAX_TAB_TEXT` and in what is left of `budget`
 * (earlier tabs first); otherwise its text is left out and marked `textOmitted`. `saved` is kept only when it differs.
 */
export function serializeTabs(tabs: SavedTab[], active: number | null, budget: number): SavedWorkspaceTabs {
  let left = budget;
  const bounded = tabs.map((tab): SavedTab => {
    if (tab.kind === "object") return tab.view ? { ...tab, view: boundViewState(tab.view) } : tab;
    if (tab.kind !== "query") return tab;
    const stored = compact(tab);
    const size = tab.text.length + (stored.kind === "query" && stored.saved !== undefined ? stored.saved.length : 0);
    if (tab.text.length <= MAX_TAB_TEXT && size <= left) {
      left -= size;
      return stored;
    }
    return withoutText(tab);
  });
  return { version: 1, tabs: bounded, active: active !== null && active >= 0 && active < tabs.length ? active : null };
}

/**
 * What a window saves for a workspace: its own tabs (in its order, with its active tab), then the tabs of the stored
 * state that this window does not show and never had (another window's tabs, by id). A tab this window had and closed
 * (`previous` has it, `local` does not) stays closed; a tab closed in another window but still open here comes back.
 * With one window the stored state holds only its own tabs, so this is `local`.
 */
export function mergeTabs(local: SavedWorkspaceTabs, stored: SavedWorkspaceTabs | null, previous: ReadonlySet<string>): SavedWorkspaceTabs {
  if (!stored || stored.tabs.length === 0) return local;
  const shown = new Set(local.tabs.map((tab) => tab.id));
  const foreign = stored.tabs.filter((tab) => !shown.has(tab.id) && !previous.has(tab.id));
  return foreign.length === 0 ? local : { ...local, tabs: [...local.tabs, ...foreign] };
}

const isString = (value: unknown): value is string => typeof value === "string";
const isRecord = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);

function parseObject(value: unknown): ObjectRef | null {
  if (!isRecord(value) || !isString(value.name) || !isString(value.kind)) return null;
  const optional = (field: unknown) => (isString(field) ? field : null);
  const path = Array.isArray(value.path) && value.path.every(isString) ? (value.path as string[]) : undefined;
  return { database: optional(value.database), schema: optional(value.schema), name: value.name, kind: value.kind, ...(path ? { path } : {}) };
}

function parseBody(value: Record<string, unknown>): SavedTabBody | null {
  if (!isString(value.title)) return null;
  if (value.kind === "query" && isString(value.connectionId) && isString(value.text)) {
    return {
      kind: "query",
      connectionId: value.connectionId,
      title: value.title,
      database: isString(value.database) ? value.database : null,
      language: isString(value.language) ? value.language : null,
      text: value.text,
      ...(isString(value.saved) && value.saved !== value.text ? { saved: value.saved } : {}),
      ...(value.textOmitted === true ? { textOmitted: true } : {}),
    };
  }
  if (value.kind === "object" && isString(value.connectionId) && isString(value.role)) {
    const object = parseObject(value.object);
    if (!object) return null;
    // Files from before workspace tabs have no `view`; a malformed one is dropped, not the tab.
    const view = parseViewState(value.view);
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
      ...(view ? { view } : {}),
    };
  }
  if (value.kind === "logs" && isString(value.taskId)) return { kind: "logs", taskId: value.taskId, title: value.title };
  return null;
}

function parseTab(value: unknown, seen: Set<string>): SavedTab | null {
  if (!isRecord(value)) return null;
  const body = parseBody(value);
  if (!body) return null;
  // A file from before tab ids (or a duplicate id) gets a fresh one.
  const id = isString(value.id) && value.id !== "" && !seen.has(value.id) ? value.id : crypto.randomUUID();
  seen.add(id);
  return { id, ...body };
}

/** Reads what `load_open_tabs` returned for one workspace; malformed tabs are skipped, anything else gives null. */
export function parseSavedTabs(value: unknown): SavedWorkspaceTabs | null {
  if (!isRecord(value) || !Array.isArray(value.tabs)) return null;
  const tabs: SavedTab[] = [];
  const seen = new Set<string>();
  let active: number | null = null;
  value.tabs.forEach((item, index) => {
    const tab = parseTab(item, seen);
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
