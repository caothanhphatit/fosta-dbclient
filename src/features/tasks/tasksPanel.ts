/**
 * The workspace's Commands panel ("tasks" in the config file): each command with its state, who uses it, start/stop,
 * logs. A command starts with the workspace, by hand, or when a connection that uses it connects ("for connections";
 * it stops again with the last of them).
 */
import { showError } from "../../core/errorPanel";
import { api, onTaskLog, onTaskStatus } from "../../core/api";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { confirmDialog } from "../../core/dialogs";
import { h, icon, iconButton, spacer, toolButton } from "../../core/dom";
import { taskDialog, type CommandDialogOptions, type SavedCommand } from "./taskDialog";
import { commandUsers } from "./commandForm";
import { formatArgs } from "../../core/shell";
import type { TabContent } from "../../core/tabs";
import type { AppConfig, ConnectionProfile, TaskDef, TaskState, TaskStatus } from "../../core/types";

export interface TasksHost {
  openLogs(task: TaskDef): void;
  configChanged(config: AppConfig): void;
  summaryChanged(text: string): void;
  workspaceName(): string;
  /** Fired whenever any command (in any workspace) changes state. */
  activityChanged?(): void;
  /** Opens the assistant on a command's log. */
  askAssistant?(task: TaskDef, logLines: string[]): void;
}

/** What a log tab exposes so the assistant can read the command's output. */
export interface LogTabContent extends TabContent {
  task: TaskDef;
  logLines(): string[];
}

type LogListener = (line: string) => void;

export class TasksPanel {
  readonly element: HTMLElement;
  private readonly host: TasksHost;
  private readonly list = h("div", { class: "task-list" });
  private readonly badge = h("span", { class: "panel-badge" });
  private readonly statuses = new Map<string, TaskStatus>();
  private readonly logListeners = new Map<string, Set<LogListener>>();
  private tasks: TaskDef[] = [];
  private connections: ConnectionProfile[] = [];

  constructor(host: TasksHost) {
    this.host = host;
    const header = h(
      "div",
      { class: "panel-header collapsible" },
      icon("chevronDown", "panel-chevron"),
      h("span", {}, "Commands"),
      this.badge,
      spacer(),
      iconButton("plus", "New command", () => void this.edit(null)),
    );
    header.addEventListener("click", () => this.element.classList.toggle("collapsed"));
    this.element = h("section", { class: "tasks-panel" }, header, this.list);
  }

  async init(): Promise<void> {
    await onTaskStatus((status) => {
      this.statuses.set(status.id, status);
      this.render();
    });
    await onTaskLog((event) => this.logListeners.get(event.id)?.forEach((listener) => listener(event.line)));
    await this.refreshStatuses();
  }

  /** The active workspace's commands, and its connections (to show who uses which command). */
  setTasks(tasks: TaskDef[], connections: ConnectionProfile[] = []): void {
    this.tasks = tasks;
    this.connections = connections;
    this.render();
  }

  async refreshStatuses(): Promise<void> {
    try {
      for (const status of await api.taskStatuses()) this.statuses.set(status.id, status);
      this.render();
    } catch (error) {
      showError(error, "Read command statuses");
    }
  }

  /** Ids of every command (in any workspace) that is starting, running or restarting. */
  runningIds(): Set<string> {
    return new Set([...this.statuses.values()].filter((status) => isActive(status.state)).map((status) => status.id));
  }

  stateOf(id: string): TaskState | undefined {
    return this.statuses.get(id)?.state;
  }

  subscribeLogs(id: string, listener: LogListener): () => void {
    const listeners = this.logListeners.get(id) ?? new Set<LogListener>();
    listeners.add(listener);
    this.logListeners.set(id, listeners);
    return () => void listeners.delete(listener);
  }

  /** Opens the command dialog; returns the saved command. */
  async edit(existing: TaskDef | null, options: CommandDialogOptions = {}): Promise<SavedCommand | null> {
    const saved = await taskDialog(existing, this.host.workspaceName(), options);
    if (saved) this.host.configChanged(saved.config);
    return saved;
  }

  canAsk(): boolean {
    return Boolean(this.host.askAssistant);
  }

  ask(task: TaskDef, logLines: string[]): void {
    this.host.askAssistant?.(task, logLines);
  }

  /** The toolbar's Commands menu: each command with its state and actions, then "New command…". */
  menuItems(): MenuItem[] {
    const items: MenuItem[] = this.tasks.map((task) => {
      const status = this.statuses.get(task.id);
      const state = status?.state ?? "stopped";
      const active = isActive(state);
      return {
        label: task.name,
        dot: state,
        detail: describeStatus(state, status),
        run: () => this.host.openLogs(task),
        actions: [
          active
            ? { icon: "stop", title: "Stop", run: () => void this.toggle(task, true) }
            : { icon: "play", title: "Start", run: () => void this.toggle(task, false) },
          { icon: "refresh", title: "Restart", run: () => void attempt(() => api.restartTask(task.id)) },
          { icon: "logs", title: "Show log", run: () => this.host.openLogs(task) },
        ],
      };
    });
    if (items.length) items.push("separator");
    items.push({ label: "New command…", icon: "plus", run: () => void this.edit(null) });
    return items;
  }

