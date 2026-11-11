/**
 * The Indexes section of a collection workspace: the collection's indexes (`mongo.indexes`) in a table (name, keys with
 * direction or type chips, property badges, size, usage since, hidden state) with an "Unused" hint for indexes without an
 * operation for a week; per index: copy the mongosh `createIndex` line, hide / unhide, drop (the gateway asks, typed in
 * Guarded workspaces). "Create index" opens a form (key rows with field completion from the tab's sampled paths,
 * options, a live preview of the mongosh command) that runs `mongo.createIndex` through the gateway. Other sections open
 * the form prefilled with `{ createIndex: { keys, options } }` (`receive`).
 *
 * The sort and search are kept with the tab; the assistant gets the indexes in a few lines.
 */
import { showMenuBelow, type MenuItem } from "../../core/contextMenu";
import { toast } from "../../core/dialogs";
import { copyText, formatNumber, h, icon, iconButton } from "../../core/dom";
import { errorText } from "../../core/errors";
import type { SectionContent, SectionContext } from "../../features/objectWorkspace/sectionWorkspace";
import { STORE_FIELD_PATHS } from "./documentsView";
import {
  bytesText,
  defaultIndexName,
  displayKeys,
  EMPTY_FORM,
  formArgs,
  formOfRequest,
  formShell,
  ID_INDEX,
  indexBadges,
  indexesSummary,
  indexShell,
  indexType,
  isUnused,
  isWildcard,
  KEY_KINDS,
  kindLabel,
  readIndexesState,
  UNUSED_DAYS,
  visibleIndexes,
  type FormField,
  type IndexesResult,
  type IndexesSavedState,
  type IndexForm,
  type IndexInfo,
  type IndexSort,
  type KeyKind,
} from "./indexModel";
import { MongoApi } from "./mongoApi";
import { schemaFieldPaths, STORE_SCHEMA, type SchemaResult } from "./schemaModel";
import "./mongodb.css";
import "./indexes.css";

/** The store key of the latest `mongo.indexes` result (Explain reads it to suggest an index). */
export const STORE_INDEXES = "mongo.indexes";

export function indexesSectionFactory(ctx: SectionContext): SectionContent {
  return new IndexesSection(ctx);
}

let datalistIds = 0;

function sinceText(since: string | null): string {
  if (!since) return "";
  const date = new Date(since);
  return Number.isNaN(date.getTime()) ? since : date.toLocaleDateString();
}

class IndexesSection implements SectionContent {
  readonly element: HTMLElement;
  private readonly api: MongoApi;
  private readonly database: string;
  private readonly collection: string;
  private readonly meta = h("span", { class: "ix-meta" });
  private readonly info = h("div", { class: "ix-info", role: "status", hidden: true });
  private readonly searchInput: HTMLInputElement;
  private readonly refreshButton: HTMLButtonElement;
  private readonly createButton: HTMLButtonElement;
  private readonly head = h("div", { class: "ix-row ix-head", role: "row" });
  private readonly rows = h("div", { class: "ix-rows", role: "rowgroup" });
  private readonly formHost = h("section", { class: "ix-form", "aria-label": "Create index", hidden: true });
  private readonly fieldList: HTMLDataListElement;
  private state: IndexesSavedState;
  private result: IndexesResult | null = null;
  private form: IndexForm = structuredClone(EMPTY_FORM);
  private formError: { field: FormField; error: string } | null = null;
  private submitting = false;
  private lastError: string | null = null;
  private loading = false;
  private generation = 0;
  private disposed = false;
  private readonly unsubscribe: (() => void)[] = [];

