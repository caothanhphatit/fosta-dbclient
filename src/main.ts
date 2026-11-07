import "./styles.css";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, errorText } from "./api";
import { AssistantPanel, type AssistantContext } from "./assistant";
import { showContextMenu, type MenuItem } from "./contextMenu";
import { confirmDialog, toast } from "./dialogs";
import { bigButton, clamp, h, icon, iconButton, separator, spacer, type IconName } from "./dom";
import { DesignView } from "./designView";
import { aiDialog, connectionDialog, embeddingDialog, workspaceDialog } from "./forms";
import type { Navigator } from "./navigator";
import type { QueryView } from "./queryView";
import { makeSplitter } from "./splitter";
import { TableView } from "./tableView";
import { TabManager, type TabContent } from "./tabs";
import { createLogView, TasksPanel, type LogTabContent } from "./tasksPanel";
import { describeTheme, initTheme, onThemeChange, setTheme, themePreference, toggleTheme, type ThemePreference } from "./theme";
import { ConnectionTree } from "./tree";
import type { AppConfig, CellValue, ConnectionProfile, TableTarget, TaskDef, Workspace, WorkspaceMeta } from "./types";
import { VectorHealthView } from "./vectorHealth";
import { VectorSearchView } from "./vectorSearch";
import { activeWorkspace, WorkspaceRail, workspaceColor, workspaceIcon, type WorkspaceActivity } from "./workspaces";

const SHORTCUTS: [string, string][] = [
  ["Ctrl+N", "New connection"],
  ["Ctrl+T", "New query"],
  ["Ctrl+Shift+A", "Assistant panel"],
  ["Ctrl+1 … 9", "Switch workspace"],
  ["Ctrl+Enter", "Run SQL (selection or all)"],
  ["Enter / F2", "Edit the selected cell"],
  ["Ctrl+S", "Save grid changes"],
  ["F5", "Reload table"],
];

const THEME_OPTIONS: { value: ThemePreference; label: string }[] = [
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
  { value: "system", label: "Follow system" },
];

const SIDEBAR_MIN = 220;
const SIDEBAR_MAX = 640;
const ASSISTANT_MIN = 300;
const ASSISTANT_MAX = 720;
const ASSISTANT_STORAGE_KEY = "db-studio.assistant";
const LOG_TAIL_LINES = 80;

const statusText = h("span", { class: "status-text" }, "Ready");
const taskSummary = h("span", { class: "status-tasks" });
const welcomeName = h("span", { class: "welcome-workspace" });
const workspaceHeader = h("div", { class: "ws-header" });
const tabs = new TabManager(welcomeScreen());
let queryCounter = 0;
let currentConfig: AppConfig | null = null;
let workspace: Workspace | null = null;
let workspaceArea: HTMLElement | null = null;

const assistant = new AssistantPanel({
  context: assistantContext,
  insertSql: (text) => {
    const active = tabs.activeContent();
    if (isQueryView(active)) active.insertText(text);
    else openQueryWithText(text);
  },
  openQuery: openQueryWithText,
  configure: () => void configureAssistant(),
  close: () => showAssistant(false),
});

const appNavigator: Navigator = {
  openTable,
  openVectorSearch,
  openVectorHealth,
  configureEmbedding: () => void configureEmbedding(),
};

const rail = new WorkspaceRail({
  switchTo: (id) => void switchWorkspace(id),
  create: () => void createWorkspace(),
  edit: (meta) => void editWorkspace(meta),
  remove: (meta) => void deleteWorkspace(meta),
  toggleTheme: () => {
    toggleTheme();
    setStatus(describeTheme());
  },
  openSettings: (event) => showContextMenu(event, settingsMenu()),
  openThemeMenu: (event) => showContextMenu(event, themeMenu()),
});

const tree = new ConnectionTree({
  openTable: (target) => openTable(target),
  openDesign,
  openQuery,
  editConnection: (profile) => void editConnection(profile),
  deleteConnection: (profile) => void deleteConnection(profile),
  connectionClosed: closeConnectionTabs,
  configChanged: applyConfig,
  status: setStatus,
  stateChanged: () => {
    refreshActivity();
    assistant.refreshContext();
  },
});

