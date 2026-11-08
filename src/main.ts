/**
 * App shell: builds the layout (rail, toolbar, sidebar, tabs, assistant,
 * status bar), wires the feature modules together and holds the current
 * config. Opening tabs lives in `app/navigation.ts`.
 */
import "./styles.css";
import { showError } from "./core/errorPanel";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { assistantContext as buildAssistantContext, isQueryView, taskLogContext } from "./app/assistantContext";
import { AppNavigation, preloadQueryView } from "./app/navigation";
import { showWelcomeEngines, welcomeScreen } from "./app/welcome";
import { renderStatusContext } from "./app/statusBar";
import { buildLayout, ASSISTANT_STORAGE_KEY } from "./app/layout";
import { shellMenus } from "./app/menus";
import { guardWindowClose, installShortcuts, revealWindowAfterFirstPaint, suppressBrowserContextMenu } from "./app/windowShell";
import { api } from "./core/api";
import { hideUnsupported, loadCapabilities } from "./core/capabilities";
import { loadEngines } from "./core/engines";
import { initDensity } from "./core/density";
// Shared view registrations and the lazy engine UI modules (`src/engines/`).
import "./engines";
import { isTauri } from "./core/transport";
import { showContextMenu } from "./core/contextMenu";
import { confirmDialog, toast } from "./core/dialogs";
import { h, icon, iconButton } from "./core/dom";
import { TabManager } from "./core/tabs";
import { describeTheme, initTheme, onThemeChange, toggleTheme } from "./core/theme";
import type { AiConnection, AppConfig, ConnectionProfile, UiAction, Workspace, WorkspaceMeta } from "./core/types";
import { aiDialog } from "./features/assistant/aiDialog";
import { AssistantPanel, type AssistantContext } from "./features/assistant/assistantPanel";
import { connectionDialog } from "./features/connections/connectionDialog";
import { ConnectionTree } from "./features/connections/tree";
import { TasksPanel } from "./features/tasks/tasksPanel";
import { embeddingDialog } from "./features/vector/embeddingDialog";
import { connectionSafety, relaxedFromBaseline, safetyLabel, safetyOf, writesBlocked } from "./features/safety/safety";
import { workspaceDialog } from "./features/workspaces/workspaceDialog";
import { activeWorkspace, WorkspaceRail, workspaceColor, workspaceIcon, type WorkspaceActivity } from "./features/workspaces/workspaceRail";

const statusText = h("span", { class: "status-text" });
const statusContext = h("span", { class: "status-context" });
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

const menus = shellMenus({
  config: () => currentConfig,
  applyConfig,
  setStatus,
  configureAssistant,
  configureEmbedding,
  editWorkspace,
  createWorkspace,
  deleteWorkspace: (current) => deleteWorkspace(current),
});

const navigation = new AppNavigation(tabs, {
  workspaceId,
  profileOf: (connectionId) => currentConfig?.workspaces.flatMap((item) => item.connections).find((profile) => profile.id === connectionId),
  tasksPanel: () => tasksPanel,
  configureEmbedding: () => void configureEmbedding(),
  queryChanged: (view) => {
    if (tabs.activeContent() === view) {
      assistant.refreshContext();
      refreshStatusContext();
    }
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
  openSettings: (event) => showContextMenu(event, menus.settingsMenu()),
  openThemeMenu: (event) => showContextMenu(event, menus.themeMenu()),
});

const tree = new ConnectionTree({
  openObject: (profile, node, role, params) => navigation.openObject(profile, node, role, params),
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

tabs.onActiveChange = () => {
  assistant.refreshContext();
  refreshStatusContext();
};

function refreshStatusContext(): void {
  renderStatusContext(statusContext, assistantContext(), tabs.activeContent());
}

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
  refreshStatusContext();
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

function renderWorkspaceHeader(current: Workspace): void {
  const menuButton = iconButton("more", "Workspace options", () => undefined);
  menuButton.addEventListener("click", (event) => showContextMenu(event, menus.workspaceMenu(current)));
  workspaceHeader.replaceChildren(
    h("span", { class: `ws-dot color-${workspaceColor(current.color)}` }, icon(workspaceIcon(current.icon))),
    h("span", { class: "ws-name", title: current.name }, current.name),
    menuButton,
  );
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
  const config = await connectionDialog(profile);
  if (config) applyConfig(config);
}

async function deleteConnection(profile: ConnectionProfile): Promise<void> {
  if (!(await confirmDialog(`Delete connection "${profile.name}"?`, "Delete"))) return;
  try {
    navigation.closeConnectionTabs(profile.id);
    applyConfig(await api.deleteConnection(profile.id));
  } catch (error) {
    showError(error, "Delete connection");
  }
}

async function switchWorkspace(id: string): Promise<void> {
  if (id === workspaceId()) return;
  try {
    applyConfig(await api.setActiveWorkspace(id));
    await tasksPanel.refreshStatuses();
    setStatus(`Workspace: ${workspace?.name ?? id}`);
  } catch (error) {
    showError(error, "Switch workspace");
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
    showError(error, "Delete workspace");
  }
}

async function bootstrap(): Promise<void> {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    // Browser-only preview (`npm run dev`): a mock backend with sample data.
    const { installDevMock } = await import("./dev/mock");
    installDevMock();
  }
  // Engine descriptors drive the connection dialog, the tree and the views; load them before the UI is built.
  const [supported] = await Promise.all([
    loadCapabilities(),
    loadEngines().catch((error: unknown) => {
      queueMicrotask(() => showError(error, "Load the engine list"));
      return [];
    }),
  ]);
  showWelcomeEngines(welcome);
  initDensity();
  initTheme();
  onThemeChange(() => rail.refreshTheme());
  workspaceArea = buildLayout({
    workspaceHeader,
    statusText,
    statusContext,
    safetyStatus,
    taskSummary,
    tree,
    tasksPanel,
    tabs,
    assistant,
    rail,
    newConnection: () => void editConnection(null),
    newQuery: newQueryFromSelection,
    newDiagram: diagramFromSelection,
    configureEmbedding: () => void configureEmbedding(),
    toggleAssistant,
  });
  hideUnsupported(document);
  hideUnsupported(welcome);
  installShortcuts({
    config: () => currentConfig,
    switchWorkspace: (id) => void switchWorkspace(id),
    newConnection: () => void editConnection(null),
    newQuery: newQueryFromSelection,
    toggleAssistant,
  });
  suppressBrowserContextMenu();
  guardWindowClose(tabs);
  revealWindowAfterFirstPaint();
  preloadQueryView();
  try {
    const config = await api.getConfig();
    applyConfig(config);
    if (config.startupNotice) toast(config.startupNotice, "info");
    if (supported.tasks) await tasksPanel.init();
  } catch (error) {
    showError(error, "Load the configuration");
  }
}

void bootstrap();
