/**
 * Saved queries of the collection workspace, without DOM: a saved query from the bar's state and back, the list the
 * Saved dropdown shows (this collection's first, then the others of the database, filtered by the search), and the
 * collection workspace a query-history entry reopens in. The queries are kept per workspace in the config file
 * (`list_saved_queries` / `save_saved_query` / `rename_saved_query` / `delete_saved_query`).
 */
import type { ObjectRef } from "../../core/contract";
import type { HistoryEntry, SavedQuery } from "../../core/types";
import { pipelineSummary } from "./pipelineModel";
import { clampLimit, EMPTY_QUERY, parseCommandText, type QueryState } from "./queryModel";

export const ENGINE = "mongodb";

export interface SavedQueryTarget {
  connectionId: string;
  database: string;
  collection: string;
}

/**
 * A new saved query of the bar's `state` (the backend sets `updatedAt`); with `pipeline` (stage texts, the Aggregations
 * section) it is a saved pipeline.
 */
export function savedQueryOf(id: string, name: string, state: QueryState, target: SavedQueryTarget, pipeline: string[] | null = null): SavedQuery {
  return {
    id,
    name: name.trim(),
    engine: ENGINE,
    connectionId: target.connectionId,
    database: target.database,
    collection: target.collection,
    query: {
      filter: state.filter,
      project: state.project,
      sort: state.sort,
      collation: state.collation,
      skip: Math.max(0, Math.floor(Number(state.skip) || 0)),
      limit: clampLimit(state.limit),
    },
    pipeline: pipeline && pipeline.length > 0 ? [...pipeline] : null,
    updatedAt: 0,
  };
}

/** A bar state handed over by another tab, checked (null when it is not one). */
export function readQueryRequest(value: unknown): QueryState | null {
  if (typeof value !== "object" || value === null) return null;
  const query = value as Record<string, unknown>;
  if (typeof query.filter !== "string") return null;
  return stateOfFields(query);
}

/** The bar state a saved query opens with; missing fields take their defaults. */
export function stateOfSaved(saved: SavedQuery): QueryState {
  return stateOfFields((saved.query ?? {}) as unknown as Record<string, unknown>);
}

function stateOfFields(query: Record<string, unknown>): QueryState {
  const text = (value: unknown) => (typeof value === "string" ? value : "");
  return {
    filter: text(query.filter),
    project: text(query.project),
    sort: text(query.sort),
    collation: text(query.collation),
    skip: Math.max(0, Math.floor(Number(query.skip) || 0)),
    limit: clampLimit(query.limit ?? EMPTY_QUERY.limit),
  };
}

export interface SavedGroups {
  /** Saved on this collection, by name. */
  here: SavedQuery[];
  /** Saved on other collections of the same database, by collection then name. */
  others: SavedQuery[];
}

/** The stages of a saved pipeline; null for a saved find. */
export function pipelineOfSaved(saved: SavedQuery): string[] | null {
  const stages = Array.isArray(saved.pipeline) ? saved.pipeline.filter((stage): stage is string => typeof stage === "string") : [];
  return stages.length > 0 ? stages : null;
}

/** One line of what a saved query does: `status: 'paid' · sort { total: -1 } · limit 50`, or `$match → $group (2 stages)`. */
export function savedSummary(saved: SavedQuery): string {
  const pipeline = pipelineOfSaved(saved);
  if (pipeline) return pipelineSummary(pipeline);
  const state = stateOfSaved(saved);
  const parts = [state.filter.trim() || "{}"];
  if (state.project.trim()) parts.push(`project ${state.project.trim()}`);
  if (state.sort.trim()) parts.push(`sort ${state.sort.trim()}`);
  if (state.collation.trim()) parts.push(`collation ${state.collation.trim()}`);
  if (state.skip > 0) parts.push(`skip ${state.skip}`);
  if (state.limit !== EMPTY_QUERY.limit) parts.push(`limit ${state.limit}`);
  return parts.join(" · ").replace(/\s+/g, " ");
}

/**
 * The saved queries the dropdown offers on `database.collection`: MongoDB ones of the database, this collection's
 * first; `search` (case-insensitive, every word) matches the name, the collection and the fields.
 */
export function groupSaved(list: readonly SavedQuery[], database: string, collection: string, search = ""): SavedGroups {
  const words = search.toLowerCase().split(/\s+/).filter(Boolean);
  const matches = (saved: SavedQuery) => {
    if (words.length === 0) return true;
    const haystack = `${saved.name} ${saved.collection} ${savedSummary(saved)}`.toLowerCase();
    return words.every((word) => haystack.includes(word));
  };
  const byName = (a: SavedQuery, b: SavedQuery) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
  const candidates = list.filter((saved) => saved.engine === ENGINE && saved.database === database && matches(saved));
  return {
    here: candidates.filter((saved) => saved.collection === collection).sort(byName),
    others: candidates.filter((saved) => saved.collection !== collection).sort((a, b) => a.collection.localeCompare(b.collection) || byName(a, b)),
  };
}

/** A name for a new saved query that no saved query of the collection has yet ("orders query", "orders query 2"). */
export function suggestName(list: readonly SavedQuery[], database: string, collection: string, noun = "query"): string {
  const taken = new Set(list.filter((saved) => saved.database === database && saved.collection === collection).map((saved) => saved.name.toLowerCase()));
  const base = `${collection} ${noun}`;
  if (!taken.has(base.toLowerCase())) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base} ${n}`.toLowerCase())) return `${base} ${n}`;
}

/** A saved query of the same collection with this name (case-insensitive), other than `exceptId`. */
export function sameName(list: readonly SavedQuery[], saved: Pick<SavedQuery, "database" | "collection" | "name">, exceptId = ""): SavedQuery | null {
  const name = saved.name.trim().toLowerCase();
  return (
    list.find(
      (item) => item.id !== exceptId && item.database === saved.database && item.collection === saved.collection && item.name.trim().toLowerCase() === name,
    ) ?? null
  );
}

// --- Query history → workspace ---------------------------------------------------------------------------------------

/** What the Aggregations section is handed (`receive`): stage texts to open (a saved pipeline, a Documents query). */
export interface AggregationsRequest {
  pipeline: string[];
}

/** What a collection workspace's Documents section is handed (`receive`): a filter, or the whole bar state. */
export interface DocumentsRequest {
  filter?: string;
  query?: QueryState;
}

export interface WorkspaceTarget {
  label: string;
  connectionId: string;
  object: ObjectRef;
  section: string;
  request: DocumentsRequest;
}

const COLLECTION_CALL = /^db\.getCollection\(("(?:[^"\\]|\\.)*")\)\.find\(/;

/**
 * The collection workspace a query-history entry of the query bar reopens in (its command parsed back into the bar
 * state), or null for anything else (a console command, another engine, a database-less entry).
 */
export function historyTarget(entry: HistoryEntry): WorkspaceTarget | null {
  if (entry.driver !== ENGINE || !entry.database) return null;
  const match = COLLECTION_CALL.exec(entry.sql);
  if (!match) return null;
  let collection: string;
  try {
    collection = JSON.parse(match[1]) as string;
  } catch {
    return null;
  }
  const query = parseCommandText(entry.sql, collection);
  if (!query) return null;
  return {
    label: "Open in collection",
    connectionId: entry.connectionId,
    object: { database: entry.database, schema: null, name: collection, kind: "collection" },
    section: "documents",
    request: { query },
  };
}
