/** Builds the shell's DOM: toolbar, sidebar, workspace area with its splitters, status bar. */
import { errorPanelBadge, errorPanelElement } from "../core/errorPanel";
import { bigButton, clamp, h, iconButton, separator, spacer } from "../core/dom";
import { makeSplitter } from "../core/splitter";
import type { TabManager } from "../core/tabs";
import type { Capabilities } from "../core/types";
import type { AssistantPanel } from "../features/assistant/assistantPanel";
import type { ConnectionTree } from "../features/connections/tree";
import type { TasksPanel } from "../features/tasks/tasksPanel";
import type { WorkspaceRail } from "../features/workspaces/workspaceRail";
import { updateStatusItem } from "./update";

const SIDEBAR_MIN = 220;
const SIDEBAR_MAX = 640;
const ASSISTANT_MIN = 300;
const ASSISTANT_MAX = 720;
export const ASSISTANT_STORAGE_KEY = "db-studio.assistant";

/** The pieces and actions the layout wires together. */
export interface LayoutParts {
  workspaceHeader: HTMLElement;
  statusText: HTMLElement;
  statusContext: HTMLElement;
  safetyStatus: HTMLElement;
  taskSummary: HTMLElement;
  tree: ConnectionTree;
  tasksPanel: TasksPanel;
  tabs: TabManager;
  assistant: AssistantPanel;
  rail: WorkspaceRail;
  newConnection(): void;
  newQuery(): void;
  newDiagram(): void;
  configureEmbedding(): void;
  toggleAssistant(): void;
}

/** Marks an element as needing a capability; `hideUnsupported` hides it when the edition lacks it. */
function withRequirement<T extends HTMLElement>(element: T, capability: keyof Capabilities): T {
  element.dataset.requires = capability;
  return element;
}

/** Appends the shell to the page and returns the workspace area (the element that toggles the assistant column). */
export function buildLayout(parts: LayoutParts): HTMLElement {
  const { workspaceHeader, statusText, statusContext, safetyStatus, taskSummary, tree, tasksPanel, tabs, assistant, rail } = parts;
  const toolbar = h(
    "header",
    { class: "app-toolbar" },
    bigButton("plug", "Connection", parts.newConnection, "New connection (Ctrl+N)"),
    bigButton("query", "Query", parts.newQuery, "New query (Ctrl+T)"),
    bigButton("diagram", "Diagram", parts.newDiagram, "ER diagram of the selected database"),
    bigButton("refresh", "Refresh", () => tree.refreshSelected(), "Reload the selected database"),
    separator(),
    withRequirement(
      bigButton("task", "Task", () => void tasksPanel.edit(null), "New startup task"),
      "tasks",
    ),
    bigButton("vector", "Embedding", parts.configureEmbedding, "Embedding API for vector search"),
    spacer(),
    bigButton("sparkle", "Assistant", parts.toggleAssistant, "Chat with your database (Ctrl+Shift+A)"),
  );
  const sidebar = h(
    "aside",
    { class: "sidebar" },
    workspaceHeader,
    h("div", { class: "panel-header" }, h("span", {}, "Connections"), iconButton("plus", "New connection", parts.newConnection)),
    tree.element,
    withRequirement(tasksPanel.element, "tasks"),
  );
  const sidebarSplitter = h("div", { class: "splitter-x" });
  const assistantSplitter = h("div", { class: "splitter-x assistant-splitter" });
  const area = h("main", { class: "workspace" }, sidebar, sidebarSplitter, tabs.element, assistantSplitter, assistant.element);

  let sidebarWidth = 290;
  makeSplitter(sidebarSplitter, "x", (delta) => {
    sidebarWidth = clamp(sidebarWidth + delta, SIDEBAR_MIN, SIDEBAR_MAX);
    area.style.setProperty("--sidebar-width", `${sidebarWidth}px`);
  });
  let assistantWidth = 380;
  makeSplitter(assistantSplitter, "x", (delta) => {
    assistantWidth = clamp(assistantWidth - delta, ASSISTANT_MIN, ASSISTANT_MAX);
    area.style.setProperty("--assistant-width", `${assistantWidth}px`);
  });
  let remembered: string | null = null;
  try {
    remembered = localStorage.getItem(ASSISTANT_STORAGE_KEY);
  } catch {
    // Fall through: closed by default.
  }
  area.classList.toggle("with-assistant", remembered === "open");
  const version = h("span", { class: "status-version", title: "Fosta Studio version" }, `v${__APP_VERSION__}`);
  const statusBar = h(
    "footer",
    { class: "status-bar" },
    statusContext,
    statusText,
    spacer(),
    errorPanelBadge(),
    safetyStatus,
    taskSummary,
    updateStatusItem(),
    version,
  );
  const main = h("div", { class: "app-main" }, toolbar, area, errorPanelElement(), statusBar);
  (document.getElementById("app") ?? document.body).append(rail.element, main);
  return area;
}
