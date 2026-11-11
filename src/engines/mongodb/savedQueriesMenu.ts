/**
 * The query bar's saved queries: "Save" asks for a name and keeps the bar's fields (a name the collection already has
 * replaces that query, after asking), and the Saved dropdown lists this collection's saved queries first, then the
 * others of the database, with a search box; a row applies its query (another collection's opens in its own tab), its
 * buttons rename or delete it. The list is read from the backend when the dropdown opens and after each change.
 *
 * The Aggregations section uses the same list for pipelines (`pipeline`): Save keeps the stages, and a saved pipeline
 * opens in the Aggregations section (`applyPipeline`) wherever it is picked; a saved find opens in Documents.
 */
import { api } from "../../core/api";
import { confirmDialog, field, openModal, textInput, toast } from "../../core/dialogs";
import { h, icon } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import { placeInViewport } from "../../core/placement";
import type { SavedQuery } from "../../core/types";
import type { QueryState } from "./queryModel";
import { groupSaved, pipelineOfSaved, sameName, savedQueryOf, savedSummary, stateOfSaved, suggestName, type SavedQueryTarget } from "./savedQueries";

export interface SavedQueriesOptions extends SavedQueryTarget {
  /** The bar's fields now. */
  state(): QueryState;
  /** The stages to save (the Aggregations section): Save keeps a pipeline, not the bar's fields. */
  pipeline?(): string[];
  /** Puts a saved query of this collection into the bar and runs it. */
  apply(state: QueryState): void;
  /** Opens a saved pipeline of this collection in the Aggregations section. */
  applyPipeline(stages: string[]): void;
  /** Opens a saved query of another collection of the database in that collection's workspace. */
  openElsewhere(saved: SavedQuery): void;
}

/** Asks for a name; null when cancelled. */
export function promptName(title: string, initial: string, actionLabel: string): Promise<string | null> {
  return new Promise((resolve) => {
    let answer: string | null = null;
    const input = textInput(initial, { maxlength: "200", "aria-label": "Name" });
    const error = h("div", { class: "field-hint mq-saved-error", role: "alert" });
    openModal(
      title,
      h("div", { class: "form-grid" }, field("Name", input), error),
      [
        { label: "Cancel", run: () => undefined },
        {
          label: actionLabel,
          primary: true,
          run: () => {
            if (!input.value.trim()) {
              error.textContent = "A name is required.";
              input.focus();
              return false;
            }
            answer = input.value.trim();
          },
        },
      ],
      { width: 420, onClose: () => resolve(answer) },
    );
    input.select();
  });
}

