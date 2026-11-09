import { bindConnection } from "../core/connectionApi";
import type { ConnectInfo, ObjectNode, ObjectRef } from "../core/contract";
import { toast } from "../core/dialogs";
import type { IconName } from "../core/dom";
import { connectInfo, engineOf, hasCapability, type EngineInfo } from "../core/engines";
import { showError } from "../core/errorPanel";
import type { Navigator } from "../core/navigator";
import type { TabContent, TabManager } from "../core/tabs";
import type { ConnectionProfile, KeyCell, TableTarget, TaskDef } from "../core/types";
import { nodeIcon, resolveView, type ViewContext, type ViewRegistration, type ViewRole } from "../core/views";
import type { QueryHost, QueryView } from "../features/query/queryView";
import { createLogView, type TasksPanel } from "../features/tasks/tasksPanel";
import { VectorHealthView } from "../features/vector/vectorHealth";
import { VectorSearchView } from "../features/vector/vectorSearch";
import { captureTyping } from "./typingBuffer";

/** The query view chunk (CodeMirror). Loaded on first use, or ahead of it by `preloadQueryView`. */
function loadQueryView(): Promise<typeof import("../features/query/queryView")> {
  return import("../features/query/queryView");
}

/** Fetches the query view chunk while the app is idle, so the first Ctrl+T opens the editor at once. */
export function preloadQueryView(): void {
  const load = () => void loadQueryView().catch(() => undefined);
  if ("requestIdleCallback" in window) window.requestIdleCallback(load, { timeout: 5000 });
  else setTimeout(load, 2000);
}

export interface NavigationHost {
  /** Tab keys start with the workspace id, so each workspace keeps its own tabs. */
  workspaceId(): string;
  /** A saved connection of any workspace. */
  profileOf(connectionId: string): ConnectionProfile | undefined;
  /** The panel that streams task logs into log tabs. */
  tasksPanel(): TasksPanel;
  configureEmbedding(): void;
  /** A query tab's text or result changed. */
  queryChanged(view: QueryView): void;
  /** Display name of a saved connection, recorded in the query history. */
  connectionName(connectionId: string): string;
  /** Whether the connection's safety rules block every write. */
  writesBlocked(connectionId: string): boolean;
  /** Whether query runs are recorded (the `queryHistory` setting). */
  historyEnabled(): boolean;
}

/** What `openObject` opens: a tree node, an object reference, or nothing (the connection itself: a console). */
export type OpenTarget = ObjectNode | ObjectRef | null;

/** Extras of `openObject`; the known ones also shape the tab key and title. */
export interface OpenParams extends Record<string, unknown> {
  /** A data tab's raw filter (part of its key: the same table with another filter is another tab). */
  filter?: string;
  /** The filter comes from the assistant and is only pre-filled. */
  suggested?: boolean;
  /** A data tab's column filter boxes (column → filter text), also part of its key. */
  columnFilters?: Record<string, string>;
  /** The database a console or diagram works in. */
  database?: string | null;
}

function isNode(target: OpenTarget): target is ObjectNode {
  return target !== null && "key" in target && "label" in target;
}

/** "shop/products", "ERP/dbo/Customers": the part of a tab key that names an object, the same from the tree or a link. */
export function objectIdentity(object: ObjectRef): string {
  return [object.database ?? "", object.schema ?? "", object.name, ...(object.path ?? [])].join("/");
}

/**
 * The object a `TableTarget` (the pre-contract address: database + the tree label) stands for. Engines with schemas
 * label tables `schema.table`, so the first dot splits there; elsewhere a dot is part of the name.
 */
export function objectOfTarget(engine: EngineInfo, target: TableTarget): ObjectRef {
  const dot = target.table.indexOf(".");
  const split = hasCapability(engine, "schemas") && dot > 0 && dot < target.table.length - 1;
  const kind = target.kind === "view" ? "view" : engine.family === "document" ? "collection" : "table";
  return {
    database: target.database,
    schema: split ? target.table.slice(0, dot) : null,
    name: split ? target.table.slice(dot + 1) : target.table,
    kind,
  };
}

/** What a view gets for `info` before the connection reported its own (opening a view of a closed connection). */
function placeholderInfo(engine: EngineInfo): ConnectInfo {
  return {
    engine: engine.id,
    server: { product: engine.label, version: "", label: "", flavor: null },
    capabilities: engine.capabilities,
    languages: engine.languages,
    connectionActions: [],
  };
}

/** Duck-typed so the CodeMirror chunk stays out of the main bundle. */
function asQueryView(content: TabContent | undefined): QueryView | null {
  return content && typeof (content as QueryView).appendTyped === "function" ? (content as QueryView) : null;
}

/**
 * Opens (or re-focuses) the tabs of the main area. A tab key is `workspace|connection|role|<object identity or node id>|…`,
 * so opening the same thing twice selects the existing tab and closing a connection or a workspace can find its tabs.
 * Every object view goes through the view registry (`openObject`); `openTable`, `openDesign`, `openQuery` and
 * `openDiagram` are shorthands for it.
 */
