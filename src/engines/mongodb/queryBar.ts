/**
 * The query bar of a collection's Documents section: Filter on the first row; Project, Sort, Skip and Limit on the
 * options row; Collation behind its own toggle. Fields take mongosh syntax (braces optional) and are parsed by
 * `queryModel.ts`; a field that does not parse is underlined at the error and named under the bar. Ctrl+Enter (or
 * Enter on a one-line field) runs; ArrowUp / ArrowDown in the filter and the history button recall recent filters of
 * this collection (this tab's, then the query history's). Save keeps the fields under a name and the Saved button lists
 * the saved queries (`savedQueriesMenu.ts`).
 */
import { api } from "../../core/api";
import { showMenuBelow, type MenuItem } from "../../core/contextMenu";
import { h, icon } from "../../core/dom";
import { queryBarCompletionSource, withFields, type QueryBarField } from "../../lang/mongo/queryCompletion";
import type { SchemaCache } from "../../lang/schemaCache";
import type { MongoApi } from "./mongoApi";
import type { SavedQueries } from "./savedQueriesMenu";
import { FieldEditor } from "./queryEditor";
import { clampLimit, commandPrefix, uniqueFilters, FIELD_LABELS, parseCommandText, type FieldError, type QueryField, type QueryState } from "./queryModel";
import { lineColumn } from "../../lang/mongo/shell";

const PLACEHOLDERS: Record<QueryField, string> = {
  filter: "{ status: 'paid' }  (Ctrl+Space lists fields)",
  project: "{ field: 1 }  or  { field: 0 }",
  sort: "{ field: -1 }",
  collation: "{ locale: 'en', strength: 2 }",
};

/** Debounce of value completion requests, while typing. */
const VALUE_DELAY_MS = 250;
/** History entries read for the recall list. */
const HISTORY_LIMIT = 100;

export interface QueryBarOptions {
  connectionId: string;
  collection: string;
  initial: QueryState & { options: boolean };
  api: MongoApi;
  schema: SchemaCache;
  /** Field paths seen in the documents shown (completion adds them to the sampled schema). */
  fieldPaths(): readonly string[];
  /** Filters run lately in this tab, newest first. */
  recent(): readonly string[];
  run(): void;
  reset(): void;
  /** A field or the options row changed (the section saves its state). */
  changed(): void;
  /** The items of the bar's "More" menu (open as aggregation, bulk actions on the matching documents); none: no menu. */
  actions?(): MenuItem[];
  /** Save and the Saved dropdown; none: no buttons. */
  saved?: SavedQueries;
}

export class QueryBar {
  readonly element: HTMLElement;
  private readonly editors = new Map<QueryField, FieldEditor>();
  private readonly skip: HTMLInputElement;
  private readonly limit: HTMLInputElement;
  private readonly optionsRow: HTMLElement;
  private readonly collationRow: HTMLElement;
  private readonly optionsButton: HTMLButtonElement;
  private readonly collationButton: HTMLButtonElement;
  private readonly errorLine = h("div", { class: "mq-error", role: "alert", hidden: true });
  private historyFilters: string[] | null = null;
  /** The position in the recall list while stepping with ArrowUp / ArrowDown; -1 is the text being typed. */
  private recallIndex = -1;
  private draft = "";

