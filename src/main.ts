/**
 * App shell: builds the layout (rail, toolbar, sidebar, tabs, assistant,
 * status bar), wires the feature modules together and holds the current
 * config. Opening tabs lives in `app/navigation.ts`.
 */
import "./styles.css";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { assistantContext as buildAssistantContext, isQueryView, taskLogContext } from "./app/assistantContext";
import { copyDiagnostics, openLogFolder, setDebugLogging } from "./app/diagnostics";
import { AppNavigation, preloadQueryView } from "./app/navigation";
import { welcomeScreen } from "./app/welcome";
import { api, errorText } from "./core/api";
import { capabilities, hideUnsupported, loadCapabilities } from "./core/capabilities";
import { isTauri, signOut } from "./core/transport";
import { showContextMenu, type MenuItem } from "./core/contextMenu";
import { confirmDialog, isModalOpen, toast } from "./core/dialogs";
import { bigButton, clamp, h, icon, iconButton, separator, spacer, type IconName } from "./core/dom";
import { makeSplitter } from "./core/splitter";
import { TabManager } from "./core/tabs";
import { describeTheme, initTheme, onThemeChange, setTheme, themePreference, toggleTheme, type ThemePreference } from "./core/theme";
import type { AiConnection, AppConfig, Capabilities, ConnectionProfile, UiAction, Workspace, WorkspaceMeta } from "./core/types";
import { aiDialog } from "./features/assistant/aiDialog";
import { AssistantPanel, type AssistantContext } from "./features/assistant/assistantPanel";
import { connectionDialog } from "./features/connections/connectionDialog";
import { ConnectionTree } from "./features/connections/tree";
import { TasksPanel } from "./features/tasks/tasksPanel";
import { embeddingDialog } from "./features/vector/embeddingDialog";
import { connectionSafety, relaxedFromBaseline, safetyLabel, safetyOf, writesBlocked } from "./features/safety/safety";
import { workspaceDialog } from "./features/workspaces/workspaceDialog";
import { activeWorkspace, WorkspaceRail, workspaceColor, workspaceIcon, type WorkspaceActivity } from "./features/workspaces/workspaceRail";

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

const statusText = h("span", { class: "status-text" }, "Ready");
const taskSummary = h("span", { class: "status-tasks" });
/** The active workspace's safety preset; a warning while its rules are relaxed. */
const safetyStatus = h("span", { class: "status-safety" });
const welcomeName = h("span", { class: "welcome-workspace" });
const workspaceHeader = h("div", { class: "ws-header" });
const welcome = welcomeScreen(welcomeName, {
  newConnection: () => void editConnection(null),
  newQuery: newQueryFromSelection,
  newTask: () => void tasksPanel.edit(null),
});
const tabs = new TabManager(welcome);
let currentConfig: AppConfig | null = null;
let workspace: Workspace | null = null;
let workspaceArea: HTMLElement | null = null;

const navigation = new AppNavigation(tabs, {
  workspaceId,
  driverOf: (connectionId) => currentConfig?.workspaces.flatMap((item) => item.connections).find((profile) => profile.id === connectionId)?.driver,
  tasksPanel: () => tasksPanel,
  configureEmbedding: () => void configureEmbedding(),
  queryChanged: (view) => {
    if (tabs.activeContent() === view) assistant.refreshContext();
  },
  connectionName: (connectionId) =>
    currentConfig?.workspaces.flatMap((item) => item.connections).find((profile) => profile.id === connectionId)?.name ?? connectionId,
  historyEnabled: () => currentConfig?.queryHistory !== false,
  writesBlocked: (connectionId) => writesBlocked(connectionSafety(currentConfig?.workspaces ?? [], connectionId)),
});

const assistant = new AssistantPanel({
  context: assistantContext,
  insertSql: (text) => {
    const active = tabs.activeContent();
    if (isQueryView(active)) active.insertText(text);
    else openQueryWithText(text);
  },
  openQuery: openQueryWithText,
  perform: performAssistantAction,
  configure: () => void configureAssistant(),
  editProvider: (connection) => void editAiProvider(connection),
  configChanged: applyConfig,
  close: () => showAssistant(false),
});