const tasksPanel = new TasksPanel({
  openLogs,
  configChanged: applyConfig,
  workspaceName: () => workspace?.name ?? "",
  summaryChanged: (text) => {
    taskSummary.textContent = text;
  },
  activityChanged: refreshActivity,
  askAssistant: (task, logLines) => {
    openLogs(task);
    showAssistant(true);
    assistant.ask(`Startup task "${task.name}" is not working. Read its log, explain what is wrong and how to fix it.`);
    void logLines;
  },
});

tabs.onActiveChange = () => assistant.refreshContext();

function isQueryView(content: TabContent | null): content is QueryView {
  return content !== null && typeof (content as QueryView).currentText === "function";
}

function isLogTab(content: TabContent | null): content is LogTabContent {
  return content !== null && typeof (content as LogTabContent).logLines === "function";
}

/** What the assistant should know about the active tab (or the tree selection). */
function assistantContext(): AssistantContext {
  const active = tabs.activeContent();
  const withConnection = (connectionId: string, rest: Partial<AssistantContext>): AssistantContext => {
    const profile = currentConfig?.workspaces.flatMap((item) => item.connections).find((item) => item.id === connectionId);
    const connected = tree.connectedIds().has(connectionId);
    return {
      kind: "connection",
      connectionId: connected ? connectionId : undefined,
      connectionName: profile?.name,
      driver: profile?.driver,
      serverVersion: tree.serverVersion(connectionId) ?? undefined,
      ...rest,
    };
  };
  if (isQueryView(active)) {
    return withConnection(active.connectionId, {
      kind: "query",
      database: active.currentDatabase() ?? undefined,
      editorText: () => active.currentText(),
      lastError: () => active.lastError,
    });
  }
  if (active instanceof TableView) {
    return withConnection(active.target.connectionId, { kind: "table", database: active.target.database, table: active.target.table });
  }
  if (isLogTab(active)) {
    const task = active.task;
    const selection = tree.activeContext();
    const base: AssistantContext = selection ? withConnection(selection.connectionId, { database: selection.database ?? undefined }) : { kind: "none" };
    return {
      ...base,
      kind: "task",
      extra: () => `Startup task "${task.name}" runs: ${task.program} ${task.args.join(" ")}\nLast log lines:\n${active.logLines().slice(-LOG_TAIL_LINES).join("\n")}`,
    };
  }
  const selection = tree.activeContext();
  return selection ? withConnection(selection.connectionId, { database: selection.database ?? undefined }) : { kind: "none" };
}

function openQueryWithText(text: string): void {
  const context = assistantContext();
  const connectionId = context.connectionId ?? tree.activeContext()?.connectionId;
  if (!connectionId) {
    toast("Connect to a database first to open a query tab");
    return;
  }
  openQuery(connectionId, context.database ?? null, text);
}

function showAssistant(visible: boolean): void {
  workspaceArea?.classList.toggle("with-assistant", visible);
  try {
    localStorage.setItem(ASSISTANT_STORAGE_KEY, visible ? "open" : "closed");
  } catch {
    // Storage can be unavailable; the panel state then lasts for this session only.
  }
  if (visible) {
    assistant.refreshContext();
    assistant.focus();
  }
}

function toggleAssistant(): void {
  showAssistant(!workspaceArea?.classList.contains("with-assistant"));
}

async function configureAssistant(): Promise<void> {
  const config = await aiDialog(currentConfig?.ai ?? null);
  if (config) applyConfig(config);
}

/** Recomputes the per-workspace badges on the rail from live connection and task state. */
function refreshActivity(): void {
  if (!currentConfig) return;
  const connected = tree.connectedIds();
  const running = tasksPanel.runningIds();
  const activity = new Map<string, WorkspaceActivity>();
  for (const item of currentConfig.workspaces) {
    activity.set(item.id, {
      connected: item.connections.filter((profile) => connected.has(profile.id)).length,
      running: item.tasks.filter((task) => running.has(task.id)).length,
    });
  }
  rail.setActivity(activity);
}

