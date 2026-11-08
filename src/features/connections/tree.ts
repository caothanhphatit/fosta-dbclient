/**
 * The connection tree in the sidebar: one row per saved connection, and under a connected one the engine's object tree
 * (`tree_children`, loaded lazily per node and page by page). Rows, icons, row counts (`detail`), badges and context-menu
 * entries all come from the engine's `ObjectNode`s and `NodeAction`s; the tree itself knows no engine.
 */
import "./tree.css";
import { api, errorCode, errorText, PASSWORD_REQUIRED } from "../../core/api";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import type { BuiltinAction, NodeAction, ObjectNode } from "../../core/contract";
import { promptPassword, toast } from "../../core/dialogs";
import { copyText, h, icon, type IconName } from "../../core/dom";
import { capabilityOf, connectInfo, describeServer, engineOf, hasCapability, setConnectInfo, type EngineInfo } from "../../core/engines";
import { showError } from "../../core/errorPanel";
import { dialectOf } from "../../core/sql";
import type { TabContent } from "../../core/tabs";
import type { AppConfig, ConnectionProfile } from "../../core/types";
import { loadEngineUi, nodeIcon, type ViewRole } from "../../core/views";
import { withConfirmation } from "../safety/confirmationDialog";
import { databaseDialog } from "./databaseDialog";
import { fillTemplate, plainName, ROOT, TreeModel, type VisibleRow } from "./treeModel";

type ConnectionState = "idle" | "connecting" | "connected" | "error";

const STATUS_LABELS: Record<ConnectionState, string> = {
  idle: "Disconnected",
  connecting: "Connecting…",
  connected: "Connected",
  error: "Connection failed",
};

/** Children asked for per `tree_children` call; more come with the "Load more" row. */
const PAGE_SIZE = 200;

/** Builtins the connection row always offers itself, so the same entries in `connectionActions` are not repeated. */
const CONNECTION_BUILTINS = new Set<BuiltinAction>(["newQuery", "refresh", "createDatabase"]);

interface ConnectionNode {
  profile: ConnectionProfile;
  state: ConnectionState;
  expanded: boolean;
  message: string | null;
  /** Version string the server reported, once connected. */
  version: string | null;
  model: TreeModel;
  /** Bumped on connect and disconnect, so a page that arrives for an older session is dropped. */
  generation: number;
}

export interface TreeContext {
  connectionId: string;
  database: string | null;
}

/** What one drawn row does; also drives keyboard navigation. */
interface RowSpec {
  key: string;
  context: TreeContext | null;
  depth: number;
  iconName: IconName | null;
  label: string;
  detail?: string | null;
  badges?: ObjectNode["badges"];
  meta?: string | null;
  tag?: string;
  title?: string;
  className?: string;
  /** Set for rows that open and close; true when open. */
  expanded?: boolean;
  onToggle?: () => void;
  onOpen: () => void;
  /** A single click opens (the "Load more" row) instead of a double-click. */
  singleClick?: boolean;
  menu?: () => MenuItem[];
  /** Key of the row above this one in the hierarchy (ArrowLeft). */
  parentKey?: string;
  /** Connection status dot drawn on the icon. */
  status?: ConnectionState;
  /** Small button shown on hover at the end of the row. */
  action?: { icon: IconName; title: string; run: () => void };
}

export interface TreeHost {
  /** Opens the view registered for the node's kind and `role` (no node: the connection itself). */
  openObject(profile: ConnectionProfile, node: ObjectNode | null, role: ViewRole, params?: Record<string, unknown>): Promise<TabContent | null>;
  openQuery(connectionId: string, database: string | null, initialSql?: string): void;
  openDiagram(profile: ConnectionProfile, database: string): void;
  editConnection(profile: ConnectionProfile): void;
  deleteConnection(profile: ConnectionProfile): void;
  connectionClosed(connectionId: string): void;
  configChanged(config: AppConfig): void;
  status(message: string): void;
  /** Fired whenever a connection changes state (connected, error, …). */
  stateChanged?(): void;
}