export class AppNavigation implements Navigator {
  private queryCounter = 0;

  constructor(
    private readonly tabs: TabManager,
    private readonly host: NavigationHost,
  ) {}

  tabKey(connectionId: string, kind: string, ...parts: string[]): string {
    return [this.host.workspaceId(), connectionId, kind, ...parts].join("|");
  }

  writesBlocked(connectionId: string): boolean {
    return this.host.writesBlocked(connectionId);
  }

  closeConnectionTabs(connectionId: string): void {
    this.tabs.closeWhere((key) => key.split("|")[1] === connectionId);
  }

  closeWorkspaceTabs(workspaceId: string): void {
    this.tabs.closeWhere((key) => key.startsWith(`${workspaceId}|`));
  }

  /**
   * Opens the view registered for the target's kind and `role` (`resolveView`), with a `ViewContext` bound to the
   * connection. Resolves with the tab content (an existing tab is selected and returned), or null when no view is
   * registered or creating it failed (reported in the error panel).
   */
  async openObject(profile: ConnectionProfile, target: OpenTarget, role: ViewRole, params: OpenParams = {}): Promise<TabContent | null> {
    const scope = this.host.workspaceId();
    const engine = engineOf(profile);
    const node = isNode(target) ? target : null;
    const object = node ? node.object : (target as ObjectRef | null);
    const kind = node?.kind ?? object?.kind ?? "connection";
    const identity = object ? objectIdentity(object) : (node?.key.id ?? "");
    const key = this.objectKey(profile.id, role, identity, params);
    const existing = this.tabs.contentOf(key);
    if (existing) {
      this.tabs.open({ key, title: "", icon: "table", create: () => existing });
      return existing;
    }
    let registration: ViewRegistration | null;
    try {
      registration = await resolveView(engine, kind, role);
    } catch (error) {
      showError(error, `Load the ${engine.label} views`);
      return null;
    }
    if (!registration) {
      toast(`${engine.label} has no ${role} view for ${kind === "connection" ? "a connection" : `a ${kind}`}`);
      return null;
    }
    const ctx: ViewContext = {
      connection: bindConnection(profile.id),
      profile,
      engine,
      info: connectInfo(profile.id) ?? placeholderInfo(engine),
      object,
      node,
      navigator: this,
      params,
    };
    try {
      const factory = await registration.load();
      const name = node?.label ?? (object ? (object.schema ? `${object.schema}.${object.name}` : object.name) : profile.name);
      const { title, subtitle } = this.describeTab(registration, ctx, role, name, params);
      const tabIcon: IconName = role === "data" ? nodeIcon(node?.icon, dataIcon(kind)) : (registration.icon ?? "table");
      const database = object?.database ?? (typeof params.database === "string" ? params.database : null);
      this.tabs.open({ key, scope, title, subtitle, icon: tabIcon, connection: profile.name, database, create: () => factory(ctx) });
    } catch (error) {
      showError(error, `Open ${describeOpen(role, kind)}`);
      return null;
    }
    return this.tabs.contentOf(key) ?? null;
  }

  /** `suggested`: the filter comes from the assistant and is only pre-filled; the user applies it. */
  openTable(target: TableTarget, filter = "", suggested = false): void {
    const profile = this.profile(target.connectionId);
    if (!profile) return;
    void this.openObject(profile, objectOfTarget(engineOf(profile), target), "data", { filter, suggested });
  }

  openData(connectionId: string, object: ObjectRef, columnFilters: Record<string, string>): void {
    const profile = this.profile(connectionId);
    if (!profile) return;
    void this.openObject(profile, object, "data", { columnFilters });
  }

  openDesign(target: TableTarget): void {
    const profile = this.profile(target.connectionId);
    if (!profile) return;
    void this.openObject(profile, objectOfTarget(engineOf(profile), target), "design");
  }

  /** Opens the ER diagram of `database`, optionally centred on one table. */
  openDiagram(profile: ConnectionProfile, database: string, focusTable?: string): void {
    // The diagram needs the `schemaGraph` capability (of the connection once it is connected, else of the engine).
    const info = connectInfo(profile.id);
    const engine = engineOf(profile);
    if (!hasCapability(info ?? engine, "schemaGraph")) {
      toast(`${engine.label} has no schema diagram`);
      return;
    }
    const object: ObjectRef = { database, schema: null, name: database, kind: "database" };
    void this.openObject(profile, object, "diagram", { database }).then((view) => {
      const diagram = view as (TabContent & { focus?: (name: string) => void }) | null;
      if (focusTable && typeof diagram?.focus === "function") diagram.focus(focusTable);
    });
  }

  openQuery(connectionId: string, database: string | null, initialSql = ""): void {
    void this.openQueryTab(connectionId, database, initialSql);
  }