  constructor(private readonly ctx: SectionContext) {
    const object = ctx.view.object;
    this.database = object?.database ?? "";
    this.collection = object?.name ?? "";
    this.api = new MongoApi(ctx.view.connection, { database: this.database, collection: this.collection });
    this.state = readIndexesState(ctx.savedState);

    this.searchInput = h("input", {
      class: "input ix-search",
      type: "search",
      placeholder: "Find an index or field",
      value: this.state.search,
      "aria-label": "Find an index or field",
    });
    this.searchInput.addEventListener("input", () => {
      this.state.search = this.searchInput.value;
      this.ctx.stateChanged();
      this.renderRows();
    });
    this.refreshButton = h("button", { class: "btn", type: "button", title: "Read the indexes again" }, icon("refresh"), h("span", {}, "Refresh"));
    this.refreshButton.addEventListener("click", () => void this.load(true));
    this.createButton = h("button", { class: "btn primary", type: "button", "aria-expanded": "false" }, icon("plus"), h("span", {}, "Create index"));
    this.createButton.addEventListener("click", () => (this.formHost.hidden ? this.openForm(structuredClone(EMPTY_FORM)) : this.closeForm()));
    const bar = h("div", { class: "ix-bar" }, this.searchInput, h("div", { class: "toolbar-end" }, this.meta, this.refreshButton, this.createButton));
    this.fieldList = h("datalist", { id: `ix-fields-${++datalistIds}` });
    const table = h("div", { class: "ix-table", role: "table", "aria-label": "Indexes" }, this.head, this.rows);
    this.element = h("div", { class: "view ix-section" }, bar, this.info, this.formHost, table, this.fieldList);
    this.formHost.addEventListener("keydown", (event) => this.onFormKey(event));
    for (const key of [STORE_FIELD_PATHS, STORE_SCHEMA]) this.unsubscribe.push(ctx.store.subscribe(key, () => this.fillFields()));
    this.fillFields();
    this.renderHead();
    this.renderRows();
    void this.load(false);
  }

  dispose(): void {
    this.disposed = true;
    for (const stop of this.unsubscribe) stop();
  }

  saveState(): unknown {
    return { ...this.state };
  }

  assistantContext(): string | null {
    const target = { database: this.database, collection: this.collection };
    const lines = [this.result ? indexesSummary(this.result, target) : `Indexes of ${this.database}.${this.collection}: not read yet.`];
    if (!this.formHost.hidden) lines.push(`Create index form open: ${formShell(this.form, this.collection)}`);
    if (this.lastError) lines.push(`Last error: ${this.lastError}`);
    return lines.join("\n");
  }

  /** `{ createIndex: { keys, options } }` from another section opens the form prefilled. */
  receive(request: unknown): void {
    const form = formOfRequest(request);
    if (form) this.openForm(form);
  }

  // --- Loading ------------------------------------------------------------------------------------------------------

  private async load(ask: boolean): Promise<void> {
    const generation = ++this.generation;
    this.loading = true;
    this.refreshButton.disabled = true;
    if (!this.result) this.renderRows();
    try {
      const result = await this.api.indexes(ask);
      if (this.disposed || generation !== this.generation) return;
      if (!result) {
        if (!this.result) this.showInfo("Press Refresh to read the indexes.", "info");
        return;
      }
      this.result = result;
      this.lastError = null;
      this.ctx.store.set(STORE_INDEXES, result);
      this.showInfo(result.stats.note, "info");
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.lastError = errorText(error);
      this.showInfo(ask || this.result ? this.lastError : "Press Refresh to read the indexes.", ask || this.result ? "error" : "info");
    } finally {
      if (generation === this.generation && !this.disposed) {
        this.loading = false;
        this.refreshButton.disabled = false;
        this.drawMeta();
        this.renderRows();
      }
    }
  }

  private showInfo(message: string | null, kind: "info" | "error"): void {
    this.info.hidden = !message;
    this.info.classList.toggle("ix-info-error", kind === "error");
    this.info.replaceChildren(...(message ? [icon(kind === "error" ? "x" : "health", "ix-info-icon"), h("span", {}, message)] : []));
  }

  private drawMeta(): void {
    const result = this.result;
    if (!result) {
      this.meta.textContent = "";
      return;
    }
    const parts = [`${formatNumber(result.indexes.length)} ${result.indexes.length === 1 ? "index" : "indexes"}`];
    if (result.totalSize !== null) parts.push(bytesText(result.totalSize));
    parts.push(`${formatNumber(result.elapsedMs)} ms`);
    this.meta.textContent = parts.join(" · ");
  }

  // --- The table ----------------------------------------------------------------------------------------------------

