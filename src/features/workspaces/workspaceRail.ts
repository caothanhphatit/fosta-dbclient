import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { h, icon, iconButton, setIcon, type IconName } from "../../core/dom";
import { describeToggle, resolvedTheme } from "../../core/theme";
import type { AppConfig, Workspace, WorkspaceMeta } from "../../core/types";
import { relaxedFromBaseline, safetyLabel, safetyOf } from "../safety/safety";

export const WORKSPACE_COLORS = ["blue", "green", "orange", "red", "purple", "teal", "pink", "slate"] as const;

export type WorkspaceColor = (typeof WORKSPACE_COLORS)[number];

/** Environment kinds a workspace can be labelled with. */
export const WORKSPACE_ICONS: { name: IconName; label: string }[] = [
  { name: "laptop", label: "Local" },
  { name: "flask", label: "Test" },
  { name: "cloud", label: "Staging" },
  { name: "shield", label: "Production" },
  { name: "briefcase", label: "Client" },
  { name: "server", label: "Server" },
  { name: "globe", label: "Public" },
  { name: "rocket", label: "Project" },
];

const MAX_SHORTCUTS = 9;

/** Live activity of one workspace, shown as badges on the rail. */
export interface WorkspaceActivity {
  connected: number;
  running: number;
}

export function activeWorkspace(config: AppConfig): Workspace {
  return config.workspaces.find((workspace) => workspace.id === config.activeWorkspace) ?? config.workspaces[0];
}

export function workspaceColor(color: string): WorkspaceColor {
  return (WORKSPACE_COLORS as readonly string[]).includes(color) ? (color as WorkspaceColor) : "blue";
}

export function workspaceIcon(name: string): IconName {
  return WORKSPACE_ICONS.find((item) => item.name === name)?.name ?? "laptop";
}

export interface RailHost {
  switchTo(id: string): void;
  create(): void;
  edit(workspace: WorkspaceMeta): void;
  remove(workspace: WorkspaceMeta): void;
  toggleTheme(): void;
  openThemeMenu(event: MouseEvent): void;
  openSettings(event: MouseEvent): void;
}

/** The strip on the far left: one entry per workspace with its live state, plus theme and settings. */
export class WorkspaceRail {
  readonly element: HTMLElement;
  private readonly host: RailHost;
  private readonly list = h("div", { class: "rail-list", role: "tablist", "aria-label": "Workspaces" });
  private readonly themeButton: HTMLButtonElement;
  private config: AppConfig | null = null;
  private activity = new Map<string, WorkspaceActivity>();

  constructor(host: RailHost) {
    this.host = host;
    this.themeButton = iconButton(resolvedTheme() === "dark" ? "sun" : "moon", describeToggle(), () => host.toggleTheme());
    this.themeButton.classList.add("rail-button");
    this.themeButton.addEventListener("contextmenu", (event) => host.openThemeMenu(event));
    const settingsButton = iconButton("settings", "Settings", () => undefined);
    settingsButton.classList.add("rail-button");
    settingsButton.addEventListener("click", (event) => host.openSettings(event));
    const addButton = h("button", { class: "rail-add", type: "button", title: "New workspace" }, icon("plus"), h("span", {}, "New"));
    addButton.addEventListener("click", () => host.create());
    this.element = h(
      "nav",
      { class: "rail", "aria-label": "Workspace rail" },
      h("div", { class: "rail-heading" }, "Workspaces"),
      this.list,
      addButton,
      h("div", { class: "spacer" }),
      h("div", { class: "rail-footer" }, this.themeButton, settingsButton),
    );
  }

  setConfig(config: AppConfig): void {
    this.config = config;
    this.render();
  }

  /** Updates the connection / command badges without re-reading the config. */
  setActivity(activity: Map<string, WorkspaceActivity>): void {
    this.activity = activity;
    this.render();
  }

  refreshTheme(): void {
    setIcon(this.themeButton, resolvedTheme() === "dark" ? "sun" : "moon");
    this.themeButton.title = describeToggle();
    this.themeButton.setAttribute("aria-label", describeToggle());
  }

  private render(): void {
    const config = this.config;
    if (!config) return;
    const items = config.workspaces.map((workspace, index) => this.entry(workspace, index, workspace.id === config.activeWorkspace, config.workspaces.length));
    this.list.replaceChildren(...items);
  }

  private entry(workspace: Workspace, index: number, active: boolean, count: number): HTMLElement {
    const activity = this.activity.get(workspace.id) ?? { connected: 0, running: 0 };
    const shortcut = index < MAX_SHORTCUTS ? `Ctrl+${index + 1}` : "";
    const relaxed = relaxedFromBaseline(workspace);
    const lines = [
      workspace.name,
      `Safety: ${safetyLabel(safetyOf(workspace))}${relaxed.length ? ` (relaxed: ${relaxed.map((change) => change.text).join("; ")})` : ""}`,
      `${workspace.connections.length} connection(s), ${activity.connected} open`,
      `${workspace.tasks.length} command(s), ${activity.running} running`,
      shortcut,
    ].filter(Boolean);
    const classes = [
      "ws-entry",
      `color-${workspaceColor(workspace.color)}`,
      active ? "active" : "",
      activity.connected ? "online" : "",
      activity.running ? "busy" : "",
      relaxed.length ? "relaxed" : "",
    ];
    const badge = h(
      "span",
      { class: "ws-badges" },
      activity.connected ? h("span", { class: "ws-badge online", title: `${activity.connected} connection(s) open` }) : null,
      activity.running ? h("span", { class: "ws-badge busy", title: `${activity.running} command(s) running` }) : null,
      relaxed.length ? h("span", { class: "ws-badge relaxed", title: "Safety rules relaxed", "aria-label": "Safety rules relaxed" }, "!") : null,
    );
    const button = h(
      "button",
      { class: classes.filter(Boolean).join(" "), type: "button", role: "tab", "aria-selected": String(active), title: lines.join("\n") },
      h("span", { class: "ws-glyph" }, icon(workspaceIcon(workspace.icon)), badge),
      h("span", { class: "ws-label" }, workspace.name),
    );
    button.addEventListener("click", () => {
      if (!active) this.host.switchTo(workspace.id);
    });
    button.addEventListener("contextmenu", (event) => showContextMenu(event, this.menu(workspace, count)));
    return button;
  }

  private menu(workspace: WorkspaceMeta, count: number): MenuItem[] {
    return [
      { label: "Open", icon: "layers", run: () => this.host.switchTo(workspace.id) },
      { label: "Edit workspace…", icon: "edit", run: () => this.host.edit(workspace) },
      "separator",
      { label: "New workspace…", icon: "plus", run: () => this.host.create() },
      { label: "Delete workspace", icon: "trash", danger: true, disabled: count <= 1, run: () => this.host.remove(workspace) },
    ];
  }
}