const rail = new WorkspaceRail({
  switchTo: (id) => void switchWorkspace(id),
  create: () => void createWorkspace(),
  edit: (meta) => void editWorkspace(meta),
  remove: (meta) => void deleteWorkspace(meta),
  toggleTheme: () => {
    toggleTheme();
    setStatus(describeTheme());
    toast(describeTheme());
  },
  openSettings: (event) => showContextMenu(event, settingsMenu()),
  openThemeMenu: (event) => showContextMenu(event, themeMenu()),
});

const tree = new ConnectionTree({
  openTable: (target) => navigation.openTable(target),
  openDesign: (target) => navigation.openDesign(target),
  openQuery: (connectionId, database, initialSql) => navigation.openQuery(connectionId, database, initialSql),
  openDiagram: (profile, database) => navigation.openDiagram(profile, database),
  editConnection: (profile) => void editConnection(profile),
  deleteConnection: (profile) => void deleteConnection(profile),
  connectionClosed: (connectionId) => navigation.closeConnectionTabs(connectionId),
  configChanged: applyConfig,
  status: setStatus,
  stateChanged: () => {
    refreshActivity();
    assistant.refreshContext();
  },
});

const tasksPanel = new TasksPanel({
  openLogs: (task) => navigation.openLogs(task),
  configChanged: applyConfig,
  workspaceName: () => workspace?.name ?? "",
  summaryChanged: (text) => {
    taskSummary.textContent = text;
  },
  activityChanged: refreshActivity,
  askAssistant: (task, logLines) => {
    navigation.openLogs(task);
    showAssistant(true);
    assistant.ask(`Startup task "${task.name}" is not working. Read its log, explain what is wrong and how to fix it.`, taskLogContext(task, logLines));
  },
});

tabs.onActiveChange = () => assistant.refreshContext();

function assistantContext(): AssistantContext {
  return buildAssistantContext(tabs.activeContent(), currentConfig, tree);
}

/** Runs a view request from the assistant against the connection the chat is about. */
function performAssistantAction(action: UiAction): void {
  const context = assistantContext();
  const profile = context.connectionId ? tree.profileOf(context.connectionId) : null;
  if (!profile) {
    toast("The assistant wanted to open a view, but no connection is open");
    return;
  }
  switch (action.kind) {
    case "openDiagram":
      navigation.openDiagram(profile, action.database, action.focusTable ?? undefined);
      break;
    case "openTable":
      navigation.openTable(
        { connectionId: profile.id, driver: profile.driver, database: action.database, table: action.table, kind: "table" },
        action.suggestedFilter ?? "",
        true,
      );
      break;
    case "openQuery":
      navigation.openQuery(profile.id, action.database ?? context.database ?? null, action.sql);
      break;
  }
}