  private renderHead(): void {
    const sortable = (label: string, sort: IndexSort | null, extra: string) => {
      if (!sort) return h("span", { class: `ix-cell ${extra}`, role: "columnheader" }, label);
      const on = this.state.sort === sort;
      const button = h(
        "button",
        { class: `ix-sort${on ? " on" : ""}`, type: "button", title: `Sort by ${label.toLowerCase()}` },
        label,
        on ? h("span", { class: "ix-sort-dir" }, this.state.descending ? "↓" : "↑") : null,
      );
      button.addEventListener("click", () => {
        if (this.state.sort === sort) this.state.descending = !this.state.descending;
        else this.state = { ...this.state, sort, descending: sort !== "name" };
        this.ctx.stateChanged();
        this.renderHead();
        this.renderRows();
      });
      return h(
        "span",
        { class: `ix-cell ${extra}`, role: "columnheader", "aria-sort": on ? (this.state.descending ? "descending" : "ascending") : "none" },
        button,
      );
    };
    this.head.replaceChildren(
      sortable("Name", "name", "ix-name"),
      sortable("Keys", null, "ix-keys"),
      sortable("Properties", null, "ix-props"),
      sortable("Size", "size", "ix-size"),
      sortable("Usage", "usage", "ix-usage"),
      h("span", { class: "ix-cell ix-actions", role: "columnheader", "aria-label": "Actions" }),
    );
  }

  private renderRows(): void {
    const result = this.result;
    if (!result) {
      this.rows.replaceChildren(h("div", { class: "ix-empty" }, this.loading ? "Reading the indexes…" : "No indexes read yet."));
      return;
    }
    const shown = visibleIndexes(result.indexes, this.state);
    if (shown.length === 0) {
      this.rows.replaceChildren(h("div", { class: "ix-empty" }, result.indexes.length ? "No index matches." : "This collection has no indexes."));
      return;
    }
    this.rows.replaceChildren(...shown.map((index) => this.row(index)));
  }

  private row(index: IndexInfo): HTMLElement {
    const hidden = index.options.hidden === true;
    const unused = isUnused(index);
    const keys = h(
      "span",
      { class: "ix-cell ix-keys" },
      ...displayKeys(index).map((key) =>
        h(
          "span",
          { class: "ix-key", title: `${key.field}: ${typeof key.kind === "number" ? key.kind : `"${key.kind}"`}` },
          h("span", { class: "ix-key-field" }, key.field),
          h("span", { class: `ix-kind ix-kind-${typeof key.kind === "number" ? (key.kind > 0 ? "asc" : "desc") : key.kind}` }, kindLabel(key.kind)),
        ),
      ),
    );
    const type = indexType(index);
    const props = h(
      "span",
      { class: "ix-cell ix-props" },
      type !== "single" && type !== "compound" ? h("span", { class: "ix-badge ix-badge-type", title: `${type} index` }, type) : null,
      ...indexBadges(index).map((badge) => h("span", { class: `ix-badge ix-badge-${badge.id}`, title: badge.title }, badge.label)),
    );
    const usage = h("span", { class: "ix-cell ix-usage" });
    if (index.usage) {
      usage.append(h("span", { class: "ix-ops" }, `${formatNumber(index.usage.ops)} ops`));
      if (index.usage.since)
        usage.append(
          h("span", { class: "ix-dim", title: `Counted since ${new Date(index.usage.since).toLocaleString()}` }, ` since ${sinceText(index.usage.since)}`),
        );
      if (unused)
        usage.append(
          h(
            "span",
            {
              class: "ix-unused",
              title: `No query used this index in at least ${UNUSED_DAYS} days (since the server started or the index was built). Consider hiding it first, then dropping it.`,
            },
            "Unused",
          ),
        );
    } else usage.append(h("span", { class: "ix-dim" }, "—"));
    const more = iconButton("more", `Actions for ${index.name}`, () => showMenuBelow(more, this.menu(index)));
    more.setAttribute("aria-haspopup", "menu");
    const copy = iconButton("copy", "Copy as mongosh createIndex", () => void this.copyDefinition(index));
    return h(
      "div",
      { class: `ix-row${hidden ? " ix-hidden" : ""}`, role: "row", "data-name": index.name },
      h("span", { class: "ix-cell ix-name", title: index.name }, index.name),
      keys,
      props,
      h("span", { class: "ix-cell ix-size", title: index.size === null ? "Not available" : `${formatNumber(index.size)} bytes` }, bytesText(index.size)),
      usage,
      h("span", { class: "ix-cell ix-actions" }, copy, more),
    );
  }

