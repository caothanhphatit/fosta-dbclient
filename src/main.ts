/**
 * App shell: builds the layout (rail, toolbar, sidebar, tabs, assistant,
 * status bar), wires the feature modules together and holds the current
 * config. Opening tabs lives in `app/navigation.ts`.
 */
import "./styles.css";
import { showError } from "./core/errorPanel";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { assistantContext as buildAssistantContext, composerTakesFocus, isQueryView, taskLogContext, typingElsewhere } from "./app/assistantContext";
import { AppNavigation, preloadQueryView } from "./app/navigation";
import { showWelcomeStart, welcomeScreen } from "./app/welcome";
import { renderStatusContext } from "./app/statusBar";
import { buildLayout, ASSISTANT_STORAGE_KEY, toggleSidebar } from "./app/layout";
import { shellMenus } from "./app/menus";
import { appActionEntry, appActionRegistry } from "./app/appActions";
import { installOnExit, openRelease, startUpdates } from "./app/update";
import { COMMUNITY_URL } from "./app/updateLogic";
import { initMcpApprovals } from "./features/mcp/mcpApproval";
import { initMcpStatus } from "./features/mcp/mcpSettings";
import { TabPersistence } from "./app/tabPersistence";
import { confirmQuit, guardPageUnload, guardWindowClose, installShortcuts, revealWindowAfterFirstPaint, suppressBrowserContextMenu } from "./app/windowShell";
import { api, onConnectionLost } from "./core/api";
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
import type { AssistantSuggestion } from "./core/assistantFences";
import type { ObjectRef } from "./core/contract";
import { aiDialog } from "./features/assistant/aiDialog";
import { AssistantPanel, type AssistantContext } from "./features/assistant/assistantPanel";
import { learnDialog, type LearnTarget } from "./features/assistant/learnDialog";
import { learningAllowed } from "./features/assistant/profileLogic";
import { knowledgeTabKey, ProfileView } from "./features/assistant/profileView";
import { connectionDialog } from "./features/connections/connectionDialog";
import { ConnectionTree } from "./features/connections/tree";
import { TasksPanel } from "./features/tasks/tasksPanel";
import { embeddingDialog } from "./features/vector/embeddingDialog";
import { setConfirmationConfigSink } from "./features/safety/confirmationDialog";
import { dismissedProductionHints, dismissProductionHint, productionWorkspace, shouldOfferProductionMove } from "./features/safety/productionHint";
import { connectionSafety, PRODUCTION_ICON, relaxedFromBaseline, safetyLabel, safetyOf, writesBlocked } from "./features/safety/safety";
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
  openCommunity: () => openRelease(COMMUNITY_URL),
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

/** The app actions the assistant may suggest as chat buttons; each runs on the user's click only. */
const appActions = appActionRegistry({
  newConnection: () => void editConnection(null),
  newWorkspace: () => void createWorkspace(),
  editWorkspace: () => {
    if (workspace) void editWorkspace(workspace);
  },
  newCommand: () => void tasksPanel.edit(null),
  newQuery: newQueryFromSelection,
  llmProviders: () => void configureAssistant(),
  embeddingSettings: () => void configureEmbedding(),
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
  ensureConnected: (connectionId): Promise<boolean> => tree.ensureConnected(connectionId),
  schemaMayHaveChanged: (connectionId) => assistant.schemaMayHaveChanged(connectionId),
  askAssistant: (prompt, extra, samples, shown) => {
    showAssistant(true);
    assistant.ask(prompt, extra, samples, shown);
  },
  taskOf: (taskId) => currentConfig?.workspaces.flatMap((item) => item.tasks).find((task) => task.id === taskId),
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
  openWorkspace: (connectionId, object, section, request) => navigation.openWorkspace(connectionId, object, section, request),
  perform: performAssistantAction,
  appAction: (id) => appActionEntry(appActions, id),
  configure: () => void configureAssistant(),
  editProvider: (connection) => void editAiProvider(connection),
  configChanged: applyConfig,
  close: () => showAssistant(false),
  learn: (connectionId, database) => void learnDatabase(connectionId, database),
  openKnowledge: (connectionId, database) => openKnowledge(connectionId, database),
  canLearn: (connectionId) => learningAllowed(currentConfig, connectionId),
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
  newConnection: () => void editConnection(null),
  openCommandLog: (commandId) => {
    const task = workspace?.tasks.find((item) => item.id === commandId);
    if (task) navigation.openLogs(task);
  },
  commandState: (commandId) => tasksPanel.stateOf(commandId),
  connectionClosed: (connectionId) => void navigation.closeConnectionTabs(connectionId),
  refreshed: (connectionId, database) => navigation.refreshQueryNames(connectionId, database),
  configChanged: applyConfig,
  status: setStatus,
  learnDatabase: (profile, database) => void learnDatabase(profile.id, database),
  openKnowledge: (profile, database) => openKnowledge(profile.id, database),
  stateChanged: () => {
    refreshActivity();
    assistant.refreshContext();
  },
});

