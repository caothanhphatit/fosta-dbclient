import type { Navigator } from "../core/navigator";
import type { TabManager } from "../core/tabs";
import type { CellValue, ConnectionProfile, DriverId, TableTarget, TaskDef } from "../core/types";
import { DesignView } from "../features/design/designView";
import { DiagramView } from "../features/diagram/diagramView";
import type { QueryView } from "../features/query/queryView";
import { TableView } from "../features/table/tableView";
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
  /** Driver of a saved connection; picks the query editor's dialect. */
  driverOf(connectionId: string): DriverId | undefined;
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

/**
 * Opens (or re-focuses) the tabs of the main area. A tab key is
 * `workspace|connection|kind|…`, so opening the same thing twice selects the
 * existing tab and closing a connection or a workspace can find its tabs.
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

  /** `suggested`: the filter comes from the assistant and is only pre-filled; the user applies it. */
  openTable(target: TableTarget, filter = "", suggested = false): void {
    const kind = suggested ? "data-suggested" : "data";
    this.tabs.open({
      key: this.tabKey(target.connectionId, kind, target.database, target.table, filter, suggested ? crypto.randomUUID() : ""),
      title: suggested ? `${target.table} (suggested filter)` : filter ? `${target.table} (filtered)` : target.table,
      subtitle: filter ? `${target.database}.${target.table} WHERE ${filter}` : `${target.database}.${target.table}`,
      icon: target.kind === "view" ? "view" : "table",
      create: () => new TableView(target, this, filter, suggested),
    });
  }

  openVectorSearch(target: TableTarget, column: string, seed?: CellValue[]): void {
    const key = this.tabKey(target.connectionId, "vector-search", target.database, target.table);
    const existing = this.tabs.contentOf(key);
    if (existing instanceof VectorSearchView && seed) existing.setSeed(column, seed);
    this.tabs.open({
      key,
      title: `${target.table} · vector search`,
      subtitle: `${target.database}.${target.table}`,
      icon: "vector",
      create: () => new VectorSearchView(target, column, this, seed ?? null),
    });
  }

  openVectorHealth(target: TableTarget, column: string): void {
    this.tabs.open({
      key: this.tabKey(target.connectionId, "vector-health", target.database, target.table),
      title: `${target.table} · vector health`,
      subtitle: `${target.database}.${target.table}`,
      icon: "health",
      create: () => new VectorHealthView(target, column),
    });
  }

  configureEmbedding(): void {
    this.host.configureEmbedding();
  }

  openDesign(target: TableTarget): void {
    this.tabs.open({
      key: this.tabKey(target.connectionId, "design", target.database, target.table),
      title: `${target.table} · design`,
      subtitle: `${target.database}.${target.table}`,
      icon: "design",
      create: () => new DesignView(target),
    });
  }

  /** Opens the ER diagram of `database`, optionally centred on one table. */
  openDiagram(profile: ConnectionProfile, database: string, focusTable?: string): void {
    const key = this.tabKey(profile.id, "diagram", database);
    this.tabs.open({
      key,
      title: `${database} · diagram`,
      subtitle: `${profile.name} · ${database}`,
      icon: "diagram",
      create: () => new DiagramView(profile.id, profile.driver, database, this),
    });
    const view = this.tabs.contentOf(key);
    if (focusTable && view instanceof DiagramView) view.focus(focusTable);
  }

  openQuery(connectionId: string, database: string | null, initialSql = ""): void {
    void this.openQueryTab(connectionId, database, initialSql);
  }

  openLogs(task: TaskDef): void {
    this.tabs.open({
      key: this.tabKey("task", "logs", task.id),
      title: `${task.name} · logs`,
      icon: "logs",
      create: () => createLogView(task, this.host.tasksPanel()),
    });
  }

  /** The query view pulls in CodeMirror, so it is loaded on first use (its own chunk). */
  private async openQueryTab(connectionId: string, database: string | null, initialSql: string): Promise<void> {
    // Typing that starts before the chunk arrives goes into the new editor instead of being lost.
    const stopCapture = captureTyping();
    let typed = "";
    let QueryView: typeof import("../features/query/queryView").QueryView;
    try {
      ({ QueryView } = await loadQueryView());
    } finally {
      typed = stopCapture();
    }
    const driver = this.host.driverOf(connectionId) ?? "mariadb";
    this.queryCounter += 1;
    this.tabs.open({
      key: this.tabKey(connectionId, "query", crypto.randomUUID()),
      title: `Query ${this.queryCounter}`,
      subtitle: database ?? undefined,
      icon: "query",
      create: () => {
        const view = new QueryView(connectionId, driver, database, initialSql, {
          connectionName: (id) => this.host.connectionName(id),
          historyEnabled: () => this.host.historyEnabled(),
          openQuery: (id, db, sql) => this.openQuery(id, db, sql),
        });
        view.onChanged = () => this.host.queryChanged(view);
        if (typed) view.appendTyped(typed);
        return view;
      },
    });
  }
}