  private menu(index: IndexInfo): MenuItem[] {
    const hidden = index.options.hidden === true;
    const isId = index.name === ID_INDEX;
    return [
      { label: "Copy as mongosh createIndex", icon: "copy", run: () => void this.copyDefinition(index) },
      { label: "Create a similar index…", icon: "plus", disabled: isId, run: () => this.openForm(this.formOf(index)) },
      "separator",
      hidden
        ? { label: "Unhide", icon: "view", detail: "Let the query planner use it again", run: () => void this.setHidden(index, false) }
        : {
            label: "Hide",
            icon: "view",
            detail: "Keep it up to date but stop the planner from using it",
            disabled: isId,
            run: () => void this.setHidden(index, true),
          },
      { label: "Drop index…", icon: "trash", danger: true, disabled: isId, run: () => void this.drop(index) },
    ];
  }

  private formOf(index: IndexInfo): IndexForm {
    return (
      formOfRequest({ createIndex: { keys: displayKeys(index), options: { ...index.options, name: `${index.name}_copy` } } }) ?? structuredClone(EMPTY_FORM)
    );
  }

  private async copyDefinition(index: IndexInfo): Promise<void> {
    await copyText(indexShell(this.collection, index));
    toast(`Copied the createIndex of ${index.name}`, "success");
  }

  private async setHidden(index: IndexInfo, hidden: boolean): Promise<void> {
    try {
      const result = await this.api.setIndexHidden(index.name, hidden);
      if (!result || this.disposed) return;
      toast(`${index.name} is ${hidden ? "hidden from" : "used by"} the query planner${result.changed ? "" : " (it already was)"}`, "success");
      await this.load(false);
    } catch (error) {
      this.lastError = errorText(error);
      this.showInfo(this.lastError, "error");
    }
  }

  private async drop(index: IndexInfo): Promise<void> {
    try {
      const result = await this.api.dropIndex(index.name);
      if (!result || this.disposed) return;
      toast(`Dropped ${index.name}`, "success");
      await this.load(false);
    } catch (error) {
      this.lastError = errorText(error);
      this.showInfo(this.lastError, "error");
    }
  }

  // --- The create form ----------------------------------------------------------------------------------------------

  /** Completion for key fields: the paths the tab sampled (Documents, Schema). */
  private fillFields(): void {
    const known = this.ctx.store.get<string[]>(STORE_FIELD_PATHS) ?? [];
    const schema = this.ctx.store.get<SchemaResult>(STORE_SCHEMA);
    const paths = [...new Set([...(schema ? schemaFieldPaths(schema) : []), ...known])].filter((path) => path !== "_id").slice(0, 600);
    this.fieldList.replaceChildren(...paths.map((path) => h("option", { value: path })), h("option", { value: "$**" }));
  }

  private openForm(form: IndexForm): void {
    this.form = form;
    this.formError = null;
    this.formHost.hidden = false;
    this.createButton.setAttribute("aria-expanded", "true");
    this.renderForm();
    this.formHost.querySelector<HTMLInputElement>(".ix-field-input")?.focus();
  }

  private closeForm(): void {
    this.formHost.hidden = true;
    this.createButton.setAttribute("aria-expanded", "false");
    this.formHost.replaceChildren();
  }