function applyConfig(config: AppConfig): void {
  currentConfig = config;
  workspace = activeWorkspace(config);
  rail.setConfig(config);
  renderWorkspaceHeader(workspace);
  tree.setConnections(workspace.connections);
  tasksPanel.setTasks(workspace.tasks);
  tabs.setScope(workspace.id);
  assistant.setConfigured(config.ai);
  assistant.setScope(workspace.id);
  welcomeName.textContent = workspace.name;
  refreshActivity();
  void getCurrentWindow().setTitle(`${workspace.name} — DB Studio`);
}

function renderWorkspaceHeader(current: Workspace): void {
  const menuButton = iconButton("more", "Workspace options", () => undefined);
  menuButton.addEventListener("click", (event) => showContextMenu(event, workspaceMenu(current)));
  workspaceHeader.replaceChildren(
    h("span", { class: `ws-dot color-${workspaceColor(current.color)}` }, icon(workspaceIcon(current.icon))),
    h("span", { class: "ws-name", title: current.name }, current.name),
    menuButton,
  );
}

function workspaceMenu(current: Workspace): MenuItem[] {
  const count = currentConfig?.workspaces.length ?? 1;
  return [
    { label: "Edit workspace…", icon: "edit", run: () => void editWorkspace(current) },
    { label: "New workspace…", icon: "plus", run: () => void createWorkspace() },
    "separator",
    { label: "Delete workspace", icon: "trash", danger: true, disabled: count <= 1, run: () => void deleteWorkspace(current) },
  ];
}

function settingsMenu(): MenuItem[] {
  return [
    { label: "Assistant…", icon: "sparkle", run: () => void configureAssistant() },
    { label: "Embedding API…", icon: "vector", run: () => void configureEmbedding() },
  ];
}

/** Right-click menu of the theme button: the one place to pick "follow system". */
function themeMenu(): MenuItem[] {
  const current = themePreference();
  return THEME_OPTIONS.map((option) => ({
    label: option.label,
    icon: option.value === current ? ("check" as IconName) : undefined,
    run: () => {
      setTheme(option.value);
      setStatus(describeTheme());
    },
  }));
}

function setStatus(message: string): void {
  statusText.textContent = message;
}

function workspaceId(): string {
  return workspace?.id ?? "";
}

function tabKey(connectionId: string, kind: string, ...parts: string[]): string {
  return [workspaceId(), connectionId, kind, ...parts].join("|");
}

function closeConnectionTabs(connectionId: string): void {
  tabs.closeWhere((key) => key.split("|")[1] === connectionId);
}

function openTable(target: TableTarget, filter = ""): void {
  tabs.open({
    key: tabKey(target.connectionId, "data", target.database, target.table, filter),
    title: filter ? `${target.table} (filtered)` : target.table,
    subtitle: filter ? `${target.database}.${target.table} WHERE ${filter}` : `${target.database}.${target.table}`,
    icon: target.kind === "view" ? "view" : "table",
    create: () => new TableView(target, appNavigator, filter),
  });
}

function openVectorSearch(target: TableTarget, column: string, seed?: CellValue[]): void {
  const key = tabKey(target.connectionId, "vector-search", target.database, target.table);
  const existing = tabs.contentOf(key);
  if (existing instanceof VectorSearchView && seed) existing.setSeed(column, seed);
  tabs.open({
    key,
    title: `${target.table} · vector search`,
    subtitle: `${target.database}.${target.table}`,
    icon: "vector",
    create: () => new VectorSearchView(target, column, appNavigator, seed ?? null),
  });
}

function openVectorHealth(target: TableTarget, column: string): void {
  tabs.open({
    key: tabKey(target.connectionId, "vector-health", target.database, target.table),
    title: `${target.table} · vector health`,
    subtitle: `${target.database}.${target.table}`,
    icon: "health",
    create: () => new VectorHealthView(target, column),
  });
}

