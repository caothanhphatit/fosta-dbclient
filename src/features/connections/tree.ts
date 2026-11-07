import { api, errorText, PASSWORD_REQUIRED } from "../../core/api";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { promptPassword, toast } from "../../core/dialogs";
import { copyText, formatCompact, h, icon, type IconName } from "../../core/dom";
import { describeServer, driverOf } from "../../core/drivers";
import { databaseDialog } from "./databaseDialog";
import type { AppConfig, ConnectionProfile, TableInfo, TableTarget } from "../../core/types";

type ConnectionState = "idle" | "connecting" | "connected" | "error";

const STATUS_LABELS: Record<ConnectionState, string> = {
  idle: "Disconnected",
  connecting: "Connecting…",
  connected: "Connected",
  error: "Connection failed",
};

interface DatabaseNode {
  name: string;
  expanded: boolean;
  loading: boolean;
  tables: TableInfo[] | null;
}

interface ConnectionNode {
  profile: ConnectionProfile;
  state: ConnectionState;
  expanded: boolean;
  databases: DatabaseNode[] | null;
  message: string | null;
  /** Version string the server reported, once connected. */
  version: string | null;
}

export interface TreeContext {
  connectionId: string;
  database: string | null;
}

interface RowSpec {
  key: string;
  context: TreeContext;
  depth: number;
  iconName: IconName;
  label: string;
  meta?: string;
  tag?: string;
  title?: string;
  className?: string;
  expanded?: boolean;
  onToggle?: () => void;
  onOpen: () => void;
  menu: () => MenuItem[];
  /** Connection status dot drawn on the icon. */
  status?: ConnectionState;
  /** Small button shown on hover at the end of the row. */
  action?: { icon: IconName; title: string; run: () => void };
}

export interface TreeHost {
  openTable(target: TableTarget): void;
  openDesign(target: TableTarget): void;
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

const SYSTEM_DATABASES = new Set(["information_schema", "mysql", "performance_schema", "sys"]);

export class ConnectionTree {
  readonly element: HTMLElement;
  private readonly host: TreeHost;
  private readonly body = h("div", { class: "tree-body" });
  private readonly search = h("input", { class: "input", placeholder: "Search tables", spellcheck: "false" });
  private nodes: ConnectionNode[] = [];
  /** Nodes of every workspace seen so far, so switching back keeps their open state. */
  private readonly cache = new Map<string, ConnectionNode>();
  private contexts = new Map<string, TreeContext>();
  private selectedKey: string | null = null;

  constructor(host: TreeHost) {
    this.host = host;
    this.search.addEventListener("input", () => this.render());
    this.element = h("div", { class: "tree" }, h("div", { class: "search-box" }, icon("search"), this.search), this.body);
  }

  setConnections(profiles: ConnectionProfile[]): void {
    this.nodes = profiles.map((profile): ConnectionNode => {
      const node = this.cache.get(profile.id) ?? { profile, state: "idle", expanded: false, databases: null, message: null, version: null };
      node.profile = profile;
      this.cache.set(profile.id, node);
      return node;
    });
    this.render();
  }

  activeContext(): TreeContext | null {
    const selected = this.selectedKey ? this.contexts.get(this.selectedKey) : undefined;
    if (selected && this.nodeOf(selected.connectionId)?.state === "connected") return selected;
    const connected = this.nodes.find((node) => node.state === "connected");
    return connected ? { connectionId: connected.profile.id, database: connected.profile.database } : null;
  }

  /** Ids of every connection (in any workspace) that is currently connected. */
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

  refreshSelected(): void {
    const context = this.activeContext();
    const node = context && this.nodeOf(context.connectionId);
    if (!node) return;
    const database = node.databases?.find((item) => item.name === context.database);
    if (database) void this.loadTables(node, database);
    else void this.loadDatabases(node);
  }

  private nodeOf(connectionId: string): ConnectionNode | undefined {
    return this.nodes.find((node) => node.profile.id === connectionId);
  }

  private async connect(node: ConnectionNode, password: string | null = null): Promise<void> {
    node.state = "connecting";
    node.message = null;
    this.render();
    try {
      const version = await api.connect(node.profile.id, password);
      node.state = "connected";
      node.version = version;
      node.expanded = true;
      this.host.status(`Connected to ${node.profile.name} · ${describeServer(node.profile, version)}`);
      await this.loadDatabases(node);
    } catch (error) {
      const message = errorText(error);
      node.state = message === PASSWORD_REQUIRED ? "idle" : "error";
      node.message = message === PASSWORD_REQUIRED ? null : message;
      this.render();
      if (message === PASSWORD_REQUIRED) await this.askPasswordAndConnect(node);
      else toast(`${node.profile.name}: ${message}`, "error");
    }
  }

  private async askPasswordAndConnect(node: ConnectionNode): Promise<void> {
    const answer = await promptPassword(node.profile.name);
    if (!answer) return;
    if (answer.remember) {
      this.host.configChanged(await api.saveConnection({ ...node.profile, savePassword: true }, answer.password));
    }
    await this.connect(node, answer.password);
  }