  private renderForm(): void {
    const form = this.form;
    const keyRows = h("div", { class: "ix-key-rows", role: "group", "aria-label": "Keys" });
    form.keys.forEach((key, position) => {
      const field = h("input", {
        class: "input mono ix-field-input",
        type: "text",
        spellcheck: false,
        placeholder: "field.path",
        value: key.field,
        list: this.fieldList.id,
        "aria-label": `Key ${position + 1} field`,
      });
      field.addEventListener("input", () => {
        key.field = field.value;
        this.formChanged(false);
      });
      field.addEventListener("change", () => this.formChanged(true));
      const kind = h("select", { class: "input ix-kind-select", "aria-label": `Key ${position + 1} direction or type` });
      for (const option of KEY_KINDS) {
        const item = h("option", { value: String(option.kind), title: option.title }, option.label);
        item.selected = option.kind === key.kind;
        kind.append(item);
      }
      kind.addEventListener("change", () => {
        const value = kind.value;
        key.kind = (value === "1" ? 1 : value === "-1" ? -1 : value) as KeyKind;
        this.formChanged(true);
      });
      const up = iconButton("prev", "Move up", () => this.moveKey(position, -1));
      up.disabled = position === 0;
      const remove = iconButton("x", "Remove this key", () => {
        form.keys.splice(position, 1);
        if (form.keys.length === 0) form.keys.push({ field: "", kind: 1 });
        this.formChanged(true);
      });
      keyRows.append(h("div", { class: "ix-key-row" }, h("span", { class: "ix-key-n" }, String(position + 1)), field, kind, up, remove));
    });
    const addKey = h("button", { class: "btn small", type: "button" }, icon("plus"), h("span", {}, "Add key"));
    addKey.addEventListener("click", () => {
      form.keys.push({ field: "", kind: 1 });
      this.formChanged(true);
      this.formHost.querySelectorAll<HTMLInputElement>(".ix-field-input")[form.keys.length - 1]?.focus();
    });

    const text = (label: string, name: FormField | "defaultLanguage", placeholder: string, hint: string, mono = true) => {
      const input = h("input", {
        class: `input${mono ? " mono" : ""} ix-opt-input`,
        type: "text",
        spellcheck: false,
        placeholder,
        value: String(form[name as keyof IndexForm] ?? ""),
        "aria-label": label,
        "data-field": name,
      });
      input.addEventListener("input", () => {
        (form as unknown as Record<string, unknown>)[name] = input.value;
        this.formChanged(false);
      });
      const wrong = this.formError?.field === name;
      if (wrong) input.setAttribute("aria-invalid", "true");
      return h("label", { class: `ix-opt${wrong ? " invalid" : ""}`, title: hint }, h("span", { class: "ix-opt-label" }, label), input);
    };
    const check = (label: string, name: "unique" | "sparse" | "hidden", hint: string) => {
      const input = h("input", { type: "checkbox", checked: form[name] });
      input.addEventListener("change", () => {
        form[name] = input.checked;
        this.formChanged(false);
      });
      return h("label", { class: "ix-check", title: hint }, input, h("span", {}, label));
    };
    const named = h("input", {
      class: "input mono ix-opt-input",
      type: "text",
      spellcheck: false,
      placeholder: defaultIndexName(form.keys) || "default name",
      value: form.name,
      "aria-label": "Name",
      "data-field": "name",
    });
    named.addEventListener("input", () => {
      form.name = named.value;
      this.formChanged(false);
    });
    const hasText = form.keys.some((key) => key.kind === "text");
    const hasWildcard = form.keys.some((key) => isWildcard(key.field.trim()));
    const options = h(
      "div",
      { class: "ix-opts" },
      h("label", { class: "ix-opt", title: "Leave empty for the server's default name" }, h("span", { class: "ix-opt-label" }, "Name"), named),
      text("Partial filter", "partial", "{ status: 'active' }", "Index only the documents matching this filter (no $where or other JavaScript)"),
      text("TTL (seconds)", "ttl", "e.g. 86400", "Delete documents this many seconds after the date in the indexed field (single field only)"),
      text("Collation", "collation", "{ locale: 'en', strength: 2 }", "String comparison rules; queries must use the same collation to use the index"),
      hasWildcard ? text("Wildcard projection", "wildcardProjection", "{ 'meta.private': 0 }", "Fields a $** index includes or leaves out") : null,
      hasText ? text("Default language", "defaultLanguage", "english", "The language of the text index (stemming, stop words)", false) : null,
      h(
        "div",
        { class: "ix-checks" },
        check("Unique", "unique", "Refuse two documents with the same key"),
        check("Sparse", "sparse", "Leave out documents without the indexed fields"),
        check("Hidden", "hidden", "Build it hidden from the planner (unhide it later)"),
      ),
    );
    const preview = h("pre", { class: "ix-preview mono", "aria-label": "mongosh command", "aria-live": "polite" }, formShell(form, this.collection));
    const error = h("div", { class: "ix-form-error", role: "alert", hidden: !this.formError }, this.formError?.error ?? "");
    const copy = h("button", { class: "btn", type: "button", title: "Copy the mongosh command" }, icon("copy"), h("span", {}, "Copy command"));
    copy.addEventListener("click", () => void copyText(formShell(this.form, this.collection)).then(() => toast("Copied the createIndex command", "success")));
    const cancel = h("button", { class: "btn", type: "button" }, "Cancel");
    cancel.addEventListener("click", () => this.closeForm());
    const submit = h(
      "button",
      { class: "btn primary ix-submit", type: "button", disabled: this.submitting },
      icon("play"),
      h("span", {}, this.submitting ? "Creating…" : "Create"),
    );
    submit.addEventListener("click", () => void this.submit());
    this.formHost.replaceChildren(
      h("div", { class: "ix-form-head" }, h("strong", {}, "Create index"), h("span", { class: "ix-dim" }, `on ${this.database}.${this.collection}`)),
      h("div", { class: "ix-form-body" }, h("div", { class: "ix-form-keys" }, h("div", { class: "ix-opt-label" }, "Keys"), keyRows, addKey), options),
      preview,
      error,
      h("div", { class: "ix-form-actions" }, copy, h("span", { class: "spacer" }), cancel, submit),
    );
  }