  private render(): void {
    const rows = this.tasks.map((task) => this.renderTask(task));
    this.list.replaceChildren(...(rows.length ? rows : [h("div", { class: "empty-hint" }, "No commands in this workspace")]));
    const running = this.tasks.filter((task) => this.statuses.get(task.id)?.state === "running").length;
    this.badge.textContent = this.tasks.length ? `${running}/${this.tasks.length}` : "";
    this.host.summaryChanged(this.tasks.length ? `Commands ${running}/${this.tasks.length} running` : "");
    this.host.activityChanged?.();
  }

  private renderTask(task: TaskDef): HTMLElement {
    const status = this.statuses.get(task.id);
    const state = status?.state ?? "stopped";
    const active = isActive(state);
    const users = commandUsers(task.id, this.connections);
    const usedBy = users.length
      ? h("span", { class: "task-users", title: `Used by: ${users.map((profile) => profile.name).join(", ")}` }, `used by ${users.length}`)
      : null;
    const row = h(
      "div",
      { class: `task-row state-${state}`, title: `${task.program} ${formatArgs(task.args)}` },
      h("span", { class: "status-dot" }),
      h(
        "div",
        { class: "task-text" },
        h("div", { class: "task-name" }, h("span", {}, task.name), usedBy),
        h("div", { class: "task-state" }, describeStatus(state, status)),
      ),
      h(
        "div",
        { class: "task-actions" },
        iconButton(active ? "stop" : "play", active ? "Stop" : "Start", () => void this.toggle(task, active)),
        iconButton("logs", "Show logs", () => this.host.openLogs(task)),
      ),
    );
    row.addEventListener("dblclick", () => this.host.openLogs(task));
    row.addEventListener("contextmenu", (event) => showContextMenu(event, this.menu(task, active)));
    return row;
  }

  private menu(task: TaskDef, active: boolean): MenuItem[] {
    return [
      active
        ? { label: "Stop", icon: "stop", run: () => void this.toggle(task, true) }
        : { label: "Start", icon: "play", run: () => void this.toggle(task, false) },
      { label: "Restart", icon: "refresh", run: () => void attempt(() => api.restartTask(task.id)) },
      { label: "Show logs", icon: "logs", run: () => this.host.openLogs(task) },
      "separator",
      { label: "Edit…", icon: "edit", run: () => void this.edit(task) },
      { label: "Delete", icon: "trash", danger: true, run: () => void this.remove(task) },
    ];
  }

  private toggle(task: TaskDef, active: boolean): Promise<void> {
    return attempt(() => (active ? api.stopTask(task.id) : api.startTask(task.id)));
  }

  private async remove(task: TaskDef): Promise<void> {
    const users = commandUsers(task.id, this.connections).map((profile) => profile.name);
    const message = users.length ? `Delete command "${task.name}"?\nUsed by: ${users.join(", ")}` : `Delete command "${task.name}"?`;
    if (!(await confirmDialog(message, "Delete"))) return;
    await attempt(async () => this.host.configChanged(await api.deleteTask(task.id)));
  }
}

export function createLogView(task: TaskDef, panel: TasksPanel): LogTabContent {
  const output = h("pre", { class: "log-output" });
  const logLines = () => output.textContent?.split("\n").filter(Boolean) ?? [];
  const append = (line: string) => {
    const following = output.scrollTop + output.clientHeight >= output.scrollHeight - 8;
    output.append(`${line}\n`);
    if (following) output.scrollTop = output.scrollHeight;
  };
  const unsubscribe = panel.subscribeLogs(task.id, append);
  api
    .taskLogs(task.id)
    .then((lines) => {
      output.textContent = lines.map((line) => `${line}\n`).join("");
      output.scrollTop = output.scrollHeight;
    })
    .catch((error) => showError(error, `Read the log of ${task.name}`));
  const toolbar = h(
    "div",
    { class: "toolbar" },
    icon("terminal", "toolbar-icon"),
    h("span", { class: "toolbar-title mono" }, `${task.program} ${formatArgs(task.args)}`),
    spacer(),
    panel.canAsk() ? toolButton("sparkle", "Ask assistant", () => panel.ask(task, logLines()), "Explain this log with the assistant") : null,
    toolButton("refresh", "Restart", () => void attempt(() => api.restartTask(task.id))),
    toolButton("x", "Clear view", () => output.replaceChildren()),
  );
  return { element: h("div", { class: "view log-view" }, toolbar, output), dispose: unsubscribe, task, logLines };
}

async function attempt(action: () => Promise<unknown>): Promise<void> {
  try {
    await action();
  } catch (error) {
    showError(error, "Command");
  }
}

function isActive(state: TaskState): boolean {
  return state === "starting" || state === "running" || state === "restarting";
}

function describeStatus(state: TaskState, status: TaskStatus | undefined): string {
  if (status?.message && state !== "running") return `${state} · ${status.message}`;
  const restarts = status && status.restarts > 0 ? ` · ${status.restarts} restart(s)` : "";
  const forConnections = status?.forConnections && isActive(state) ? " · for connections" : "";
  return `${state}${forConnections}${restarts}`;
}
