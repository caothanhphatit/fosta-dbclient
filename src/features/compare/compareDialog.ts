/**
 * "Compare schema with…": picks the source and the target (workspace, connection, database, schema), from any workspace.
 * Picking a connection connects it (through the tree, so a password prompt or a workspace command works as usual) and
 * lists its databases and schemas. Connections of another engine, or of an engine without schema compare, are disabled.
 */
import "./compare.css";
import { api, errorText } from "../../core/api";
import type { CompareOptions } from "../../core/contract";
import { checkbox, field, openModal } from "../../core/dialogs";
import { h, icon, iconButton, iconOrDefault } from "../../core/dom";
import { connectInfo, engineOf, hasCapability } from "../../core/engines";
import type { ConnectionProfile, Workspace } from "../../core/types";
import { connectionSafety, safetyLabel, safetyOf, PRODUCTION_ICON } from "../safety/safety";
import { comparableEngines, defaultOptions, OPTION_TOGGLES, schemaNames, sideName, type CompareRequest, type CompareSide } from "./compareModel";

export interface CompareDialogHost {
  workspaces(): Workspace[];
  /** The workspace shown now (the default for both sides). */
  currentWorkspace(): string;
  /** Connects the connection unless it is connected; true once it is. */
  connect(connectionId: string): Promise<boolean>;
}

/** What the dialog starts with: the source from the tree selection, maybe a whole request ("Change…" on a compare tab). */
export interface CompareDialogStart {
  source: Partial<CompareSide> | null;
  target?: CompareSide | null;
  options?: CompareOptions;
}

const LAST_KEY = "db-studio.compare.last";

function readLast(): { source: CompareSide; target: CompareSide } | null {
  try {
    const value = JSON.parse(localStorage.getItem(LAST_KEY) ?? "null") as { source?: CompareSide; target?: CompareSide } | null;
    return value?.source?.connectionId && value.target?.connectionId ? { source: value.source, target: value.target } : null;
  } catch {
    return null;
  }
}

function writeLast(request: CompareRequest): void {
  try {
    localStorage.setItem(LAST_KEY, JSON.stringify({ source: request.source, target: request.target }));
  } catch {
    // Not remembered: private window or blocked storage.
  }
}

const sameSide = (a: Partial<CompareSide> | null | undefined, b: CompareSide) =>
  Boolean(a) && a?.connectionId === b.connectionId && a?.database === b.database && (a?.schema ?? null) === b.schema;

/** Resolves with what to compare, or null when cancelled. */
export function compareDialog(start: CompareDialogStart, host: CompareDialogHost): Promise<CompareRequest | null> {
  return new Promise((resolve) => {
    let result: CompareRequest | null = null;
    const last = readLast();
    const initialSource = start.source ?? last?.source ?? null;
    // The target: given, else the last pair when its source is this source, else the same connection (another schema).
    const initialTarget: Partial<CompareSide> | null =
      start.target ??
      (last && initialSource && sameSide(initialSource, last.source)
        ? last.target
        : initialSource
          ? { connectionId: initialSource.connectionId, database: initialSource.database }
          : null);
    const options = { ...(start.options ?? defaultOptions()) };
    const pickers: SidePicker[] = [];
    const changed = () => {
      const [first, second] = pickers;
      if (!first || !second) return;
      first.refreshEngines(second.engine());
      second.refreshEngines(first.engine());
    };
    const source = new SidePicker("Source", host, initialSource, null, changed);
    pickers.push(source);
    const target = new SidePicker("Target", host, initialTarget, initialSource?.schema ?? null, changed);
    pickers.push(target);
    changed();
    const swap = iconButton("compare", "Swap source and target", () => {
      const a = source.value();
      const b = target.value();
      source.load(b);
      target.load(a);
    });
    const toggles = OPTION_TOGGLES.map((toggle) => {
      const box = checkbox(toggle.label, Boolean(options[toggle.key]));
      box.element.title = toggle.hint;
      box.input.addEventListener("change", () => Object.assign(options, { [toggle.key]: box.input.checked }));
      return box.element;
    });
    const body = h(
      "div",
      { class: "compare-dialog" },
      h("p", { class: "form-intro" }, "The script makes the target match the source. Nothing runs until you open it in a query tab and run it there."),
      h("div", { class: "compare-dialog-sides" }, source.element, h("div", { class: "compare-dialog-swap" }, swap), target.element),
      h("details", { class: "compare-dialog-options" }, h("summary", {}, "Options"), h("div", { class: "compare-option-toggles" }, ...toggles)),
    );
    openModal(
      "Compare schemas",
      body,
      [
        { label: "Cancel", run: () => undefined },
        {
          label: "Compare",
          primary: true,
          run: () => {
            const a = source.value();
            const b = target.value();
            if (!a || !b) throw new Error("Pick a connection and a database on both sides");
            if (sameSide(a, b)) throw new Error(`Source and target are both ${sideName(a)}: pick another schema or database`);
            const engines = [source.engine(), target.engine()];
            if (engines[0] && engines[1] && !comparableEngines(engines[0], engines[1])) throw new Error("Compare needs two connections of the same engine");
            result = { source: a, target: b, options };
            writeLast(result);
          },
        },
      ],
      { width: 760, onClose: () => resolve(result) },
    );
  });
}