async function configureEmbedding(): Promise<void> {
  const config = await embeddingDialog(currentConfig?.embedding ?? null);
  if (config) applyConfig(config);
}

function openDesign(target: TableTarget): void {
  tabs.open({
    key: tabKey(target.connectionId, "design", target.database, target.table),
    title: `${target.table} · design`,
    subtitle: `${target.database}.${target.table}`,
    icon: "design",
    create: () => new DesignView(target),
  });
}

function openQuery(connectionId: string, database: string | null, initialSql = ""): void {
  void openQueryTab(connectionId, database, initialSql);
}

async function openQueryTab(connectionId: string, database: string | null, initialSql: string): Promise<void> {
  const { QueryView } = await import("./queryView");
  const driver = currentConfig?.workspaces.flatMap((item) => item.connections).find((profile) => profile.id === connectionId)?.driver ?? "mariadb";
  queryCounter += 1;
  tabs.open({
    key: tabKey(connectionId, "query", crypto.randomUUID()),
    title: `Query ${queryCounter}`,
    subtitle: database ?? undefined,
    icon: "query",
    create: () => {
      const view = new QueryView(connectionId, driver, database, initialSql);
      view.onChanged = () => {
        if (tabs.activeContent() === view) assistant.refreshContext();
      };
      return view;
    },
  });
}

function openLogs(task: TaskDef): void {
  tabs.open({
    key: tabKey("task", "logs", task.id),
    title: `${task.name} · logs`,
    icon: "logs",
    create: () => createLogView(task, tasksPanel),
  });
}

function newQueryFromSelection(): void {
  const context = tree.activeContext();
  if (context) openQuery(context.connectionId, context.database);
  else toast("Connect to a database first (double-click a connection)");
}

async function editConnection(profile: ConnectionProfile | null): Promise<void> {
  const config = await connectionDialog(profile, workspace?.name ?? "");
  if (config) applyConfig(config);
}

async function deleteConnection(profile: ConnectionProfile): Promise<void> {
  if (!(await confirmDialog(`Delete connection "${profile.name}"?`, "Delete"))) return;
  try {
    closeConnectionTabs(profile.id);
    applyConfig(await api.deleteConnection(profile.id));
  } catch (error) {
    toast(errorText(error), "error");
  }
}

async function switchWorkspace(id: string): Promise<void> {
  if (id === workspaceId()) return;
  try {
    applyConfig(await api.setActiveWorkspace(id));
    await tasksPanel.refreshStatuses();
    setStatus(`Workspace: ${workspace?.name ?? id}`);
  } catch (error) {
    toast(errorText(error), "error");
  }
}

async function createWorkspace(): Promise<void> {
  const config = await workspaceDialog(null);
  if (!config) return;
  const created = config.workspaces[config.workspaces.length - 1];
  applyConfig(config);
  if (created && created.id !== config.activeWorkspace) await switchWorkspace(created.id);
}

async function editWorkspace(meta: WorkspaceMeta): Promise<void> {
  const config = await workspaceDialog(meta);
  if (config) applyConfig(config);
}

async function deleteWorkspace(meta: WorkspaceMeta): Promise<void> {
  const target = currentConfig?.workspaces.find((item) => item.id === meta.id);
  if (!target) return;
  const summary = `${target.connections.length} connection(s) and ${target.tasks.length} startup task(s) will be removed, and saved passwords forgotten.`;
  if (!(await confirmDialog(`Delete workspace "${target.name}"?\n${summary}`, "Delete"))) return;
  try {
    tabs.closeWhere((key) => key.startsWith(`${target.id}|`));
    applyConfig(await api.deleteWorkspace(target.id));
    setStatus(`Deleted workspace ${target.name}`);
  } catch (error) {
    toast(errorText(error), "error");
  }
}