function openQueryWithText(text: string): void {
  const context = assistantContext();
  const connectionId = context.connectionId ?? tree.activeContext()?.connectionId;
  if (!connectionId) {
    toast("Connect to a database first to open a query tab");
    return;
  }
  navigation.openQuery(connectionId, context.database ?? null, text);
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

/** "LLM providers…": the active connection, or a new one when there is none. */
function configureAssistant(): Promise<void> {
  const active = currentConfig?.aiConnections.find((item) => item.id === currentConfig?.aiActive?.connectionId);
  return editAiProvider(active ?? null);
}

async function editAiProvider(connection: AiConnection | null): Promise<void> {
  const config = await aiDialog(connection);
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
  assistant.setConfigured(config.aiConnections, config.aiActive);
  assistant.setScope(workspace.id);
  welcomeName.textContent = workspace.name;
  renderSafetyStatus(workspace);
  refreshActivity();
  const title = `${workspace.name} — Fosta Studio`;
  if (isTauri()) void getCurrentWindow().setTitle(title);
  else document.title = title;
}

function renderSafetyStatus(current: Workspace): void {
  const relaxed = relaxedFromBaseline(current);
  const label = safetyLabel(safetyOf(current));
  safetyStatus.classList.toggle("relaxed", relaxed.length > 0);
  safetyStatus.replaceChildren(icon("shield"), h("span", {}, relaxed.length ? `${label} · rules relaxed` : label));
  safetyStatus.title = relaxed.length
    ? `Safety rules of ${current.name} are looser than its default:\n${relaxed.map((change) => change.text).join("\n")}`
    : `Safety rules of ${current.name}: ${label}`;
}

/** Closing the window rolls back open transactions; ask first. */
function guardWindowClose(): void {
  // The browser preview (dev mock) has no window to guard.
  if (!("__TAURI_INTERNALS__" in window)) return;
  void getCurrentWindow().onCloseRequested(async (event) => {
    const open = tabs.contents().filter((content) => isQueryView(content) && content.hasOpenTransaction()).length;
    if (open === 0) return;
    const message = `${open} query tab(s) have an open transaction. Closing Fosta Studio rolls them back, so uncommitted changes are lost. Close anyway?`;
    if (!(await confirmDialog(message, "Close"))) event.preventDefault();
  });
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
    { label: "LLM providers…", icon: "sparkle", run: () => void configureAssistant() },
    { label: "Embedding API…", icon: "vector", run: () => void configureEmbedding() },
    "separator",
    {
      label: "Record query history",
      icon: currentConfig?.queryHistory !== false ? "check" : undefined,
      run: () => void setQueryHistory(currentConfig?.queryHistory === false),
    },
    { label: "Clear query history…", icon: "trash", run: () => void clearQueryHistory() },
    "separator",
    {
      label: "Debug logging",
      icon: currentConfig?.debugLogging ? "check" : undefined,
      run: () => void setDebugLogging(!currentConfig?.debugLogging, applyConfig),
    },
    { label: "Copy diagnostics", icon: "copy", run: () => void copyDiagnostics() },
    { label: "Open log folder", icon: "logs", run: () => void openLogFolder() },
    ...(capabilities().edition === "web" ? (["separator", { label: "Sign out…", icon: "key", run: () => void confirmSignOut() }] as MenuItem[]) : []),
  ];
}

/** Web edition only: there is one access token per server, so signing out ends every browser's session. */
async function confirmSignOut(): Promise<void> {
  const message = "Sign out of Fosta Studio? This signs out every browser using this server; sign in again with the access token.";
  if (!(await confirmDialog(message, "Sign out"))) return;
  try {
    await signOut();
  } catch (error) {
    toast(errorText(error), "error");
  }
}

/** History stays on this machine (fosta-studio-history.jsonl next to the app); turning it off keeps old entries. */
async function setQueryHistory(enabled: boolean): Promise<void> {
  try {
    applyConfig(await api.setQueryHistory(enabled));
    setStatus(enabled ? "Query history is on" : "Query history is off; new runs are not recorded");
  } catch (error) {
    toast(errorText(error), "error");
  }
}

