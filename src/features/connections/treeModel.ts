/**
 * The object tree of one connection as plain data, without DOM: which nodes are loaded under which parent (`tree_children`
 * pages merged in order), which are expanded, and the flat list of rows a search filter leaves visible. Node ids are the
 * engine's opaque, stable `NodeKey.id`, so expansion and selection survive a refresh.
 */
import type { ObjectNode, ObjectRef, TreeChildren } from "../../core/contract";
import { quoteIdent, type SqlDialect } from "../../core/sql";

/** The parent id of the connection's top level (`TreeRequest.parent: null`). */
export const ROOT = "";

export interface ChildList {
  ids: string[];
  nextCursor: string | null;
  totalHint: number | null;
  /** At least one page arrived. */
  loaded: boolean;
  loading: boolean;
  error: string | null;
}

export type VisibleRow =
  | { type: "node"; node: ObjectNode; depth: number; parentId: string; expanded: boolean; count: string | null }
  | { type: "loading" | "empty"; parentId: string; depth: number }
  | { type: "error"; parentId: string; depth: number; message: string }
  | { type: "more"; parentId: string; depth: number; loading: boolean; loaded: number; total: number | null };

export class TreeModel {
  private readonly nodes = new Map<string, ObjectNode>();
  private readonly parents = new Map<string, string>();
  private readonly lists = new Map<string, ChildList>();
  private readonly expanded = new Set<string>();

  node(id: string): ObjectNode | undefined {
    return this.nodes.get(id);
  }

  parentOf(id: string): string | undefined {
    return this.parents.get(id);
  }

  list(parentId: string): ChildList | undefined {
    return this.lists.get(parentId);
  }

  children(parentId: string): ObjectNode[] {
    return (this.lists.get(parentId)?.ids ?? []).flatMap((id) => this.nodes.get(id) ?? []);
  }

  isExpanded(id: string): boolean {
    return this.expanded.has(id);
  }

  setExpanded(id: string, open: boolean): void {
    if (open) this.expanded.add(id);
    else this.expanded.delete(id);
  }

  /** Whether expanding `parentId` has to fetch its first page. */
  needsLoad(parentId: string): boolean {
    const list = this.lists.get(parentId);
    return !list || (!list.loaded && !list.loading);
  }

  beginLoad(parentId: string): void {
    const list = this.lists.get(parentId) ?? emptyList();
    list.loading = true;
    list.error = null;
    this.lists.set(parentId, list);
  }

  fail(parentId: string, message: string): void {
    const list = this.lists.get(parentId) ?? emptyList();
    list.loading = false;
    list.error = message;
    this.lists.set(parentId, list);
  }

  /**
   * Stores a page of children. `append` adds a "Load more" page after the ones already there (a node seen before is
   * updated in place); otherwise the list is replaced (a refresh) and nodes that are gone are forgotten with their
   * subtrees, while expanded nodes that are still there stay expanded.
   */
  receive(parentId: string, page: TreeChildren, append: boolean): void {
    const previous = this.lists.get(parentId);
    const ids = append && previous ? [...previous.ids] : [];
    const seen = new Set(ids);
    for (const node of page.nodes) {
      const id = node.key.id;
      this.nodes.set(id, node);
      this.parents.set(id, parentId);
      if (!seen.has(id)) {
        seen.add(id);
        ids.push(id);
      }
    }
    if (!append && previous) for (const id of previous.ids) if (!seen.has(id)) this.forget(id);
    this.lists.set(parentId, { ids, nextCursor: page.nextCursor, totalHint: page.totalHint, loaded: true, loading: false, error: null });
  }

  /** How many children a refresh of `parentId` should ask for, so the pages loaded so far stay loaded. */
  reloadLimit(parentId: string, pageSize: number): number {
    return Math.max(pageSize, this.lists.get(parentId)?.ids.length ?? 0);
  }

  /** `parentId` and every expanded, already loaded node below it: what a refresh reloads, parents first. */
  reloadTargets(parentId: string): string[] {
    const targets: string[] = [];
    const visit = (id: string) => {
      const list = this.lists.get(id);
      if (!list?.loaded) return;
      targets.push(id);
      for (const child of list.ids) if (this.expanded.has(child)) visit(child);
    };
    visit(parentId);
    return targets;
  }

  /** Forgets everything (disconnect). */
  clear(): void {
    this.nodes.clear();
    this.parents.clear();
    this.lists.clear();
    this.expanded.clear();
  }