function welcomeScreen(): HTMLElement {
  const card = (iconName: IconName, title: string, detail: string, run: () => void) => {
    const button = h("button", { class: "welcome-action", type: "button" }, icon(iconName), h("span", { class: "welcome-action-title" }, title), h("span", { class: "welcome-action-detail" }, detail));
    button.addEventListener("click", run);
    return button;
  };
  return h(
    "div",
    { class: "welcome" },
    h(
      "div",
      { class: "welcome-inner" },
      h("div", { class: "welcome-brand" }, icon("database", "welcome-icon"), h("h1", {}, "DB Studio")),
      h("p", { class: "welcome-sub" }, "Workspace ", welcomeName, ". Connect on the left, then open a table or a query."),
      h(
        "div",
        { class: "welcome-actions" },
        card("plug", "New connection", "Save a MariaDB or MySQL server in this workspace", () => void editConnection(null)),
        card("query", "New query", "Open a SQL editor on the selected connection", newQueryFromSelection),
        card("task", "New startup task", "Keep a port-forward alive while this workspace is open", () => void tasksPanel.edit(null)),
      ),
      h("ul", { class: "shortcuts" }, ...SHORTCUTS.map(([keys, action]) => h("li", {}, h("kbd", {}, keys), h("span", {}, action)))),
    ),
  );
}

function buildLayout(): void {
  const toolbar = h(
    "header",
    { class: "app-toolbar" },
    bigButton("plug", "Connection", () => void editConnection(null), "New connection (Ctrl+N)"),
    bigButton("query", "Query", newQueryFromSelection, "New query (Ctrl+T)"),
    bigButton("refresh", "Refresh", () => tree.refreshSelected(), "Reload the selected database"),
    separator(),
    bigButton("task", "Task", () => void tasksPanel.edit(null), "New startup task"),
    bigButton("vector", "Embedding", () => void configureEmbedding(), "Embedding API for vector search"),
    spacer(),
    bigButton("sparkle", "Assistant", toggleAssistant, "Chat with your database (Ctrl+Shift+A)"),
  );
  const sidebar = h(
    "aside",
    { class: "sidebar" },
    workspaceHeader,
    h("div", { class: "panel-header" }, h("span", {}, "Connections"), iconButton("plus", "New connection", () => void editConnection(null))),
    tree.element,
    tasksPanel.element,
  );
  const sidebarSplitter = h("div", { class: "splitter-x" });
  const assistantSplitter = h("div", { class: "splitter-x assistant-splitter" });
  const area = h("main", { class: "workspace" }, sidebar, sidebarSplitter, tabs.element, assistantSplitter, assistant.element);
  workspaceArea = area;
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
  const statusBar = h("footer", { class: "status-bar" }, statusText, spacer(), taskSummary);
  const main = h("div", { class: "app-main" }, toolbar, area, statusBar);
  (document.getElementById("app") ?? document.body).append(rail.element, main);
}

function installShortcuts(): void {
  document.addEventListener("keydown", (event) => {
    if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
    const key = event.key.toLowerCase();
    if (key >= "1" && key <= "9" && !event.shiftKey) {
      const target = currentConfig?.workspaces[Number(key) - 1];
      if (target) {
        event.preventDefault();
        void switchWorkspace(target.id);
      }
    } else if (key === "n" && !event.shiftKey) {
      event.preventDefault();
      void editConnection(null);
    } else if (key === "t" && !event.shiftKey) {
      event.preventDefault();
      newQueryFromSelection();
    } else if (key === "a" && event.shiftKey) {
      event.preventDefault();
      toggleAssistant();
    }
  });
}

function suppressBrowserContextMenu(): void {
  document.addEventListener("contextmenu", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest("input, textarea, .cm-editor")) return;
    event.preventDefault();
  });
}

function revealWindowAfterFirstPaint(): void {
  requestAnimationFrame(() => requestAnimationFrame(() => void getCurrentWindow().show()));
}

async function bootstrap(): Promise<void> {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    const { installDevMock } = await import("./devMock");
    installDevMock();
  }
  initTheme();
  onThemeChange(() => rail.refreshTheme());
  buildLayout();
  installShortcuts();
  suppressBrowserContextMenu();
  revealWindowAfterFirstPaint();
  try {
    applyConfig(await api.getConfig());
    await tasksPanel.init();
  } catch (error) {
    toast(errorText(error), "error");
  }
}

void bootstrap();