  openVectorSearch(target: TableTarget, column: string, seed?: KeyCell[]): void {
    const key = this.tabKey(target.connectionId, "vector-search", target.database, target.table);
    const existing = this.tabs.contentOf(key);
    if (existing instanceof VectorSearchView && seed) existing.setSeed(column, seed);
    this.tabs.open({
      key,
      title: `${target.table} · vector search`,
      subtitle: `${target.database}.${target.table}`,
      connection: this.host.connectionName(target.connectionId),
      database: target.database,
      icon: "vector",
      create: () => new VectorSearchView(target, column, this, seed ?? null),
    });
  }

  openVectorHealth(target: TableTarget, column: string): void {
    this.tabs.open({
      key: this.tabKey(target.connectionId, "vector-health", target.database, target.table),
      title: `${target.table} · vector health`,
      subtitle: `${target.database}.${target.table}`,
      connection: this.host.connectionName(target.connectionId),
      database: target.database,
      icon: "health",
      create: () => new VectorHealthView(target, column),
    });
  }

  configureEmbedding(): void {
    this.host.configureEmbedding();
  }

  openLogs(task: TaskDef): void {
    this.tabs.open({
      key: this.tabKey("task", "logs", task.id),
      title: `${task.name} · logs`,
      icon: "logs",
      create: () => createLogView(task, this.host.tasksPanel()),
    });
  }

  private profile(connectionId: string): ConnectionProfile | null {
    const profile = this.host.profileOf(connectionId);
    if (!profile) toast("That connection is no longer saved");
    return profile ?? null;
  }

  /** `workspace|connection|role|identity`, plus the filter for data tabs and a fresh id for each console. */
  private objectKey(connectionId: string, role: ViewRole, identity: string, params: OpenParams): string {
    if (role === "console") return this.tabKey(connectionId, "console", crypto.randomUUID());
    if (role === "data") {
      const filter = [params.filter ?? "", columnFiltersText(params.columnFilters)].filter(Boolean).join(" ");
      if (params.suggested) return this.tabKey(connectionId, "data-suggested", identity, filter, crypto.randomUUID());
      return this.tabKey(connectionId, "data", identity, filter);
    }
    return this.tabKey(connectionId, role, identity);
  }

  private describeTab(
    registration: ViewRegistration,
    ctx: ViewContext,
    role: ViewRole,
    name: string,
    params: OpenParams,
  ): { title: string; subtitle?: string } {
    const object = ctx.object;
    const where = object?.database && object.kind !== "database" ? `${object.database}.${name}` : name;
    switch (role) {
      case "data": {
        const filter = params.filter ?? "";
        const columns = columnFiltersText(params.columnFilters);
        const title = params.suggested ? `${name} (suggested filter)` : filter || columns ? `${name} (filtered)` : name;
        return { title, subtitle: filter ? `${where} WHERE ${filter}` : columns ? `${where} · ${columns}` : where };
      }
      case "design":
        return { title: `${name} · design`, subtitle: where };
      case "diagram": {
        const database = params.database ?? object?.database ?? name;
        return { title: `${database} · diagram`, subtitle: `${ctx.profile.name} · ${database}` };
      }
      case "console":
        return {
          title: typeof params.title === "string" ? params.title : (registration.title?.(ctx) ?? "Query"),
          subtitle: params.database ?? object?.database ?? undefined,
        };
      default:
        return { title: registration.title?.(ctx) ?? `${name} · ${role}`, subtitle: where };
    }
  }

  /** The query view pulls in CodeMirror, so it is loaded on first use (its own chunk). */
  private async openQueryTab(connectionId: string, database: string | null, initialSql: string): Promise<void> {
    const profile = this.profile(connectionId);
    if (!profile) return;
    // Typing that starts before the chunk arrives goes into the new editor instead of being lost.
    const stopCapture = captureTyping();
    let typed = "";
    try {
      await loadQueryView();
    } catch (error) {
      showError(error, "Load the query editor");
    } finally {
      typed = stopCapture();
    }
    this.queryCounter += 1;
    const host: QueryHost = {
      connectionName: (id) => this.host.connectionName(id),
      historyEnabled: () => this.host.historyEnabled(),
      openQuery: (id, db, sql) => this.openQuery(id, db, sql),
    };
    const counter = this.queryCounter;
    const view = asQueryView((await this.openObject(profile, null, "console", { database, text: initialSql, host, title: `Query ${counter}` })) ?? undefined);
    if (!view) return;
    view.onChanged = () => this.host.queryChanged(view);
    if (typed) view.appendTyped(typed);
  }
}

function dataIcon(kind: string): IconName {
  return kind === "view" || kind === "materializedView" ? "view" : "table";
}

function describeOpen(role: ViewRole, kind: string): string {
  return role === "console" ? "a query tab" : `the ${role} view of a ${kind}`;
}

/** "customer_id =42, status null": column filters for a tab key and subtitle. */
function columnFiltersText(filters: Record<string, string> | undefined): string {
  return Object.entries(filters ?? {})
    .map(([column, text]) => `${column} ${text}`)
    .join(", ");
}