  /**
   * The rows to draw, depth-first. Without a query: expanded nodes show their children (or a loading / empty / error
   * row) and a "Load more" row when the engine has more. With a query (case-insensitive substring of the label): a
   * matching node shows with its subtree as usual; a node that does not match shows only when a loaded descendant
   * matches (opened, whatever its expansion) or when its children are not loaded yet (they might match).
   */
  rows(query = ""): VisibleRow[] {
    const needle = query.trim().toLowerCase();
    const rows: VisibleRow[] = [];
    const memo = new Map<string, boolean>();
    const matches = (node: ObjectNode) => node.label.toLowerCase().includes(needle);
    const hasMatch = (id: string): boolean => {
      const known = memo.get(id);
      if (known !== undefined) return known;
      const result = this.children(id).some((child) => matches(child) || hasMatch(child.key.id));
      memo.set(id, result);
      return result;
    };

    const walk = (parentId: string, depth: number, filtered: boolean) => {
      const list = this.lists.get(parentId);
      if (!list || (!list.loaded && !list.loading && !list.error)) return;
      if (list.error && list.ids.length === 0) {
        rows.push({ type: "error", parentId, depth, message: list.error });
        return;
      }
      if (!list.loaded) {
        rows.push({ type: "loading", parentId, depth });
        return;
      }
      if (list.ids.length === 0 && !list.nextCursor) {
        if (!filtered) rows.push({ type: "empty", parentId, depth });
        return;
      }
      for (const node of this.children(parentId)) {
        const id = node.key.id;
        const childList = this.lists.get(id);
        let showChildren = this.expanded.has(id);
        let childFilter = false;
        if (filtered && !matches(node)) {
          if (childList?.loaded) {
            if (!hasMatch(id)) continue;
            showChildren = true;
            childFilter = true;
          } else if (!node.expandable) {
            continue;
          }
        }
        rows.push({ type: "node", node, depth, parentId, expanded: showChildren && node.expandable, count: countLabel(childList) });
        if (showChildren && node.expandable) walk(id, depth + 1, childFilter);
      }
      if (list.nextCursor) rows.push({ type: "more", parentId, depth, loading: list.loading, loaded: list.ids.length, total: list.totalHint });
    };

    walk(ROOT, 0, needle !== "");
    return rows;
  }

  private forget(id: string): void {
    for (const child of this.lists.get(id)?.ids ?? []) this.forget(child);
    this.nodes.delete(id);
    this.parents.delete(id);
    this.lists.delete(id);
    this.expanded.delete(id);
  }
}

function emptyList(): ChildList {
  return { ids: [], nextCursor: null, totalHint: null, loaded: false, loading: false, error: null };
}

/** "12", or "200+" while more pages wait; null until loaded. */
function countLabel(list: ChildList | undefined): string | null {
  if (!list?.loaded) return null;
  if (list.totalHint !== null && list.totalHint >= list.ids.length) return String(list.totalHint);
  return list.nextCursor ? `${list.ids.length}+` : String(list.ids.length);
}

// --- Action templates ---------------------------------------------------------------------------------------------------

/**
 * Fills a `query` action's template. `{name}`, `{schema}` and `{database}` are the object's parts and `{qualified}` is
 * `schema.name` (or `database.name` when there is no schema); for SQL engines (`dialect` set) each part is quoted with
 * the engine's identifier quoting, for other engines the parts go in as they are.
 */
export function fillTemplate(template: string, object: ObjectRef | null, dialect: SqlDialect | null, database: string | null = null): string {
  const quote = (text: string) => (dialect ? quoteIdent(text, dialect) : text);
  const name = object?.name ?? "";
  const schema = object?.schema ?? null;
  const db = object?.database ?? database;
  const parts: Record<string, string> = {
    name: quote(name),
    schema: schema === null ? "" : quote(schema),
    database: db === null ? "" : quote(db),
    qualified: [schema ?? (object?.kind === "database" ? null : db), name].flatMap((part) => (part ? [quote(part)] : [])).join("."),
  };
  return template.replace(/\{(name|schema|database|qualified)\}/g, (_, key: string) => parts[key]);
}

/** A node's name as "Copy name" copies it: `schema.name`, else the name, else the label. */
export function plainName(node: Pick<ObjectNode, "label" | "object">): string {
  const object = node.object;
  if (!object) return node.label;
  return object.schema ? `${object.schema}.${object.name}` : object.name;
}