  private async disconnect(node: ConnectionNode): Promise<void> {
    try {
      await api.disconnect(node.profile.id);
    } catch (error) {
      toast(errorText(error), "error");
    }
    node.state = "idle";
    node.expanded = false;
    node.databases = null;
    this.host.connectionClosed(node.profile.id);
    this.host.status(`Disconnected from ${node.profile.name}`);
    this.render();
  }

  private async loadDatabases(node: ConnectionNode): Promise<void> {
    try {
      const names = await api.listDatabases(node.profile.id);
      const previous = new Map((node.databases ?? []).map((database) => [database.name, database]));
      node.databases = names.map((name): DatabaseNode => previous.get(name) ?? { name, expanded: false, loading: false, tables: null });
      for (const database of node.databases.filter((item) => item.expanded)) void this.loadTables(node, database);
    } catch (error) {
      toast(errorText(error), "error");
    }
    this.render();
  }

  private async createDatabase(node: ConnectionNode): Promise<void> {
    const created = await databaseDialog(node.profile);
    if (!created) return;
    await this.loadDatabases(node);
    const database = node.databases?.find((item) => item.name === created);
    if (database) {
      database.expanded = true;
      await this.loadTables(node, database);
    }
    this.host.status(`Created database ${created} on ${node.profile.name}`);
  }

  private async loadTables(node: ConnectionNode, database: DatabaseNode): Promise<void> {
    database.loading = true;
    this.render();
    try {
      database.tables = await api.listTables(node.profile.id, database.name);
    } catch (error) {
      toast(errorText(error), "error");
    } finally {
      database.loading = false;
      this.render();
    }
  }

  private toggleConnection(node: ConnectionNode): void {
    if (node.state === "connected") {
      node.expanded = !node.expanded;
      this.render();
    } else if (node.state !== "connecting") {
      void this.connect(node);
    }
  }

  private toggleDatabase(node: ConnectionNode, database: DatabaseNode): void {
    database.expanded = !database.expanded;
    if (database.expanded && database.tables === null) void this.loadTables(node, database);
    else this.render();
  }

  private render(): void {
    this.contexts = new Map();
    const query = this.search.value.trim().toLowerCase();
    const rows = this.nodes.flatMap((node) => this.connectionRows(node, query));
    this.body.replaceChildren(...(rows.length ? rows : [h("div", { class: "empty-hint" }, "No connections yet")]));
    this.host.stateChanged?.();
  }

  private connectionRows(node: ConnectionNode, query: string): HTMLElement[] {
    const id = node.profile.id;
    const connected = node.state === "connected";
    const driver = driverOf(node.profile);
    const row = this.row({
      key: `c|${id}`,
      context: { connectionId: id, database: null },
      depth: 0,
      iconName: driver.icon,
      label: node.profile.name,
      meta: node.state === "connecting" ? "connecting…" : undefined,
      tag: driver.label,
      title: node.message ?? `${driver.label} · ${node.profile.user}@${node.profile.host}:${node.profile.port}`,
      className: `connection driver-${driver.id} state-${node.state}`,
      status: node.state,
      expanded: connected && node.expanded,
      onToggle: () => this.toggleConnection(node),
      onOpen: () => this.toggleConnection(node),
      menu: () => this.connectionMenu(node),
    });
    if (!connected || !node.expanded || !node.databases) return [row];
    return [row, ...node.databases.flatMap((database) => this.databaseRows(node, database, query))];
  }

  private databaseRows(node: ConnectionNode, database: DatabaseNode, query: string): HTMLElement[] {
    const tables = (database.tables ?? []).filter((table) => !query || table.name.toLowerCase().includes(query));
    if (query && database.tables && tables.length === 0) return [];
    const connectionId = node.profile.id;
    const row = this.row({
      key: `d|${connectionId}|${database.name}`,
      context: { connectionId, database: database.name },
      depth: 1,
      iconName: "database",
      label: database.name,
      meta: database.tables ? String(database.tables.length) : undefined,
      className: SYSTEM_DATABASES.has(database.name) ? "system" : undefined,
      expanded: database.expanded,
      onToggle: () => this.toggleDatabase(node, database),
      onOpen: () => this.toggleDatabase(node, database),
      menu: () => this.databaseMenu(node, database),
      action: { icon: "diagram", title: "ER diagram", run: () => this.host.openDiagram(node.profile, database.name) },
    });
    const showChildren = database.expanded || (query !== "" && database.tables !== null);
    if (!showChildren) return [row];
    if (database.loading && !database.tables) return [row, placeholder(2, "Loading…")];
    if (tables.length === 0) return [row, placeholder(2, `No ${driverOf(node.profile).objectsLabel}`)];
    return [row, ...tables.map((table) => this.tableRow(connectionId, database.name, table))];
  }