const newId = (): string =>
  typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `sq-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;

export class SavedQueries {
  private list: SavedQuery[] = [];
  /** The saved query applied last in this bar (its name is offered when saving again). */
  private applied: SavedQuery | null = null;
  private popover: { element: HTMLElement; anchor: HTMLElement; close: () => void } | null = null;

  constructor(private readonly options: SavedQueriesOptions) {}

  /** Reads the list again; keeps the last one when the backend cannot be reached. */
  async refresh(): Promise<SavedQuery[]> {
    try {
      this.list = await api.listSavedQueries(this.options.connectionId);
    } catch (error) {
      showError(error, "Read the saved queries");
    }
    return this.list;
  }

  /** "Save": asks for a name and saves the bar's fields under it. */
  async save(): Promise<void> {
    const { database, collection } = this.options;
    await this.refresh();
    const initial =
      this.applied && this.applied.database === database && this.applied.collection === collection
        ? this.applied.name
        : suggestName(this.list, database, collection, this.options.pipeline ? "pipeline" : "query");
    const stages = this.options.pipeline?.() ?? null;
    if (stages && stages.length === 0) {
      toast("Add a stage first: an empty pipeline is not saved.", "error");
      return;
    }
    const name = await promptName(stages ? "Save pipeline" : "Save query", initial, "Save");
    if (name === null) return;
    const existing = sameName(this.list, { database, collection, name });
    if (existing && !(await confirmDialog(`Replace the saved query "${existing.name}" of ${collection}?`, "Replace"))) return;
    const saved = savedQueryOf(existing?.id ?? newId(), name, this.options.state(), this.options, stages);
    try {
      this.list = await api.saveSavedQuery(saved);
      this.applied = this.list.find((item) => item.id === saved.id) ?? saved;
      toast(`Saved "${name}"`, "success");
    } catch (error) {
      showError(error, "Save the query");
    }
  }

  /** Opens the Saved dropdown under `anchor`; a second click on the same button closes it. */
  async open(anchor: HTMLElement): Promise<void> {
    if (this.popover?.anchor === anchor) {
      this.popover.close();
      return;
    }
    this.popover?.close();
    const search = h("input", {
      class: "input mq-saved-search",
      type: "search",
      placeholder: "Search saved queries",
      spellcheck: "false",
      "aria-label": "Search saved queries",
    });
    const list = h("div", { class: "mq-saved-list", role: "listbox", "aria-label": "Saved queries" });
    const saveButton = h(
      "button",
      { class: "btn small", type: "button", title: "Save the fields of the bar under a name" },
      icon("plus"),
      h("span", {}, "Save current…"),
    );
    const element = h(
      "div",
      { class: "mq-saved-popover", role: "dialog", "aria-label": "Saved queries" },
      h("div", { class: "mq-saved-head" }, icon("search"), search, saveButton),
      list,
    );
    let focused = -1;
    const rows = () => Array.from(list.querySelectorAll<HTMLElement>(".mq-saved-row"));
    const focusRow = (index: number) => {
      const all = rows();
      focused = Math.max(-1, Math.min(all.length - 1, index));
      all.forEach((row, i) => row.classList.toggle("focused", i === focused));
      all[focused]?.scrollIntoView({ block: "nearest" });
    };
    const render = () => {
      const groups = groupSaved(this.list, this.options.database, this.options.collection, search.value);
      focused = -1;
      const children: HTMLElement[] = [];
      if (groups.here.length > 0) {
        children.push(h("div", { class: "mq-saved-group" }, this.options.collection));
        children.push(...groups.here.map((saved) => this.row(saved, false, close, render)));
      }
      if (groups.others.length > 0) {
        children.push(h("div", { class: "mq-saved-group" }, `Other collections of ${this.options.database}`));
        children.push(...groups.others.map((saved) => this.row(saved, true, close, render)));
      }
      if (children.length === 0) {
        const empty = search.value.trim() ? "No saved query matches" : "No saved queries in this database yet. Save the fields of the bar to find them here.";
        children.push(h("div", { class: "mq-saved-empty" }, empty));
      }
      list.replaceChildren(...children);
    };
    const onPointerDown = (event: PointerEvent) => {
      const node = event.target as Node;
      if (element.contains(node) || anchor.contains(node)) return;
      // A dialog opened from the dropdown (rename, delete) keeps it open.
      if (node instanceof Element && node.closest(".modal-overlay")) return;
      close();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !document.querySelector(".modal-overlay")) {
        event.stopPropagation();
        close();
      }
    };
    const close = () => {
      element.remove();
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKey, true);
      if (this.popover?.element === element) this.popover = null;
    };
    search.addEventListener("input", render);
    element.addEventListener("keydown", (event) => {
      event.stopPropagation();
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        focusRow(focused + (event.key === "ArrowDown" ? 1 : -1));
      } else if (event.key === "Enter" && event.target === search) {
        event.preventDefault();
        (rows()[Math.max(0, focused)] as HTMLElement | undefined)?.click();
      }
    });
    saveButton.addEventListener("click", () => {
      close();
      void this.save();
    });
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKey, true);
    this.popover = { element, anchor, close };
    list.append(h("div", { class: "mq-saved-empty" }, "Loading…"));
    document.body.append(element);
    this.place(element, anchor);
    search.focus();
    await this.refresh();
    if (this.popover?.element !== element) return;
    render();
    this.place(element, anchor);
  }

  dispose(): void {
    this.popover?.close();
  }

  private row(saved: SavedQuery, elsewhere: boolean, close: () => void, render: () => void): HTMLElement {
    const summary = savedSummary(saved);
    const pipeline = pipelineOfSaved(saved);
    const rename = h("button", { class: "icon-button", type: "button", title: "Rename", "aria-label": `Rename ${saved.name}` }, icon("edit"));
    const remove = h("button", { class: "icon-button", type: "button", title: "Delete", "aria-label": `Delete ${saved.name}` }, icon("trash"));
    const row = h(
      "div",
      { class: "mq-saved-row", role: "option", title: elsewhere ? `Open in ${saved.collection}: ${summary}` : `Apply and run: ${summary}` },
      h(
        "div",
        { class: "mq-saved-text" },
        h(
          "div",
          { class: "mq-saved-name" },
          saved.name,
          pipeline ? h("span", { class: "mq-saved-kind" }, "pipeline") : null,
          elsewhere ? h("span", { class: "mq-saved-collection" }, saved.collection) : null,
        ),
        h("div", { class: "mq-saved-summary" }, summary.length > 120 ? `${summary.slice(0, 119)}…` : summary),
      ),
      h("span", { class: "mq-saved-actions" }, rename, remove),
    );
    row.addEventListener("click", () => {
      close();
      this.applied = saved;
      if (elsewhere) this.options.openElsewhere(saved);
      else if (pipeline) this.options.applyPipeline(pipeline);
      else this.options.apply(stateOfSaved(saved));
    });
    rename.addEventListener("click", (event) => {
      event.stopPropagation();
      void this.rename(saved).then(render);
    });
    remove.addEventListener("click", (event) => {
      event.stopPropagation();
      void this.remove(saved).then(render);
    });
    return row;
  }

  private async rename(saved: SavedQuery): Promise<void> {
    const name = await promptName("Rename saved query", saved.name, "Rename");
    if (name === null || name === saved.name) return;
    const clash = sameName(this.list, { ...saved, name }, saved.id);
    if (clash) {
      toast(`${saved.collection} already has a saved query "${clash.name}"`, "error");
      return;
    }
    try {
      this.list = await api.renameSavedQuery(saved.id, name);
      if (this.applied?.id === saved.id) this.applied = this.list.find((item) => item.id === saved.id) ?? null;
    } catch (error) {
      showError(error, "Rename the saved query");
    }
  }

  private async remove(saved: SavedQuery): Promise<void> {
    if (!(await confirmDialog(`Delete the saved query "${saved.name}"?`, "Delete"))) return;
    try {
      this.list = await api.deleteSavedQuery(saved.id);
      if (this.applied?.id === saved.id) this.applied = null;
    } catch (error) {
      showError(error, "Delete the saved query");
    }
  }

  private place(element: HTMLElement, anchor: HTMLElement): void {
    const rect = anchor.getBoundingClientRect();
    const width = element.offsetWidth;
    const x = rect.left + width > window.innerWidth - 4 ? rect.right - width : rect.left;
    const placed = placeInViewport(x, rect.bottom + 4, width, element.offsetHeight, window.innerWidth, window.innerHeight);
    element.style.left = `${placed.left}px`;
    element.style.top = `${placed.top}px`;
  }
}