// Selecting a node changes what the chat is about when no tab says otherwise: the panel (context, Learn, its
// conversation) and the status bar follow at once. The tree's own handlers select first; these run as the event bubbles.
for (const type of ["click", "keydown", "contextmenu"]) {
  tree.element.addEventListener(type, () => {
    assistant.refreshContext();
    refreshStatusContext();
  });
}

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
    assistant.ask(`Command "${task.name}" is not working. Read its log, explain what is wrong and how to fix it.`, taskLogContext(task, logLines));
  },
});

tabs.onActiveChange = () => {
  assistant.refreshContext();
  refreshStatusContext();
};

/** The open tabs of every workspace come back on the next start. */
const persistence = new TabPersistence(tabs, navigation, () => currentConfig?.workspaces.map((item) => item.id) ?? []);
tabs.onTabsChanged = () => persistence.schedule();
navigation.onTabEdited = () => persistence.schedule();

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
    case "openCollectionWorkspace": {
      // Pinned to the chat's database (the backend refuses others too).
      if (action.database !== context.database) {
        toast(`The assistant asked to open ${action.database}.${action.collection}, outside the chat's database; it was not opened`, "error");
        break;
      }
      // Kind as the active tab knows it (a view opens in the same workspace); a collection otherwise.
      const same = context.object && context.object.database === action.database && context.object.name === action.collection;
      const object: ObjectRef = { database: action.database, schema: null, name: action.collection, kind: same ? context.object!.kind : "collection" };
      const suggestion = {
        filter: action.suggestedFilter,
        sort: action.suggestedSort,
        project: action.suggestedProject,
        pipeline: action.suggestedPipeline,
      };
      // Always marked as the assistant's, even without a suggestion: the section opens without running anything.
      navigation.openWorkspace(profile.id, object, action.section, { assistant: suggestion } satisfies AssistantSuggestion);
      break;
    }
  }
}

function learnTarget(connectionId: string, database: string): LearnTarget {
  const name = currentConfig?.workspaces.flatMap((item) => item.connections).find((profile) => profile.id === connectionId)?.name;
  return { connectionId, database, connectionName: name ?? connectionId };
}

/** "Learn this database" (connecting first); a refresh with `onlyChanged` describes only new or changed objects. */
async function learnDatabase(connectionId: string, database: string, onlyChanged = false): Promise<{ learned: boolean }> {
  if (!(await tree.ensureConnected(connectionId))) return { learned: false };
  const result = await learnDialog(currentConfig, learnTarget(connectionId, database), {
    onlyChanged,
    openKnowledge: () => openKnowledge(connectionId, database),
  });
  if (result.learned) assistant.knowledgeChanged(connectionId, database);
  // An open "What the assistant knows" tab shows the new run (a cancel can still have saved the structure).
  const open = tabs.contentOf(knowledgeTabKey(workspaceId(), connectionId, database));
  if (open instanceof ProfileView) open.reload();
  return result;
}