/** One side: workspace, connection, database and schema selects. */
class SidePicker {
  readonly element: HTMLElement;
  private readonly workspace = h("select", { class: "input" });
  private readonly connection = h("select", { class: "input" });
  private readonly database = h("select", { class: "input" });
  private readonly databaseText = h("input", { class: "input mono", type: "text", spellcheck: "false", placeholder: "Database name" });
  private readonly schema = h("select", { class: "input" });
  private readonly schemaField: HTMLElement;
  private readonly status = h("div", { class: "compare-dialog-status" });
  private readonly safety = h("div", { class: "compare-dialog-safety" });
  /** Bumped per connection pick: a slower load of an earlier pick is dropped. */
  private loading = 0;
  private otherEngine: string | null = null;

  constructor(
    role: string,
    private readonly host: CompareDialogHost,
    initial: Partial<CompareSide> | null,
    /** A schema to avoid by default (the target starts on another schema than the source). */
    private avoidSchema: string | null,
    private readonly changed: () => void,
  ) {
    this.schemaField = field("Schema", this.schema);
    this.databaseText.hidden = true;
    this.element = h(
      "div",
      { class: "compare-dialog-side" },
      h("div", { class: "compare-dialog-role" }, role),
      field("Workspace", this.workspace),
      this.safety,
      field("Connection", this.connection),
      field("Database", h("div", { class: "compare-dialog-database" }, this.database, this.databaseText)),
      this.schemaField,
      this.status,
    );
    this.workspace.addEventListener("change", () => {
      this.fillConnections(null);
      void this.connectionPicked(null);
    });
    this.connection.addEventListener("change", () => void this.connectionPicked(null));
    this.database.addEventListener("change", () => void this.loadSchemas(null));
    this.load(initial);
  }

  /** Shows `side` (another workspace's connection switches the workspace select too). */
  load(side: Partial<CompareSide> | null): void {
    const workspaces = this.host.workspaces();
    const owner = side?.connectionId ? workspaces.find((item) => item.connections.some((profile) => profile.id === side.connectionId)) : undefined;
    const selected = owner?.id ?? this.host.currentWorkspace();
    this.workspace.replaceChildren(...workspaces.map((item) => h("option", { value: item.id, selected: item.id === selected }, item.name)));
    this.workspace.value = selected;
    this.fillConnections(side?.connectionId ?? null);
    void this.connectionPicked(side ?? null);
  }

  value(): CompareSide | null {
    const connectionId = this.connection.value;
    const database = (this.databaseText.hidden ? this.database.value : this.databaseText.value).trim();
    if (!connectionId || !database) return null;
    return { connectionId, database, schema: this.schemaField.hidden ? null : this.schema.value || null };
  }

  engine(): string | null {
    const profile = this.profile();
    return profile ? profile.driver : null;
  }

  /** Disables the connections the other side's engine cannot be compared with. */
  refreshEngines(other: string | null): void {
    this.otherEngine = other;
    for (const option of Array.from(this.connection.options)) {
      const profile = this.profiles().find((item) => item.id === option.value);
      if (!profile) continue;
      const reason = this.unavailable(profile);
      option.disabled = reason !== null && option.value !== this.connection.value;
      option.textContent = reason ? `${profile.name} (${reason})` : profile.name;
    }
  }

  private currentWorkspace(): Workspace | undefined {
    return this.host.workspaces().find((item) => item.id === this.workspace.value);
  }

  private profiles(): ConnectionProfile[] {
    return this.currentWorkspace()?.connections ?? [];
  }

  private profile(): ConnectionProfile | undefined {
    return this.profiles().find((item) => item.id === this.connection.value);
  }

  /** Why `profile` cannot be picked here, or null. */
  private unavailable(profile: ConnectionProfile): string | null {
    const engine = engineOf(profile);
    if (!hasCapability(connectInfo(profile.id) ?? engine, "schemaCompare")) return "no schema compare";
    if (this.otherEngine && !comparableEngines(profile.driver, this.otherEngine)) return "different engine";
    return null;
  }