  private onFormKey(event: KeyboardEvent): void {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      void this.submit();
    } else if (event.key === "Escape" && !this.submitting) {
      event.preventDefault();
      this.closeForm();
      this.createButton.focus();
    }
  }

  private moveKey(position: number, by: number): void {
    const keys = this.form.keys;
    const to = position + by;
    if (to < 0 || to >= keys.length) return;
    [keys[position], keys[to]] = [keys[to], keys[position]];
    this.formChanged(true);
  }

  /** The form changed: the preview follows; `structure` (keys added, removed, a kind changed) draws the form again. */
  private formChanged(structure: boolean): void {
    this.formError = null;
    if (structure) {
      const focused = document.activeElement;
      const index = [...this.formHost.querySelectorAll("input, select")].indexOf(focused as Element);
      this.renderForm();
      if (index >= 0) this.formHost.querySelectorAll<HTMLElement>("input, select")[index]?.focus();
      return;
    }
    const preview = this.formHost.querySelector(".ix-preview");
    if (preview) preview.textContent = formShell(this.form, this.collection);
    const named = this.formHost.querySelector<HTMLInputElement>('[data-field="name"]');
    if (named) named.placeholder = defaultIndexName(this.form.keys) || "default name";
    const error = this.formHost.querySelector<HTMLElement>(".ix-form-error");
    if (error) error.hidden = true;
    for (const invalid of this.formHost.querySelectorAll(".invalid")) invalid.classList.remove("invalid");
  }

  private async submit(): Promise<void> {
    if (this.submitting) return;
    const built = formArgs(this.form, { database: this.database, collection: this.collection });
    if (!built.ok) {
      this.formError = { field: built.field, error: built.error };
      this.renderForm();
      const target = built.field === "keys" ? ".ix-field-input" : `[data-field="${built.field}"]`;
      this.formHost.querySelector<HTMLElement>(target)?.focus();
      return;
    }
    this.submitting = true;
    this.renderForm();
    try {
      const result = await this.api.createIndex(built.args);
      if (this.disposed) return;
      if (!result) return;
      toast(result.created ? `Created ${result.name}` : `${result.name} already exists with these keys and options`, result.created ? "success" : "info");
      this.closeForm();
      await this.load(false);
    } catch (error) {
      if (this.disposed) return;
      this.lastError = errorText(error);
      this.formError = { field: "keys", error: this.lastError };
    } finally {
      this.submitting = false;
      if (!this.formHost.hidden && !this.disposed) this.renderForm();
    }
  }
}
