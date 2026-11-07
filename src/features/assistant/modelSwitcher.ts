/**
 * The model pill below the chat composer and its popover: every LLM
 * connection with its models, the active one checked. Model lists are
 * fetched lazily per connection with its saved key and cached.
 */
import { api, errorText } from "../../core/api";
import { connectionSettings, providerPreset } from "./aiProviders";
import { toast } from "../../core/dialogs";
import { h, icon, iconButton } from "../../core/dom";
import type { AiActive, AiConnection, AppConfig } from "../../core/types";

export interface ModelSwitcherHost {
  /** Opens the provider dialog for `connection`, or an empty one. */
  edit(connection: AiConnection | null): void;
  configChanged(config: AppConfig): void;
  /** Where focus goes back after the popover closes. */
  focusComposer(): void;
}

type ModelList = { state: "loading" } | { state: "ok"; models: string[] } | { state: "error"; message: string };

/** Models shown per connection before the filter is needed. */
const MAX_UNFILTERED = 40;

export class ModelSwitcher {
  readonly element: HTMLElement;
  private readonly host: ModelSwitcherHost;
  private readonly pill: HTMLButtonElement;
  private readonly filter = h("input", { class: "model-filter", type: "text", placeholder: "Filter models…", spellcheck: "false" });
  private readonly list = h("div", { class: "model-popover-list" });
  private readonly popover: HTMLElement;
  private connections: AiConnection[] = [];
  private active: AiActive | null = null;
  private readonly lists = new Map<string, ModelList>();
  /** Provider + endpoint each cached list was loaded for. */
  private readonly loadedFor = new Map<string, string>();

  constructor(host: ModelSwitcherHost) {
    this.host = host;
    this.pill = h("button", { class: "model-pill", type: "button", "aria-haspopup": "listbox", "aria-expanded": "false" });
    this.pill.addEventListener("click", () => (this.isOpen() ? this.close() : this.open()));
    this.filter.addEventListener("input", () => this.renderList());
    this.popover = h("div", { class: "model-popover", role: "listbox" }, h("div", { class: "model-filter-row" }, icon("search"), this.filter), this.list);
    this.popover.hidden = true;
    this.element = h("div", { class: "model-switcher" }, this.pill, this.popover);
  }

  set(connections: AiConnection[], active: AiActive | null): void {
    this.connections = connections;
    this.active = active;
    for (const id of [...this.lists.keys()]) {
      const connection = connections.find((item) => item.id === id);
      if (!connection || this.loadedFor.get(id) !== signature(connection)) this.forget(id);
    }
    this.renderPill();
    if (this.isOpen()) this.renderList();
  }

  close(): void {
    if (!this.isOpen()) return;
    this.popover.hidden = true;
    this.pill.setAttribute("aria-expanded", "false");
    document.removeEventListener("pointerdown", this.onPointerDown, true);
    window.removeEventListener("keydown", this.onKeyDown, true);
  }

  private isOpen(): boolean {
    return !this.popover.hidden;
  }

  private open(): void {
    this.popover.hidden = false;
    this.pill.setAttribute("aria-expanded", "true");
    this.filter.value = "";
    for (const connection of this.connections) {
      const list = this.lists.get(connection.id);
      if (!list || list.state === "error") void this.load(connection);
    }
    this.renderList();
    this.list.querySelector(".model-option.active")?.scrollIntoView({ block: "nearest" });
    this.filter.focus();
    document.addEventListener("pointerdown", this.onPointerDown, true);
    window.addEventListener("keydown", this.onKeyDown, true);
  }

  private readonly onPointerDown = (event: PointerEvent) => {
    if (!this.element.contains(event.target as Node)) this.close();
  };