  constructor(private readonly options: QueryBarOptions) {
    const initial = options.initial;
    const filterHost = h("div", { class: "mq-editor mq-filter" });
    this.addEditor("filter", filterHost, initial.filter);

    const historyButton = h(
      "button",
      { class: "tool-button mq-history", type: "button", title: "Recent filters (ArrowUp / ArrowDown in the filter)", "aria-label": "Recent filters" },
      icon("history"),
    );
    historyButton.addEventListener("click", () => void this.openRecent(historyButton));
    this.optionsButton = h(
      "button",
      { class: "tool-button mq-options-toggle", type: "button", "aria-expanded": "false", title: "Project, Sort, Skip, Limit" },
      icon("chevronDown"),
      h("span", {}, "Options"),
    );
    this.optionsButton.addEventListener("click", () => this.setOptionsOpen(this.optionsRow.hidden));
    const saved = options.saved;
    const savedButton = saved
      ? h(
          "button",
          { class: "tool-button mq-saved", type: "button", title: "Saved queries of this database", "aria-label": "Saved queries", "aria-haspopup": "dialog" },
          icon("bookmark"),
          h("span", {}, "Saved"),
        )
      : null;
    savedButton?.addEventListener("click", () => void saved?.open(savedButton));
    const saveButton = saved ? h("button", { class: "btn mq-save", type: "button", title: "Save the fields under a name" }, "Save") : null;
    saveButton?.addEventListener("click", () => void saved?.save());
    const reset = h("button", { class: "btn mq-reset", type: "button", title: "Clear every field and run" }, "Reset");
    reset.addEventListener("click", () => options.reset());
    const find = h("button", { class: "btn primary mq-find", type: "button", title: "Run the query (Ctrl+Enter)" }, "Find");
    find.addEventListener("click", () => options.run());

    const projectHost = h("div", { class: "mq-editor" });
    const sortHost = h("div", { class: "mq-editor" });
    const collationHost = h("div", { class: "mq-editor" });
    this.addEditor("project", projectHost, initial.project);
    this.addEditor("sort", sortHost, initial.sort);
    this.addEditor("collation", collationHost, initial.collation);
    this.skip = this.numberInput("Skip", initial.skip, 0);
    this.limit = this.numberInput("Limit", clampLimit(initial.limit), 1);
    this.collationButton = h("button", { class: "link-button mq-collation-toggle", type: "button", "aria-expanded": "false" }, "Collation");
    this.collationButton.addEventListener("click", () => this.setCollationOpen(this.collationRow.hidden));

    this.optionsRow = h(
      "div",
      { class: "mq-row mq-options", hidden: true },
      this.labelled("Project", projectHost, "mq-grow"),
      this.labelled("Sort", sortHost, "mq-grow"),
      this.labelled("Skip", this.skip),
      this.labelled("Limit", this.limit),
      this.collationButton,
    );
    this.collationRow = h("div", { class: "mq-row mq-collation", hidden: true }, this.labelled("Collation", collationHost, "mq-grow"));

    this.element = h(
      "div",
      { class: "mq-bar" },
      h(
        "div",
        { class: "mq-row" },
        this.labelled("Filter", filterHost, "mq-grow"),
        historyButton,
        savedButton,
        this.optionsButton,
        saveButton,
        reset,
        find,
        this.moreButton(),
      ),
      this.optionsRow,
      this.collationRow,
      this.errorLine,
    );
    this.setOptionsOpen(initial.options || !!initial.project.trim() || !!initial.sort.trim() || initial.skip > 0 || !!initial.collation.trim(), false);
    this.setCollationOpen(!!initial.collation.trim(), false);
  }

  /** The "More" button with the bar's actions (null without any). */
  private moreButton(): HTMLButtonElement | null {
    const actions = this.options.actions;
    if (!actions) return null;
    const button = h(
      "button",
      { class: "tool-button mq-more", type: "button", title: "Open as aggregation; update or delete the matching documents", "aria-label": "More actions" },
      icon("more"),
    );
    button.addEventListener("click", () => showMenuBelow(button, actions()));
    return button;
  }

  state(): QueryState {
    return {
      filter: this.text("filter"),
      project: this.text("project"),
      sort: this.text("sort"),
      collation: this.text("collation"),
      skip: Math.max(0, Math.floor(Number(this.skip.value) || 0)),
      limit: clampLimit(this.limit.value),
    };
  }

  setState(state: QueryState): void {
    for (const [field, editor] of this.editors) editor.text = state[field];
    this.skip.value = String(state.skip);
    this.limit.value = String(clampLimit(state.limit));
    this.showError(null);
    // What is set is shown: a saved or reopened query may have options and a collation.
    if (state.project.trim() || state.sort.trim() || state.skip > 0 || state.collation.trim()) this.setOptionsOpen(true, false);
    if (state.collation.trim()) this.setCollationOpen(true, false);
  }

  optionsOpen(): boolean {
    return !this.optionsRow.hidden;
  }

  /** Shows a field's parse error (underline and the line under the bar), or clears every error. */
  showError(error: FieldError | null): void {
    for (const editor of this.editors.values()) editor.showError(null);
    if (!error) {
      this.errorLine.hidden = true;
      this.errorLine.replaceChildren();
      return;
    }
    if (error.field !== "filter") this.setOptionsOpen(true, false);
    if (error.field === "collation") this.setCollationOpen(true, false);
    const editor = this.editors.get(error.field);
    editor?.showError(error.position);
    const { line, column } = lineColumn(editor?.text ?? "", error.position);
    const where = h(
      "button",
      { class: "link-button mq-error-where", type: "button", title: "Go to the error" },
      `${FIELD_LABELS[error.field]}, line ${line}, column ${column}`,
    );
    where.addEventListener("click", () => editor?.focus(error.position));
    this.errorLine.replaceChildren(icon("x", "mq-error-icon"), where, h("span", {}, error.message));
    this.errorLine.hidden = false;
  }

  focus(): void {
    this.editors.get("filter")?.focus();
  }

  /** Puts the cursor at a field's error. */
  focusError(error: FieldError): void {
    this.editors.get(error.field)?.focus(error.position);
  }

  /** A run happened: the recall list starts over from the text being typed. */
  ran(): void {
    this.recallIndex = -1;
  }