/** The "What the assistant knows" tab of one database (not restored on the next start). */
function openKnowledge(connectionId: string, database: string): void {
  const target = learnTarget(connectionId, database);
  tabs.open({
    key: knowledgeTabKey(workspaceId(), connectionId, database),
    title: `${database || target.connectionName} · assistant knows`,
    icon: "book",
    connection: target.connectionName,
    database,
    create: () =>
      new ProfileView(target, {
        learn: (item, onlyChanged) => learnDatabase(item.connectionId, item.database, onlyChanged),
        changed: (item) => assistant.knowledgeChanged(item.connectionId, item.database),
        statusSeen: (item, status) => assistant.setKnowledge(item.connectionId, item.database, status),
      }),
  });
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

/**
 * Shows or hides the assistant panel. Opening it never moves the focus, except from the keyboard shortcut (`byKeyboard`,
 * the user wants to type a question) and then not away from an editor or a field that has it.
 */
function showAssistant(visible: boolean, byKeyboard = false): void {
  workspaceArea?.classList.toggle("with-assistant", visible);
  try {
    localStorage.setItem(ASSISTANT_STORAGE_KEY, visible ? "open" : "closed");
  } catch {
    // Storage can be unavailable; the panel state then lasts for this session only.
  }
  if (visible) {
    assistant.refreshContext();
    assistant.recheckKnowledge();
    if (composerTakesFocus(byKeyboard, typingElsewhere(document.activeElement, assistant.element))) assistant.focus();
  }
}

function hideOrShowSidebar(): void {
  if (workspaceArea) toggleSidebar(workspaceArea);
}

function toggleAssistant(byKeyboard = false): void {
  showAssistant(!workspaceArea?.classList.contains("with-assistant"), byKeyboard);
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
  tasksPanel.setTasks(workspace.tasks, workspace.connections);
  tabs.setScope(workspace.id);
  assistant.setConfigured(config.aiConnections, config.aiActive);
  assistant.setScope(workspace.id);
  welcomeName.textContent = workspace.name;
  showWelcomeStart(welcome, workspace.connections.length === 0);
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
    iconButton("panel", "Hide the sidebar (Ctrl+B)", hideOrShowSidebar),
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
  const before = new Set((workspace?.connections ?? []).map((item) => item.id));
  const config = await connectionDialog(profile, {
    commands: workspace?.tasks ?? [],
    create: async (target) => (await tasksPanel.edit(null, { forConnection: target }))?.task ?? null,
  });
  if (!config) return;
  applyConfig(config);
  const saved = workspace?.connections.find((item) => (profile ? item.id === profile.id : !before.has(item.id)));
  if (saved) await offerProductionMove(saved);
}

/** A connection that looks like production, saved outside a Production workspace: offer to move it there (read-only by default). */
async function offerProductionMove(profile: ConnectionProfile): Promise<void> {
  const current = workspace;
  if (!current || !currentConfig || !shouldOfferProductionMove(profile, current, dismissedProductionHints())) return;
  const target = productionWorkspace(currentConfig.workspaces);
  const where = target ? `the ${target.name} workspace` : "a new Production workspace";
  const move = await confirmDialog(`"${profile.name}" looks like a production database. Move it to ${where}, where it is read-only by default?`, "Move");
  if (!move) {
    dismissProductionHint(profile.id);
    return;
  }
  try {
    let targetId = target?.id;
    if (!targetId) {
      targetId = crypto.randomUUID();
      applyConfig(await api.saveWorkspace({ id: targetId, name: "Production", color: "red", icon: PRODUCTION_ICON }));
    }
    applyConfig(await api.saveConnection(profile, null, targetId));
    toast(`Moved "${profile.name}" to ${target?.name ?? "Production"} (read-only)`);
  } catch (error) {
    showError(error, "Move connection");
  }
}

async function deleteConnection(profile: ConnectionProfile): Promise<void> {
  if (!(await confirmDialog(`Delete connection "${profile.name}"?`, "Delete"))) return;
  try {
    // A tab the user keeps (its unsaved work) keeps the connection too.
    if (!(await navigation.closeConnectionTabs(profile.id))) return;
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
  const summary = `${target.connections.length} connection(s) and ${target.tasks.length} command(s) will be removed, and saved passwords forgotten.`;
  if (!(await confirmDialog(`Delete workspace "${target.name}"?\n${summary}`, "Delete"))) return;
  try {
    // A tab the user keeps (its unsaved work) keeps the workspace too: no tab is left in a deleted workspace.
    if (!(await navigation.closeWorkspaceTabs(target.id))) {
      setStatus(`Workspace ${target.name} was not deleted: its tabs stay open`);
      return;
    }
    applyConfig(await api.deleteWorkspace(target.id));
    persistence.forget(target.id);
    setStatus(`Deleted workspace ${target.name}`);
  } catch (error) {
    showError(error, "Delete workspace");
  }
}

async function bootstrap(): Promise<void> {
  setConfirmationConfigSink(applyConfig);
  // A call that finds its connection gone (the backend restarted) shows it as closed in the tree.
  onConnectionLost((connectionId) => tree.connectionLost(connectionId));
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
    toggleAssistant: () => toggleAssistant(),
    toggleSidebar: hideOrShowSidebar,
  });
  hideUnsupported(document);
  hideUnsupported(welcome);
  installShortcuts({
    config: () => currentConfig,
    switchWorkspace: (id) => void switchWorkspace(id),
    newConnection: () => void editConnection(null),
    newQuery: newQueryFromSelection,
    // From the keyboard: the user wants to type a question.
    toggleAssistant: () => toggleAssistant(true),
    toggleSidebar: hideOrShowSidebar,
    toggleRail: () => rail.toggleHidden(),
  });
  suppressBrowserContextMenu();
  guardWindowClose(
    tabs,
    async () => {
      await persistence.flush();
      await installOnExit();
    },
    () => persistence.textLostKeys(),
  );
  guardPageUnload(tabs, () => persistence.saveOnLeave());
  revealWindowAfterFirstPaint();
  preloadQueryView();
  try {
    const config = await api.getConfig();
    applyConfig(config);
    if (config.startupNotice) toast(config.startupNotice, "info");
    void startUpdates({
      confirmRestart: async () => {
        const go = await confirmQuit(tabs, "Restart", persistence.textLostKeys());
        // The tabs (and query text) come back after the update.
        if (go) await persistence.flush();
        return go;
      },
    });
    void initMcpStatus(applyConfig);
    void initMcpApprovals();
    if (supported.tasks) await tasksPanel.init();
    await persistence.restore(config);
  } catch (error) {
    showError(error, "Load the configuration");
  }
}

void bootstrap();