export class ConnectionTree {
  readonly element: HTMLElement;
  private readonly host: TreeHost;
  private readonly body = h("div", { class: "tree-body", tabindex: "0", role: "tree", "aria-label": "Connections" });
  private readonly search = h("input", { class: "input", placeholder: "Search", spellcheck: "false", "aria-label": "Search the tree" });
  private nodes: ConnectionNode[] = [];
  /** Nodes of every workspace seen so far, so switching back keeps their open state. */
  private readonly cache = new Map<string, ConnectionNode>();
  private rows = new Map<string, RowSpec>();
  private selectedKey: string | null = null;

  constructor(host: TreeHost) {
    this.host = host;
    this.search.addEventListener("input", () => this.render());
    this.search.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && this.search.value) {
        this.search.value = "";
        this.render();
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        this.body.focus();
        this.moveSelection(1);
      }
    });
    this.body.addEventListener("keydown", (event) => this.handleKey(event));
    this.element = h("div", { class: "tree" }, h("div", { class: "search-box" }, icon("search"), this.search), this.body);
  }

  setConnections(profiles: ConnectionProfile[]): void {
    this.nodes = profiles.map((profile): ConnectionNode => {
      const node = this.cache.get(profile.id) ?? {
        profile,
        state: "idle",
        expanded: false,
        message: null,
        version: null,
        model: new TreeModel(),
        generation: 0,
      };
      node.profile = profile;
      this.cache.set(profile.id, node);
      return node;
    });
    this.render();
  }

  activeContext(): TreeContext | null {
    const selected = this.selectedKey ? this.rows.get(this.selectedKey)?.context : undefined;
    if (selected && this.nodeOf(selected.connectionId)?.state === "connected") return selected;
    const connected = this.nodes.find((node) => node.state === "connected");
    return connected ? { connectionId: connected.profile.id, database: connected.profile.database } : null;
  }

  hasConnections(): boolean {
    return this.nodes.length > 0;
  }

  /**
   * Like `activeContext`, but connects first when nothing is connected yet: the selected connection, or the only
   * one in the workspace. Null when there is nothing to connect to (or connecting failed or was cancelled).
   */
  async connectedContext(): Promise<TreeContext | null> {
    const ready = this.activeContext();
    if (ready) return ready;
    const selected = this.selectedKey ? this.rows.get(this.selectedKey)?.context : undefined;
    const node = selected ? this.nodeOf(selected.connectionId) : this.nodes.length === 1 ? this.nodes[0] : undefined;
    if (!node || node.state === "connecting") return null;
    await this.connect(node);
    return this.activeContext();
  }

  /** Profile of a connection in the current workspace, if known. */
  profileOf(connectionId: string): ConnectionProfile | null {
    return this.nodeOf(connectionId)?.profile ?? null;
  }

  /** Version reported by a connected server, or null when not connected. */
  serverVersion(connectionId: string): string | null {
    const node = this.nodeOf(connectionId);
    return node?.state === "connected" ? node.version : null;
  }

  connectedIds(): Set<string> {
    return new Set([...this.cache.values()].filter((node) => node.state === "connected").map((node) => node.profile.id));
  }

  /** The toolbar's Refresh: the selected node's children (or its parent's, for a leaf), else the connection's top level. */
  refreshSelected(): void {
    const selected = this.selectedKey ? parseKey(this.selectedKey) : null;
    const conn = selected ? this.nodeOf(selected.connectionId) : this.nodes.find((node) => node.state === "connected");
    if (!conn || conn.state !== "connected") return;
    const node = selected?.nodeId ? conn.model.node(selected.nodeId) : undefined;
    void this.refresh(conn, node ? this.refreshTarget(conn, node) : ROOT);
  }

  private nodeOf(connectionId: string): ConnectionNode | undefined {
    return this.nodes.find((node) => node.profile.id === connectionId);
  }

  // --- Connecting -------------------------------------------------------------------------------------------------------

  private async connect(conn: ConnectionNode, password: string | null = null): Promise<void> {
    conn.state = "connecting";
    conn.message = null;
    this.render();
    try {
      const info = await api.connect(conn.profile.id, password);
      setConnectInfo(conn.profile.id, info);
      conn.state = "connected";
      conn.version = info.server.version;
      conn.expanded = true;
      conn.generation += 1;
      conn.model.clear();
      this.host.status(`Connected to ${conn.profile.name} · ${describeServer(info)}`);
      // The engine's UI module maps its own node icons; without it the nodes fall back to generic icons.
      await loadEngineUi(info.engine).catch(() => undefined);
      await this.loadChildren(conn, ROOT, "first");
    } catch (error) {
      const message = errorText(error);
      const needsPassword = errorCode(error) === PASSWORD_REQUIRED || message === PASSWORD_REQUIRED;
      conn.state = needsPassword ? "idle" : "error";
      conn.message = needsPassword ? null : message;
      this.render();
      if (needsPassword) await this.askPasswordAndConnect(conn);
      else showError(error, `Connect to ${conn.profile.name}`);
    }
  }

  private async askPasswordAndConnect(conn: ConnectionNode): Promise<void> {
    const answer = await promptPassword(conn.profile.name);
    if (!answer) return;
    if (answer.remember) {
      this.host.configChanged(await api.saveConnection({ ...conn.profile, savePassword: true }, answer.password));
    }
    await this.connect(conn, answer.password);
  }

  private async disconnect(conn: ConnectionNode): Promise<void> {
    try {
      await api.disconnect(conn.profile.id);
    } catch (error) {
      showError(error, `Disconnect ${conn.profile.name}`);
    }
    conn.state = "idle";
    conn.expanded = false;
    conn.generation += 1;
    conn.model.clear();
    setConnectInfo(conn.profile.id, null);
    this.host.connectionClosed(conn.profile.id);
    this.host.status(`Disconnected from ${conn.profile.name}`);
    this.render();
  }

  // --- Loading ----------------------------------------------------------------------------------------------------------

  /**
   * Loads children of `parentId`: its first page, the next page ("Load more"), or a refresh (as many as are loaded now).
   * Failures are reported in the error panel and leave an error row when nothing was loaded before.
   */
  private async loadChildren(conn: ConnectionNode, parentId: string, mode: "first" | "more" | "refresh"): Promise<void> {
    const model = conn.model;
    const list = model.list(parentId);
    if (list?.loading) return;
    const parent = parentId === ROOT ? null : (model.node(parentId)?.key ?? null);
    if (parentId !== ROOT && !parent) return;
    const append = mode === "more";
    const cursor = append ? (list?.nextCursor ?? null) : null;
    if (append && !cursor) return;
    const limit = mode === "refresh" ? model.reloadLimit(parentId, PAGE_SIZE) : PAGE_SIZE;
    const generation = conn.generation;
    model.beginLoad(parentId);
    this.render();
    try {
      const page = await api.treeChildren(conn.profile.id, { parent, cursor, limit, filter: null });
      if (conn.generation !== generation) return;
      model.receive(parentId, page, append);
    } catch (error) {
      if (conn.generation !== generation) return;
      model.fail(parentId, errorText(error));
      const what = parentId === ROOT ? conn.profile.name : (model.node(parentId)?.label ?? "the node");
      showError(error, `List the contents of ${what}`);
    }
    this.render();
  }

  /** The "Load more" row: the next page, then the selection moves to its first node (the row it was on is gone). */
  private async loadMore(conn: ConnectionNode, parentId: string): Promise<void> {
    const before = conn.model.list(parentId)?.ids.length ?? 0;
    const wasSelected = this.selectedKey === `m|${conn.profile.id}|${parentId}`;
    await this.loadChildren(conn, parentId, "more");
    const first = conn.model.list(parentId)?.ids[before];
    if (wasSelected && first !== undefined) this.selectKey(rowKey(conn.profile.id, first));
  }

  /** Reloads `parentId` and, below it, every expanded node that was loaded (parents first, so vanished nodes are skipped). */
  private async refresh(conn: ConnectionNode, parentId: string): Promise<void> {
    const targets = conn.model.reloadTargets(parentId);
    if (targets.length === 0) {
      await this.loadChildren(conn, parentId, "first");
      return;
    }
    for (const id of targets) {
      if (id !== ROOT && !conn.model.node(id)) continue;
      await this.loadChildren(conn, id, "refresh");
    }
  }

  /** The list a refresh of `node` reloads: its own children when it has some, else its parent's. */
  private refreshTarget(conn: ConnectionNode, node: ObjectNode): string {
    return node.expandable ? node.key.id : (conn.model.parentOf(node.key.id) ?? ROOT);
  }

  private async createDatabase(conn: ConnectionNode): Promise<void> {
    const created = await databaseDialog(conn.profile);
    if (!created) return;
    await this.refresh(conn, ROOT);
    const database = conn.model.children(ROOT).find((node) => node.kind === "database" && (node.object?.name ?? node.label) === created);
    if (database?.expandable) {
      conn.model.setExpanded(database.key.id, true);
      await this.loadChildren(conn, database.key.id, "first");
    }
    this.host.status(`Created database ${created} on ${conn.profile.name}`);
  }

  // --- Expanding and opening --------------------------------------------------------------------------------------------

  private toggleConnection(conn: ConnectionNode): void {
    if (conn.state === "connected") {
      conn.expanded = !conn.expanded;
      this.render();
    } else if (conn.state !== "connecting") {
      void this.connect(conn);
    }
  }

  private toggleNode(conn: ConnectionNode, node: ObjectNode, shown: boolean): void {
    const id = node.key.id;
    const open = !shown;
    conn.model.setExpanded(id, open);
    if (open && conn.model.needsLoad(id)) void this.loadChildren(conn, id, "first");
    else this.render();
  }

  /** Double-click / Enter: containers open and close; everything else opens its default view. */
  private activate(conn: ConnectionNode, node: ObjectNode, shown: boolean): void {
    if (node.expandable && !node.defaultView) this.toggleNode(conn, node, shown);
    else void this.host.openObject(conn.profile, node, node.defaultView ?? "data");
  }

  // --- Actions ----------------------------------------------------------------------------------------------------------

  /** Runs a node action (or a connection action when `node` is null); a failure goes to the error panel. */
  private async runAction(conn: ConnectionNode, node: ObjectNode | null, action: NodeAction): Promise<void> {
    try {
      await this.dispatch(conn, node, action);
    } catch (error) {
      showError(error, `${action.label.replace(/…$/, "")} · ${node ? plainName(node) : conn.profile.name}`);
    }
  }

  private async dispatch(conn: ConnectionNode, node: ObjectNode | null, action: NodeAction): Promise<void> {
    const target = action.target;
    const database = node?.object?.database ?? conn.profile.database;
    switch (target.kind) {
      case "view":
        await this.host.openObject(conn.profile, node, target.role);
        return;
      case "query": {
        const engine = engineOf(conn.profile);
        const dialect = engine.sqlDialect ? dialectOf(engine.sqlDialect) : null;
        this.host.openQuery(conn.profile.id, database, fillTemplate(target.template, node?.object ?? null, dialect, database));
        return;
      }
      case "capability":
        await this.invokeCapability(conn, node, action, target.name, target.args);
        return;
      case "builtin":
        await this.runBuiltin(conn, node, target.name);
        return;
    }
  }

  /** `invoke_capability` through the gateway: a confirmation it asks for goes to the safety dialog, errors to the panel. */
  private async invokeCapability(conn: ConnectionNode, node: ObjectNode | null, action: NodeAction, name: string, args: unknown): Promise<void> {
    const object = node?.object ?? null;
    const payload = args === null || args === undefined ? { object } : isPlainObject(args) && object && !("object" in args) ? { ...args, object } : args;
    const where = node ? plainName(node) : conn.profile.name;
    try {
      const outcome = await withConfirmation(async (confirmation) => ({
        value: await api.invokeCapability(conn.profile.id, name, payload, confirmation),
      }));
      if (outcome === null) return;
      this.host.status(`${action.label} · ${where}: done`);
      toast(`${action.label}: done`, "success");
      if (conn.state === "connected") void this.refresh(conn, node ? (conn.model.parentOf(node.key.id) ?? ROOT) : ROOT);
    } catch (error) {
      showError(error, `${action.label} · ${where}`);
    }
  }

  private async runBuiltin(conn: ConnectionNode, node: ObjectNode | null, name: BuiltinAction): Promise<void> {
    const profile = conn.profile;
    const database = node?.object?.database ?? profile.database;
    switch (name) {
      case "refresh":
        await this.refresh(conn, node ? this.refreshTarget(conn, node) : ROOT);
        return;
      case "copyName": {
        const text = node ? plainName(node) : profile.name;
        await copyText(text);
        this.host.status(`Copied ${text}`);
        return;
      }
      case "newQuery":
        this.host.openQuery(profile.id, database);
        return;
      case "diagram":
        if (database) this.host.openDiagram(profile, database);
        else toast("Select a database in the tree, then open its diagram");
        return;
      case "createDatabase":
        await this.createDatabase(conn);
        return;
      case "design":
        await this.host.openObject(profile, node, "design");
        return;
      case "import":
      case "export": {
        const view = (await this.host.openObject(profile, node, "data")) as
          (TabContent & { startTransfer?: (kind: "import" | "export") => Promise<void> }) | null;
        if (view && typeof view.startTransfer === "function") await view.startTransfer(name);
        return;
      }
    }
  }

  private connectionMenu(conn: ConnectionNode): MenuItem[] {
    const connected = conn.state === "connected";
    const id = conn.profile.id;
    const engine = engineOf(conn.profile);
    const canCreate = connected ? capabilityOf(id, "createDatabase") : hasCapability(engine, "createDatabase");
    const items: MenuItem[] = [
      connected
        ? { label: "Disconnect", icon: "x", run: () => void this.disconnect(conn) }
        : { label: "Connect", icon: "plug", disabled: conn.state === "connecting", run: () => void this.connect(conn) },
      { label: "New query", icon: "query", disabled: !connected, run: () => this.host.openQuery(id, conn.profile.database) },
      { label: "Refresh", icon: "refresh", disabled: !connected, run: () => void this.refresh(conn, ROOT) },
    ];
    if (canCreate) items.push({ label: "New database…", icon: "database", disabled: !connected, run: () => void this.createDatabase(conn) });
    const extra = connected
      ? (connectInfo(id)?.connectionActions ?? []).filter((action) => action.target.kind !== "builtin" || !CONNECTION_BUILTINS.has(action.target.name))
      : [];
    if (extra.length) items.push("separator", ...this.actionItems(conn, null, extra, false));
    items.push(
      "separator",
      { label: "Edit connection…", icon: "edit", run: () => this.host.editConnection(conn.profile) },
      { label: "Delete connection", icon: "trash", danger: true, run: () => this.host.deleteConnection(conn.profile) },
    );
    return items;
  }

  private nodeMenu(conn: ConnectionNode, node: ObjectNode, shown: boolean): MenuItem[] {
    const items: MenuItem[] = [];
    if (!node.expandable || node.defaultView) {
      const role = node.defaultView ?? "data";
      items.push({ label: openLabel(node.kind, role), icon: nodeIcon(node.icon, "table"), run: () => void this.host.openObject(conn.profile, node, role) });
    } else {
      items.push({ label: shown ? "Collapse" : "Expand", icon: "chevron", run: () => this.toggleNode(conn, node, shown) });
    }
    const hasNewQuery = node.actions.some((action) => action.target.kind === "builtin" && action.target.name === "newQuery");
    if (!hasNewQuery) {
      items.push({ label: "New query", icon: "query", run: () => this.host.openQuery(conn.profile.id, node.object?.database ?? conn.profile.database) });
    }
    items.push(...this.actionItems(conn, node, node.actions, true));
    return items;
  }

  /** Menu entries for engine actions; `separatorBefore` draws a line (never at the very top). */
  private actionItems(conn: ConnectionNode, node: ObjectNode | null, actions: NodeAction[], afterOthers: boolean): MenuItem[] {
    const items: MenuItem[] = [];
    for (const action of actions) {
      if (action.separatorBefore && (afterOthers || items.length > 0)) items.push("separator");
      items.push({
        label: action.label,
        icon: nodeIcon(action.icon, actionIcon(action)),
        danger: action.danger,
        run: () => void this.runAction(conn, node, action),
      });
    }
    return items;
  }

  // --- Rendering --------------------------------------------------------------------------------------------------------

  private render(): void {
    this.rows = new Map();
    const query = this.search.value.trim();
    const elements = this.nodes.flatMap((conn) => this.connectionRows(conn, query));
    this.body.replaceChildren(...(elements.length ? elements : [h("div", { class: "empty-hint" }, "No connections yet")]));
    this.host.stateChanged?.();
  }

  private connectionRows(conn: ConnectionNode, query: string): HTMLElement[] {
    const id = conn.profile.id;
    const connected = conn.state === "connected";
    const engine = engineOf(conn.profile);
    const connectionKey = rowKey(id);
    const row = this.row({
      key: connectionKey,
      context: { connectionId: id, database: null },
      depth: 0,
      iconName: nodeIcon(engine.icon, "database"),
      label: conn.profile.name,
      meta: conn.state === "connecting" ? "connecting…" : undefined,
      tag: engine.label,
      title: conn.message ?? `${engine.label} · ${endpoint(conn.profile, engine)}`,
      className: `connection driver-${engine.id} state-${conn.state}`,
      status: conn.state,
      expanded: connected && conn.expanded,
      onToggle: () => this.toggleConnection(conn),
      onOpen: () => this.toggleConnection(conn),
      menu: () => this.connectionMenu(conn),
    });
    if (!connected || !conn.expanded) return [row];
    const children = conn.model.rows(query).map((visible) => this.visibleRow(conn, visible));
    return [row, ...children];
  }

  private visibleRow(conn: ConnectionNode, visible: VisibleRow): HTMLElement {
    const id = conn.profile.id;
    const depth = visible.depth + 1;
    const parentKey = visible.parentId === ROOT ? rowKey(id) : rowKey(id, visible.parentId);
    const parentDatabase = visible.parentId === ROOT ? null : (conn.model.node(visible.parentId)?.object?.database ?? null);
    switch (visible.type) {
      case "node":
        return this.nodeRow(conn, visible.node, depth, visible.expanded, visible.count, parentKey);
      case "more": {
        const progress = visible.total !== null ? ` (${visible.loaded} of ${visible.total})` : "";
        return this.row({
          key: `m|${id}|${visible.parentId}`,
          context: { connectionId: id, database: parentDatabase },
          depth,
          iconName: visible.loading ? null : "more",
          label: visible.loading ? "Loading…" : `Load more${progress}`,
          className: "load-more",
          parentKey,
          onOpen: () => void this.loadMore(conn, visible.parentId),
          singleClick: true,
        });
      }
      case "error":
        return this.row({
          key: `e|${id}|${visible.parentId}`,
          context: null,
          depth,
          iconName: "x",
          label: "Could not load; double-click to retry",
          title: visible.message,
          className: "placeholder error-row",
          parentKey,
          onOpen: () => void this.loadChildren(conn, visible.parentId, "first"),
        });
      default:
        return placeholder(depth, visible.type === "loading" ? "Loading…" : emptyText(conn, visible.parentId));
    }
  }

  private nodeRow(conn: ConnectionNode, node: ObjectNode, depth: number, shown: boolean, count: string | null, parentKey: string): HTMLElement {
    const id = conn.profile.id;
    const diagram = node.actions.find((action) => action.target.kind === "builtin" && action.target.name === "diagram");
    const system = node.detail === "system";
    return this.row({
      key: rowKey(id, node.key.id),
      context: { connectionId: id, database: node.object?.database ?? null },
      depth,
      iconName: nodeIcon(node.icon, node.expandable ? "layers" : "table"),
      label: node.label,
      detail: node.detail,
      badges: node.badges,
      meta: count,
      title: [node.label, node.detail].filter(Boolean).join(" · "),
      className: system ? "system" : undefined,
      expanded: node.expandable ? shown : undefined,
      onToggle: node.expandable ? () => this.toggleNode(conn, node, shown) : undefined,
      onOpen: () => this.activate(conn, node, shown),
      menu: () => this.nodeMenu(conn, node, shown),
      parentKey,
      action: diagram ? { icon: "diagram", title: diagram.label, run: () => void this.runAction(conn, node, diagram) } : undefined,
    });
  }

  private row(spec: RowSpec): HTMLElement {
    this.rows.set(spec.key, spec);
    const chevron = h("span", { class: `chevron${spec.expanded ? " open" : ""}` }, spec.onToggle ? icon("chevron") : null);
    const toggle = spec.onToggle;
    if (toggle) {
      chevron.addEventListener("click", (event) => {
        event.stopPropagation();
        this.select(spec.key);
        toggle();
      });
      chevron.addEventListener("dblclick", (event) => event.stopPropagation());
    }
    const classes = ["tree-row", spec.className ?? "", spec.key === this.selectedKey ? "selected" : ""];
    const element = h(
      "div",
      {
        class: classes.filter(Boolean).join(" "),
        "data-key": spec.key,
        role: "treeitem",
        "aria-expanded": spec.expanded === undefined ? null : String(spec.expanded),
        "aria-selected": String(spec.key === this.selectedKey),
        title: spec.title ?? null,
        style: `padding-left:${6 + spec.depth * 16}px`,
      },
      chevron,
      spec.iconName === null
        ? h("span", { class: "icon" })
        : spec.status
          ? h("span", { class: "tree-icon-wrap" }, icon(spec.iconName), h("span", { class: `conn-dot conn-${spec.status}`, title: STATUS_LABELS[spec.status] }))
          : icon(spec.iconName),
      h("span", { class: "tree-label" }, spec.label),
      spec.detail ? h("span", { class: "tree-detail" }, spec.detail) : null,
      ...(spec.badges ?? []).map((badge) => h("span", { class: `tree-badge tone-${badge.tone}` }, badge.text)),
      spec.tag ? h("span", { class: "tree-tag" }, spec.tag) : null,
      spec.meta ? h("span", { class: "tree-meta" }, spec.meta) : null,
    );
    if (spec.action) {
      const action = spec.action;
      const button = h("button", { class: "tree-action", type: "button", title: action.title, tabindex: "-1" }, icon(action.icon));
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        action.run();
      });
      button.addEventListener("dblclick", (event) => event.stopPropagation());
      element.append(button);
    }
    element.addEventListener("click", () => {
      this.select(spec.key);
      this.body.focus({ preventScroll: true });
      if (spec.singleClick) spec.onOpen();
    });
    if (!spec.singleClick) element.addEventListener("dblclick", spec.onOpen);
    const menu = spec.menu;
    element.addEventListener("contextmenu", (event) => {
      this.select(spec.key);
      if (menu) showContextMenu(event, menu());
    });
    return element;
  }

  private select(key: string): void {
    this.selectedKey = key;
    for (const row of this.body.querySelectorAll<HTMLElement>(".tree-row")) {
      const selected = row.dataset.key === key;
      row.classList.toggle("selected", selected);
      if (row.dataset.key) row.setAttribute("aria-selected", String(selected));
    }
  }

  // --- Keyboard ---------------------------------------------------------------------------------------------------------

  private rowElements(): HTMLElement[] {
    return [...this.body.querySelectorAll<HTMLElement>(".tree-row[data-key]")];
  }

  private moveSelection(step: number | "first" | "last"): void {
    const elements = this.rowElements();
    if (elements.length === 0) return;
    const current = elements.findIndex((element) => element.dataset.key === this.selectedKey);
    const index =
      step === "first"
        ? 0
        : step === "last" || (current < 0 && typeof step === "number" && step < 0)
          ? elements.length - 1
          : current < 0
            ? 0
            : Math.max(0, Math.min(elements.length - 1, current + step));
    const target = elements[index];
    if (!target.dataset.key) return;
    this.select(target.dataset.key);
    target.scrollIntoView({ block: "nearest" });
  }

  private selectKey(key: string): void {
    const element = this.rowElements().find((item) => item.dataset.key === key);
    if (!element) return;
    this.select(key);
    element.scrollIntoView({ block: "nearest" });
  }

  private handleKey(event: KeyboardEvent): void {
    if (event.target !== this.body || event.altKey || event.ctrlKey || event.metaKey) return;
    const spec = this.selectedKey ? this.rows.get(this.selectedKey) : undefined;
    let handled = true;
    switch (event.key) {
      case "ArrowDown":
        this.moveSelection(1);
        break;
      case "ArrowUp":
        this.moveSelection(-1);
        break;
      case "Home":
        this.moveSelection("first");
        break;
      case "End":
        this.moveSelection("last");
        break;
      case "ArrowRight":
        if (spec?.onToggle && !spec.expanded) spec.onToggle();
        else if (spec?.expanded) this.moveSelection(1);
        break;
      case "ArrowLeft":
        if (spec?.onToggle && spec.expanded) spec.onToggle();
        else if (spec?.parentKey) this.selectKey(spec.parentKey);
        break;
      case "Enter":
        spec?.onOpen();
        break;
      case "ContextMenu":
      case "F10":
        if (event.key === "F10" && !event.shiftKey) {
          handled = false;
          break;
        }
        if (spec?.menu) this.menuAtRow(spec);
        break;
      default:
        handled = false;
    }
    if (handled) event.preventDefault();
  }

  private menuAtRow(spec: RowSpec): void {
    const element = this.rowElements().find((item) => item.dataset.key === spec.key);
    if (!element || !spec.menu) return;
    const rect = element.getBoundingClientRect();
    const event = new MouseEvent("contextmenu", { clientX: rect.left + 24, clientY: rect.bottom, bubbles: false, cancelable: true });
    showContextMenu(event, spec.menu());
  }
}