  private readonly onKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopImmediatePropagation();
    this.close();
    this.host.focusComposer();
  };

  private forget(id: string): void {
    this.lists.delete(id);
    this.loadedFor.delete(id);
  }

  private async load(connection: AiConnection): Promise<void> {
    const key = signature(connection);
    this.lists.set(connection.id, { state: "loading" });
    this.loadedFor.set(connection.id, key);
    let result: ModelList;
    try {
      result = { state: "ok", models: await api.listAiModels(connectionSettings(connection), null, connection.id) };
    } catch (error) {
      result = { state: "error", message: errorText(error) };
    }
    if (this.loadedFor.get(connection.id) !== key) return;
    this.lists.set(connection.id, result);
    if (this.isOpen()) this.renderList();
  }

  private renderPill(): void {
    const connection = this.connections.find((item) => item.id === this.active?.connectionId);
    const preset = providerPreset(connection?.provider);
    const label = connection && this.active ? `${connection.name} · ${this.active.model}` : "Choose a model";
    this.pill.title = connection ? `${preset.label}: ${label}. Click to switch model or provider.` : label;
    this.pill.replaceChildren(icon(preset.icon, "provider-icon"), h("span", { class: "model-pill-text" }, label), icon("chevronDown", "model-pill-chevron"));
  }

  private renderList(): void {
    const query = this.filter.value.trim().toLowerCase();
    const groups = this.connections.map((connection) => this.group(connection, query));
    const add = h("button", { class: "model-add", type: "button" }, icon("plus"), "Add provider…");
    add.addEventListener("click", () => this.edit(null));
    const scroll = this.list.scrollTop;
    this.list.replaceChildren(...groups, add);
    this.list.scrollTop = scroll;
  }

  private group(connection: AiConnection, query: string): HTMLElement {
    const preset = providerPreset(connection.provider);
    const editButton = iconButton("edit", `Edit ${connection.name}`, () => this.edit(connection));
    const header = h(
      "div",
      { class: "model-group-header" },
      icon(preset.icon, "provider-icon"),
      h("span", { class: "model-group-name" }, connection.name),
      h("span", { class: "model-group-provider" }, preset.label),
      editButton,
    );
    const list = this.lists.get(connection.id);
    const all = this.modelsOf(connection, list);
    const matching = query ? all.filter((model) => model.toLowerCase().includes(query)) : all;
    const shown = query ? matching : matching.slice(0, MAX_UNFILTERED);
    const rows: HTMLElement[] = shown.map((model) => this.option(connection, model));
    if (shown.length < matching.length) rows.push(h("div", { class: "model-note" }, `${matching.length - shown.length} more; type to filter`));
    if (query && !matching.length) rows.push(h("div", { class: "model-note" }, "No matching model"));
    if (list?.state === "loading") rows.push(h("div", { class: "model-note" }, "Loading…"));
    if (list?.state === "error") rows.push(h("div", { class: "model-note error", title: list.message }, `Could not list models: ${list.message}`));
    return h("div", { class: "model-group" }, header, ...rows);
  }

  /** Default model first, then the active one if it is not listed, then the provider's list. */
  private modelsOf(connection: AiConnection, list: ModelList | undefined): string[] {
    const active = this.active?.connectionId === connection.id ? this.active.model : null;
    const loaded = list?.state === "ok" ? list.models : [];
    return [...new Set([connection.defaultModel, ...(active ? [active] : []), ...loaded])];
  }

  private option(connection: AiConnection, model: string): HTMLElement {
    const selected = this.active?.connectionId === connection.id && this.active.model === model;
    const button = h(
      "button",
      { class: `model-option${selected ? " active" : ""}`, type: "button", role: "option", "aria-selected": String(selected) },
      icon("check", "model-check"),
      h("span", { class: "model-option-name" }, model),
      model === connection.defaultModel ? h("span", { class: "model-default" }, "default") : null,
    );
    button.addEventListener("click", () => void this.pick(connection, model));
    return button;
  }

  private async pick(connection: AiConnection, model: string): Promise<void> {
    this.close();
    this.host.focusComposer();
    try {
      this.host.configChanged(await api.setAiActive(connection.id, model));
    } catch (error) {
      toast(errorText(error));
    }
  }

  private edit(connection: AiConnection | null): void {
    this.close();
    if (connection) this.forget(connection.id);
    this.host.edit(connection);
  }
}

function signature(connection: AiConnection): string {
  return `${connection.provider}|${connection.baseUrl}`;
}
