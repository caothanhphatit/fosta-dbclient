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

  closeConnectionTabs(connectionId: string): void {
    this.tabs.closeWhere((key) => key.split("|")[1] === connectionId);
  }

  closeWorkspaceTabs(workspaceId: string): void {
    this.tabs.closeWhere((key) => key.startsWith(`${workspaceId}|`));
  }

  openTable(target: TableTarget, filter = ""): void {
    this.tabs.open({
      key: this.tabKey(target.connectionId, "data", target.database, target.table, filter),
      title: filter ? `${target.table} (filtered)` : target.table,
      subtitle: filter ? `${target.database}.${target.table} WHERE ${filter}` : `${target.database}.${target.table}`,
      icon: target.kind === "view" ? "view" : "table",
      create: () => new TableView(target, this, filter),
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
    const { QueryView } = await import("../features/query/queryView");
    const driver = this.host.driverOf(connectionId) ?? "mariadb";
    this.queryCounter += 1;
    this.tabs.open({
      key: this.tabKey(connectionId, "query", crypto.randomUUID()),
      title: `Query ${this.queryCounter}`,
      subtitle: database ?? undefined,
      icon: "query",
      create: () => {
        const view = new QueryView(connectionId, driver, database, initialSql);
        view.onChanged = () => this.host.queryChanged(view);
        return view;
      },
    });
  }
}