  private fillConnections(preferred: string | null): void {
    const workspace = this.currentWorkspace();
    const profiles = this.profiles();
    const usable = profiles.filter((profile) => this.unavailable(profile) === null);
    const chosen = profiles.find((profile) => profile.id === preferred) ?? usable[0];
    this.connection.replaceChildren(
      ...(profiles.length === 0 ? [h("option", { value: "" }, "No connections in this workspace")] : []),
      ...profiles.map((profile) => h("option", { value: profile.id, selected: profile.id === chosen?.id }, profile.name)),
    );
    this.connection.value = chosen?.id ?? "";
    this.refreshEngines(this.otherEngine);
    if (workspace) {
      const settings = safetyOf(workspace);
      const production = workspace.icon === PRODUCTION_ICON;
      this.safety.replaceChildren(
        icon(iconOrDefault(workspace.icon, "briefcase")),
        h("span", {}, production ? "Production" : workspace.name),
        h("span", { class: `compare-safety preset-${settings.preset}${production ? " production" : ""}` }, safetyLabel(settings)),
      );
    } else this.safety.replaceChildren();
  }

  /** Connects the picked connection and lists its databases (`side`: what to select once they are there). */
  private async connectionPicked(side: Partial<CompareSide> | null): Promise<void> {
    const ticket = ++this.loading;
    const profile = this.profile();
    this.changed();
    this.database.replaceChildren();
    this.schema.replaceChildren();
    this.showDatabaseText(false, "");
    if (!profile) {
      this.status.textContent = "";
      return;
    }
    const rules = connectionSafety(this.host.workspaces(), profile.id);
    if (rules && profile.safety) this.safety.append(h("span", { class: "compare-safety" }, `connection: ${safetyLabel(rules)}`));
    const engine = engineOf(profile);
    this.schemaField.hidden = !hasCapability(connectInfo(profile.id) ?? engine, "schemas");
    this.status.textContent = `Connecting to ${profile.name}…`;
    const connected = await this.host.connect(profile.id).catch(() => false);
    if (ticket !== this.loading) return;
    if (!connected) {
      this.status.textContent = "Not connected: type the database name, or pick the connection again to retry.";
      this.showDatabaseText(true, side?.database ?? profile.database ?? "");
      return;
    }
    try {
      const root = await api.treeChildren(profile.id, { parent: null, cursor: null, limit: null, filter: null });
      if (ticket !== this.loading) return;
      const databases = root.nodes.filter((node) => node.kind === "database");
      if (databases.length === 0) {
        this.showDatabaseText(true, side?.database ?? profile.database ?? "");
        this.status.textContent = "";
        return;
      }
      const wanted = side?.database ?? profile.database ?? null;
      const names = databases.map((node) => node.object?.name ?? node.label);
      const chosen = wanted && names.includes(wanted) ? wanted : names[0];
      this.database.replaceChildren(
        ...databases.map((node) => {
          const name = node.object?.name ?? node.label;
          return h("option", { value: name, selected: name === chosen }, node.detail ? `${name} (${node.detail})` : name);
        }),
      );
      this.database.value = chosen;
      this.status.textContent = "";
      await this.loadSchemas(side?.schema ?? null, ticket);
    } catch (error) {
      if (ticket !== this.loading) return;
      this.status.textContent = `Could not list the databases: ${errorText(error)}`;
      this.showDatabaseText(true, side?.database ?? profile.database ?? "");
    }
  }

  /** Lists the schemas of the picked database (engines with schemas), selecting `wanted` when it is there. */
  private async loadSchemas(wanted: string | null, ticket = ++this.loading): Promise<void> {
    const profile = this.profile();
    if (!profile || this.schemaField.hidden) return;
    const database = this.database.value;
    this.schema.replaceChildren(h("option", { value: "" }, "Loading…"));
    try {
      const root = await api.treeChildren(profile.id, { parent: null, cursor: null, limit: null, filter: null });
      const node = root.nodes.find((item) => item.kind === "database" && (item.object?.name ?? item.label) === database);
      const children = node ? await api.treeChildren(profile.id, { parent: node.key, cursor: null, limit: null, filter: null }) : { nodes: [] };
      if (ticket !== this.loading) return;
      const names = schemaNames(children.nodes);
      const fallback = names.find((name) => name !== this.avoidSchema) ?? names[0];
      const chosen = wanted && names.includes(wanted) ? wanted : (fallback ?? "");
      this.avoidSchema = null;
      this.schema.replaceChildren(...names.map((name) => h("option", { value: name, selected: name === chosen }, name)));
      if (names.length === 0) this.schema.replaceChildren(h("option", { value: "" }, "No schemas"));
      this.schema.value = chosen;
    } catch (error) {
      if (ticket !== this.loading) return;
      this.schema.replaceChildren(h("option", { value: "" }, "Could not list the schemas"));
      this.status.textContent = errorText(error);
    }
  }

  private showDatabaseText(shown: boolean, value: string): void {
    this.databaseText.hidden = !shown;
    this.database.hidden = shown;
    if (shown) this.databaseText.value = value;
  }
}