  dispose(): void {
    this.options.saved?.dispose();
    for (const editor of this.editors.values()) editor.destroy();
    this.editors.clear();
  }

  private text(field: QueryField): string {
    return this.editors.get(field)?.text ?? "";
  }

  private addEditor(field: QueryField, host: HTMLElement, doc: string): void {
    const { api: mongo, collection } = this.options;
    const values =
      field === "filter"
        ? async (path: string): Promise<unknown[]> => {
            if (!mongo.hasDistinct(path)) await new Promise((resolve) => setTimeout(resolve, VALUE_DELAY_MS));
            return mongo.distinct(path);
          }
        : undefined;
    const editor = new FieldEditor(host, {
      doc,
      label: FIELD_LABELS[field],
      placeholder: PLACEHOLDERS[field],
      completion: queryBarCompletionSource({
        field: field as QueryBarField,
        collection,
        schema: () => withFields(this.options.schema.get(), collection, this.options.fieldPaths()),
        values,
      }),
      run: () => this.options.run(),
      changed: () => {
        if (field === "filter") this.showErrorCleared(field);
        this.options.changed();
      },
      onHistory: field === "filter" ? (direction) => this.recall(direction) : undefined,
    });
    this.editors.set(field, editor);
  }

  /** Typing in the field with the error hides the message (the underline goes with the edit). */
  private showErrorCleared(field: QueryField): void {
    if (!this.errorLine.hidden && this.errorLine.textContent?.startsWith(FIELD_LABELS[field])) this.showError(null);
  }

  private numberInput(label: string, value: number, min: number): HTMLInputElement {
    const input = h("input", { class: "input mq-number", type: "number", min: String(min), step: "1", value: String(value), "aria-label": label });
    input.addEventListener("change", () => this.options.changed());
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        this.options.run();
      }
    });
    return input;
  }

  private labelled(label: string, control: HTMLElement, extra = ""): HTMLElement {
    return h("label", { class: `mq-field ${extra}`.trim() }, h("span", { class: "mq-label" }, label), control);
  }

  private setOptionsOpen(open: boolean, notify = true): void {
    this.optionsRow.hidden = !open;
    this.optionsButton.classList.toggle("active", open);
    this.optionsButton.setAttribute("aria-expanded", String(open));
    if (!open) this.setCollationOpen(false, false);
    if (notify) this.options.changed();
  }

  private setCollationOpen(open: boolean, notify = true): void {
    // A collation that is set stays visible: hiding it would hide what changes the results.
    const keep = !open && this.text("collation").trim() !== "" && !this.optionsRow.hidden;
    this.collationRow.hidden = !open && !keep;
    this.collationButton.setAttribute("aria-expanded", String(!this.collationRow.hidden));
    if (notify) this.options.changed();
  }

  /** This tab's recent filters, then the query history's for this collection (read once). */
  private async recallList(): Promise<string[]> {
    if (this.historyFilters === null) {
      this.historyFilters = [];
      try {
        const prefix = commandPrefix(this.options.collection);
        const entries = await api.historyList({ connectionId: this.options.connectionId, search: prefix, limit: HISTORY_LIMIT });
        this.historyFilters = entries
          .map((entry) => parseCommandText(entry.sql, this.options.collection)?.filter.trim() ?? "")
          .filter((filter) => filter !== "");
      } catch {
        // No history (turned off, or the web edition without one): the tab's own list is enough.
      }
    }
    return uniqueFilters([...this.options.recent(), ...this.historyFilters]);
  }

  private recall(direction: -1 | 1): boolean {
    const list = [...this.options.recent()];
    // The history list joins in the background; the first press works with what the tab has.
    void this.recallList();
    const all = uniqueFilters(this.historyFilters ? [...list, ...this.historyFilters] : list);
    if (all.length === 0) return false;
    const editor = this.editors.get("filter");
    if (!editor) return false;
    if (this.recallIndex === -1) this.draft = editor.text;
    const next = Math.max(-1, Math.min(all.length - 1, this.recallIndex - direction));
    if (next === this.recallIndex) return true;
    this.recallIndex = next;
    editor.text = next === -1 ? this.draft : all[next];
    return true;
  }

  private async openRecent(anchor: HTMLElement): Promise<void> {
    const list = await this.recallList();
    const items: MenuItem[] =
      list.length === 0
        ? [{ label: "No recent filters for this collection", disabled: true, run: () => undefined }]
        : list.slice(0, 20).map((filter) => ({
            label: filter.length > 90 ? `${filter.slice(0, 89)}…` : filter,
            run: () => {
              const editor = this.editors.get("filter");
              if (!editor) return;
              editor.text = filter;
              this.options.changed();
              this.options.run();
            },
          }));
    showMenuBelow(anchor, items);
  }
}
