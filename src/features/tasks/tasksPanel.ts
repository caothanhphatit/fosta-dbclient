import { api, errorText, onTaskLog, onTaskStatus } from "../../core/api";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { confirmDialog, toast } from "../../core/dialogs";
import { h, icon, iconButton, spacer, toolButton } from "../../core/dom";
import { taskDialog } from "./taskDialog";
import { formatArgs } from "../../core/shell";
import type { TabContent } from "../../core/tabs";
import type { AppConfig, TaskDef, TaskState, TaskStatus } from "../../core/types";

export interface TasksHost {
  openLogs(task: TaskDef): void;
  configChanged(config: AppConfig): void;
  summaryChanged(text: string): void;
  workspaceName(): string;
  /** Fired whenever any task (in any workspace) changes state. */
  activityChanged?(): void;
  /** Opens the assistant on a task's log. */
  askAssistant?(task: TaskDef, logLines: string[]): void;
}

/** What a log tab exposes so the assistant can read the task's output. */
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

  constructor(host: TasksHost) {
    this.host = host;
    const header = h(
      "div",
      { class: "panel-header collapsible" },
      icon("chevronDown", "panel-chevron"),
      h("span", {}, "Startup tasks"),
      this.badge,
      spacer(),
      iconButton("plus", "New startup task", () => void this.edit(null)),
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

  setTasks(tasks: TaskDef[]): void {
    this.tasks = tasks;
    this.render();
  }

  async refreshStatuses(): Promise<void> {
    try {
      for (const status of await api.taskStatuses()) this.statuses.set(status.id, status);
      this.render();
    } catch (error) {
      toast(errorText(error), "error");
    }
  }

  /** Ids of every task (in any workspace) that is starting, running or restarting. */
  runningIds(): Set<string> {
    return new Set([...this.statuses.values()].filter((status) => isActive(status.state)).map((status) => status.id));
  }

  subscribeLogs(id: string, listener: LogListener): () => void {
    const listeners = this.logListeners.get(id) ?? new Set<LogListener>();
    listeners.add(listener);
    this.logListeners.set(id, listeners);
    return () => void listeners.delete(listener);
  }

  async edit(existing: TaskDef | null): Promise<void> {
    const config = await taskDialog(existing, this.host.workspaceName());
    if (config) this.host.configChanged(config);
  }

  canAsk(): boolean {
    return Boolean(this.host.askAssistant);
  }

  ask(task: TaskDef, logLines: string[]): void {
    this.host.askAssistant?.(task, logLines);
  }

  private render(): void {
    const rows = this.tasks.map((task) => this.renderTask(task));
    this.list.replaceChildren(...(rows.length ? rows : [h("div", { class: "empty-hint" }, "No startup tasks in this workspace")]));
    const running = this.tasks.filter((task) => this.statuses.get(task.id)?.state === "running").length;
    this.badge.textContent = this.tasks.length ? `${running}/${this.tasks.length}` : "";
    this.host.summaryChanged(this.tasks.length ? `Tasks ${running}/${this.tasks.length} running` : "");
    this.host.activityChanged?.();
  }

  private renderTask(task: TaskDef): HTMLElement {
    const status = this.statuses.get(task.id);
    const state = status?.state ?? "stopped";
    const active = isActive(state);
    const row = h(
      "div",
      { class: `task-row state-${state}`, title: `${task.program} ${formatArgs(task.args)}` },
      h("span", { class: "status-dot" }),
      h("div", { class: "task-text" }, h("div", { class: "task-name" }, task.name), h("div", { class: "task-state" }, describeStatus(state, status))),
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
    if (!(await confirmDialog(`Delete startup task "${task.name}"?`, "Delete"))) return;
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
    .catch((error) => toast(errorText(error), "error"));
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
    toast(errorText(error), "error");
  }
}

function isActive(state: TaskState): boolean {
  return state === "starting" || state === "running" || state === "restarting";
}

function describeStatus(state: TaskState, status: TaskStatus | undefined): string {
  if (status?.message && state !== "running") return `${state} · ${status.message}`;
  const restarts = status && status.restarts > 0 ? ` · ${status.restarts} restart(s)` : "";
  return `${state}${restarts}`;
}