/** Row keys: `c|<connection>` for a connection, `n|<connection>|<node id>` for a node (stable across refreshes). */
function rowKey(connectionId: string, nodeId?: string): string {
  return nodeId === undefined ? `c|${connectionId}` : `n|${connectionId}|${nodeId}`;
}

function parseKey(key: string): { connectionId: string; nodeId: string | null } | null {
  const [type, connectionId, ...rest] = key.split("|");
  if (!connectionId) return null;
  return { connectionId, nodeId: type === "n" ? rest.join("|") : null };
}

function placeholder(depth: number, text: string): HTMLElement {
  return h("div", { class: "tree-row placeholder", style: `padding-left:${6 + depth * 16 + 18}px` }, text);
}

function emptyText(conn: ConnectionNode, parentId: string): string {
  if (parentId === ROOT) return "No databases";
  const parent = conn.model.node(parentId);
  return parent?.kind === "database" || parent?.kind === "schema" ? `No ${engineOf(conn.profile).objectsLabel}` : "Empty";
}

function endpoint(profile: ConnectionProfile, engine: EngineInfo): string {
  if (engine.connection.usesFile && profile.file) return profile.file;
  if (profile.url) return profile.url;
  return `${profile.user ? `${profile.user}@` : ""}${profile.host}:${profile.port}`;
}

/** "Open table", "Open materialized view", "Open overview". */
function openLabel(kind: string, role: ViewRole): string {
  if (role !== "data") return `Open ${role}`;
  const words = kind
    .replace(/^.*\./, "")
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .toLowerCase();
  return `Open ${words}`;
}

function actionIcon(action: NodeAction): IconName {
  const target = action.target;
  switch (target.kind) {
    case "view":
      return target.role === "design" ? "design" : target.role === "data" ? "table" : "view";
    case "query":
      return action.danger ? "trash" : "query";
    case "capability":
      return action.danger ? "trash" : "bolt";
    case "builtin":
      return BUILTIN_ICONS[target.name];
  }
}

const BUILTIN_ICONS: Record<BuiltinAction, IconName> = {
  refresh: "refresh",
  copyName: "copy",
  newQuery: "query",
  diagram: "diagram",
  createDatabase: "database",
  import: "upload",
  export: "download",
  design: "design",
};

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