  private tableRow(connectionId: string, database: string, table: TableInfo): HTMLElement {
    const driver = this.nodeOf(connectionId)?.profile.driver ?? "mariadb";
    const target: TableTarget = { connectionId, driver, database, table: table.name, kind: table.kind };
    return this.row({
      key: `t|${connectionId}|${database}|${table.name}`,
      context: { connectionId, database },
      depth: 2,
      iconName: table.kind === "view" ? "view" : "table",
      label: table.name,
      meta: table.rows === null ? undefined : `~${formatCompact(table.rows)}`,
      title: table.comment || undefined,
      onOpen: () => this.host.openTable(target),
      menu: () => this.tableMenu(target),
    });
  }

  private row(spec: RowSpec): HTMLElement {
    this.contexts.set(spec.key, spec.context);
    const chevron = h("span", { class: `chevron${spec.expanded ? " open" : ""}` }, spec.onToggle ? icon("chevron") : null);
    const toggle = spec.onToggle;
    if (toggle) {
      chevron.addEventListener("click", (event) => {
        event.stopPropagation();
        toggle();
      });
      chevron.addEventListener("dblclick", (event) => event.stopPropagation());
    }
    const classes = ["tree-row", spec.className ?? "", spec.key === this.selectedKey ? "selected" : ""];
    const element = h(
      "div",
      { class: classes.filter(Boolean).join(" "), "data-key": spec.key, title: spec.title ?? null, style: `padding-left:${6 + spec.depth * 16}px` },
      chevron,
      spec.status
        ? h("span", { class: "tree-icon-wrap" }, icon(spec.iconName), h("span", { class: `conn-dot conn-${spec.status}`, title: STATUS_LABELS[spec.status] }))
        : icon(spec.iconName),
      h("span", { class: "tree-label" }, spec.label),
      spec.tag ? h("span", { class: "tree-tag" }, spec.tag) : null,
      spec.meta ? h("span", { class: "tree-meta" }, spec.meta) : null,
    );
    if (spec.action) {
      const action = spec.action;
      const button = h("button", { class: "tree-action", type: "button", title: action.title }, icon(action.icon));
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        action.run();
      });
      button.addEventListener("dblclick", (event) => event.stopPropagation());
      element.append(button);
    }
    element.addEventListener("click", () => this.select(spec.key));
    element.addEventListener("dblclick", spec.onOpen);
    element.addEventListener("contextmenu", (event) => {
      this.select(spec.key);
      showContextMenu(event, spec.menu());
    });
    return element;
  }

  private select(key: string): void {
    this.selectedKey = key;
    for (const row of this.body.querySelectorAll<HTMLElement>(".tree-row")) {
      row.classList.toggle("selected", row.dataset.key === key);
    }
  }

  private connectionMenu(node: ConnectionNode): MenuItem[] {
    const connected = node.state === "connected";
    return [
      connected
        ? { label: "Disconnect", icon: "x", run: () => void this.disconnect(node) }
        : { label: "Connect", icon: "plug", disabled: node.state === "connecting", run: () => void this.connect(node) },
      { label: "New query", icon: "query", disabled: !connected, run: () => this.host.openQuery(node.profile.id, node.profile.database) },
      { label: "Refresh", icon: "refresh", disabled: !connected, run: () => void this.loadDatabases(node) },
      { label: "New database…", icon: "database", disabled: !connected, run: () => void this.createDatabase(node) },
      "separator",
      { label: "Edit connection…", icon: "edit", run: () => this.host.editConnection(node.profile) },
      { label: "Delete connection", icon: "trash", danger: true, run: () => this.host.deleteConnection(node.profile) },
    ];
  }

  private databaseMenu(node: ConnectionNode, database: DatabaseNode): MenuItem[] {
    return [
      { label: "New query", icon: "query", run: () => this.host.openQuery(node.profile.id, database.name) },
      { label: "ER diagram", icon: "diagram", run: () => this.host.openDiagram(node.profile, database.name) },
      { label: "Refresh tables", icon: "refresh", run: () => void this.loadTables(node, database) },
      "separator",
      { label: "Copy name", icon: "copy", run: () => void copyText(database.name) },
    ];
  }

  private tableMenu(target: TableTarget): MenuItem[] {
    const select = `SELECT *\nFROM \`${target.table}\`\nLIMIT 100;`;
    return [
      { label: target.kind === "view" ? "Open view" : "Open table", icon: target.kind === "view" ? "view" : "table", run: () => this.host.openTable(target) },
      { label: "Design", icon: "design", run: () => this.host.openDesign(target) },
      { label: "Query this table", icon: "query", run: () => this.host.openQuery(target.connectionId, target.database, select) },
      "separator",
      { label: "Copy name", icon: "copy", run: () => void copyText(target.table) },
    ];
  }
}

function placeholder(depth: number, text: string): HTMLElement {
  return h("div", { class: "tree-row placeholder", style: `padding-left:${6 + depth * 16 + 18}px` }, text);
}