async function clearQueryHistory(): Promise<void> {
  if (!(await confirmDialog("Delete the whole query history of every connection on this machine?", "Delete"))) return;
  try {
    await api.historyClear();
    toast("Query history cleared", "success");
  } catch (error) {
    toast(errorText(error), "error");
  }
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

async function configureEmbedding(): Promise<void> {
  const config = await embeddingDialog(currentConfig?.embedding ?? null);
  if (config) applyConfig(config);
}

function diagramFromSelection(): void {
  const context = tree.activeContext();
  const profile = context ? tree.profileOf(context.connectionId) : null;
  if (!context || !profile) {
    toast("Connect to a database first (double-click a connection)");
    return;
  }
  if (!context.database) {
    toast("Select a database in the tree, then open its diagram");
    return;
  }
  navigation.openDiagram(profile, context.database);
}

/** Opens a query tab on the selected connection, connecting it first when needed. */
function newQueryFromSelection(): void {
  void tree.connectedContext().then((context) => {
    if (context) navigation.openQuery(context.connectionId, context.database);
    else if (!tree.hasConnections()) toast("Add a connection first, then open a query on it");
    else toast("Select a connection in the tree to open a query on it");
  });
}

async function editConnection(profile: ConnectionProfile | null): Promise<void> {
  const config = await connectionDialog(profile, workspace?.name ?? "");
  if (config) applyConfig(config);
}

async function deleteConnection(profile: ConnectionProfile): Promise<void> {
  if (!(await confirmDialog(`Delete connection "${profile.name}"?`, "Delete"))) return;
  try {
    navigation.closeConnectionTabs(profile.id);
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
    navigation.closeWorkspaceTabs(target.id);
    applyConfig(await api.deleteWorkspace(target.id));
    setStatus(`Deleted workspace ${target.name}`);
  } catch (error) {
    toast(errorText(error), "error");
  }
}

function buildLayout(): void {
  const toolbar = h(
    "header",
    { class: "app-toolbar" },
    bigButton("plug", "Connection", () => void editConnection(null), "New connection (Ctrl+N)"),
    bigButton("query", "Query", newQueryFromSelection, "New query (Ctrl+T)"),
    bigButton("diagram", "Diagram", diagramFromSelection, "ER diagram of the selected database"),
    bigButton("refresh", "Refresh", () => tree.refreshSelected(), "Reload the selected database"),
    separator(),
    withRequirement(
      bigButton("task", "Task", () => void tasksPanel.edit(null), "New startup task"),
      "tasks",
    ),
    bigButton("vector", "Embedding", () => void configureEmbedding(), "Embedding API for vector search"),
    spacer(),
    bigButton("sparkle", "Assistant", toggleAssistant, "Chat with your database (Ctrl+Shift+A)"),
  );
  const sidebar = h(
    "aside",
    { class: "sidebar" },
    workspaceHeader,
    h(
      "div",
      { class: "panel-header" },
      h("span", {}, "Connections"),
      iconButton("plus", "New connection", () => void editConnection(null)),
    ),
    tree.element,
    withRequirement(tasksPanel.element, "tasks"),
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
  const version = h("span", { class: "status-version", title: "Fosta Studio version" }, `v${__APP_VERSION__}`);
  const statusBar = h("footer", { class: "status-bar" }, statusText, spacer(), safetyStatus, taskSummary, version);
  const main = h("div", { class: "app-main" }, toolbar, area, statusBar);
  (document.getElementById("app") ?? document.body).append(rail.element, main);
}

function installShortcuts(): void {
  document.addEventListener("keydown", (event) => {
    if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
    const key = event.key.toLowerCase();
    // Shortcuts would act behind an open dialog (or stack a second one); keep the browser's own meaning off too.
    if (isModalOpen()) {
      if (/^[1-9nt]$/.test(key) && !event.shiftKey) event.preventDefault();
      return;
    }
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
  if (isTauri()) requestAnimationFrame(() => requestAnimationFrame(() => void getCurrentWindow().show()));
}

async function bootstrap(): Promise<void> {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    // Browser-only preview (`npm run dev`): a mock backend with sample data.
    const { installDevMock } = await import("./dev/devMock");
    installDevMock();
  }
  const supported = await loadCapabilities();
  initTheme();
  onThemeChange(() => rail.refreshTheme());
  buildLayout();
  hideUnsupported(document);
  hideUnsupported(welcome);
  installShortcuts();
  suppressBrowserContextMenu();
  guardWindowClose();
  revealWindowAfterFirstPaint();
  preloadQueryView();
  try {
    const config = await api.getConfig();
    applyConfig(config);
    if (config.startupNotice) toast(config.startupNotice, "info");
    if (supported.tasks) await tasksPanel.init();
  } catch (error) {
    toast(errorText(error), "error");
  }
}

/** Marks an element as needing a capability; `hideUnsupported` hides it when the edition lacks it. */
function withRequirement<T extends HTMLElement>(element: T, capability: keyof Capabilities): T {
  element.dataset.requires = capability;
  return element;
}

void bootstrap();
