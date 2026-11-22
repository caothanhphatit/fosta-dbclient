/**
 * The left pane of a compare tab: diff items grouped by status (Different, Only in source, Only in target, Identical),
 * each with a checkbox (what goes into the script), a kind icon, a change count and a risk badge, plus the objects that
 * were not compared.
 */
import type { DiffItem, DiffStatus, SkippedObject } from "../../core/contract";
import { h, icon } from "../../core/dom";
import { changeSummary, displayRisk, groupCheckState, groupItems, riskLabel, selectable, setGroup, type ItemGroup } from "./compareModel";

export interface CompareListHost {
  /** The checked items changed. */
  selectionChanged(): void;
  /** An item was clicked (or reached with the arrow keys): its details are shown. */
  activated(item: DiffItem): void;
}

export class CompareList {
  readonly element: HTMLElement;
  private readonly body = h("div", { class: "compare-groups", tabindex: "0", role: "tree", "aria-label": "Compared objects" });
  private readonly footer = h("div", { class: "compare-skipped" });
  private items: DiffItem[] = [];
  private skipped: SkippedObject[] = [];
  selection = new Set<string>();
  private active: string | null = null;
  private filter = "";
  private showIdentical = false;
  private readonly collapsed = new Set<DiffStatus>();

  constructor(private readonly host: CompareListHost) {
    this.element = h("div", { class: "compare-list" }, this.body, this.footer);
    this.body.addEventListener("keydown", (event) => this.handleKey(event));
  }

  setItems(items: DiffItem[], selection: Set<string>, skipped: SkippedObject[]): void {
    this.items = items;
    this.selection = selection;
    this.skipped = skipped;
    if (this.active && !items.some((item) => item.id === this.active)) this.active = null;
    this.render();
  }

  setFilter(filter: string): void {
    this.filter = filter;
    this.render();
  }

  setShowIdentical(show: boolean): void {
    this.showIdentical = show;
    this.render();
  }

  activeId(): string | null {
    return this.active;
  }

  /** Shows `id`'s details and scrolls its row into view (a script warning was clicked, say). */
  activate(id: string): void {
    const item = this.items.find((candidate) => candidate.id === id);
    if (!item) return;
    if (item.status === "identical" && !this.showIdentical) this.showIdentical = true;
    this.collapsed.delete(item.status);
    this.active = id;
    this.render();
    this.body.querySelector<HTMLElement>(".compare-item.active")?.scrollIntoView({ block: "nearest" });
    this.host.activated(item);
  }

  /** The first item shown, for the details pane after a compare. */
  firstShown(): DiffItem | null {
    return this.shownItems()[0] ?? null;
  }

  private groups(): ItemGroup[] {
    return groupItems(this.items, this.filter, this.showIdentical);
  }

  private shownItems(): DiffItem[] {
    return this.groups().flatMap((group) => (this.collapsed.has(group.status) ? [] : group.items));
  }

  private render(): void {
    const groups = this.groups();
    const elements = groups.map((group) => this.groupElement(group));
    if (elements.length === 0) {
      const message = this.items.length === 0 ? "Nothing compared yet" : this.filter ? "No object matches the filter" : "Both sides are the same";
      elements.push(h("div", { class: "compare-empty" }, message));
    }
    this.body.replaceChildren(...elements);
    this.footer.hidden = this.skipped.length === 0;
    this.footer.replaceChildren(
      h("details", {}, h("summary", {}, `Not compared (${this.skipped.length})`), h("ul", {}, ...this.skipped.map((item) => skippedRow(item)))),
    );
  }

  private groupElement(group: ItemGroup): HTMLElement {
    const collapsed = this.collapsed.has(group.status);
    const state = groupCheckState(group, this.selection);
    const checkbox = h("input", { type: "checkbox", "aria-label": `Select all: ${group.label}` });
    checkbox.checked = state === "all";
    checkbox.indeterminate = state === "some";
    checkbox.disabled = group.status === "identical" || group.items.length === 0;
    checkbox.addEventListener("click", (event) => event.stopPropagation());
    checkbox.addEventListener("change", () => {
      this.selection = setGroup(this.selection, group, checkbox.checked);
      this.render();
      this.host.selectionChanged();
    });
    const count = group.items.length === group.total ? String(group.total) : `${group.items.length} / ${group.total}`;
    const header = h(
      "div",
      { class: `compare-group-header status-${group.status}`, role: "button", "aria-expanded": String(!collapsed) },
      h("span", { class: `compare-chevron${collapsed ? "" : " open"}` }, icon("chevron")),
      checkbox,
      h("span", { class: "compare-group-label" }, group.label),
      h("span", { class: "compare-count" }, count),
    );
    header.addEventListener("click", () => {
      if (collapsed) this.collapsed.delete(group.status);
      else this.collapsed.add(group.status);
      this.render();
    });
    return h("div", { class: "compare-group" }, header, ...(collapsed ? [] : group.items.map((item) => this.itemElement(item))));
  }

  private itemElement(item: DiffItem): HTMLElement {
    const checkbox = h("input", { type: "checkbox", "aria-label": `Include ${item.name}` });
    checkbox.checked = this.selection.has(item.id);
    checkbox.disabled = !selectable(item);
    checkbox.addEventListener("click", (event) => event.stopPropagation());
    checkbox.addEventListener("change", () => {
      if (checkbox.checked) this.selection.add(item.id);
      else this.selection.delete(item.id);
      this.render();
      this.host.selectionChanged();
    });
    const risk = displayRisk(item);
    const row = h(
      "div",
      { class: `compare-item${item.id === this.active ? " active" : ""}`, role: "treeitem", title: item.id },
      checkbox,
      icon(item.kind === "view" ? "view" : "table", "compare-kind"),
      h("span", { class: "compare-name" }, item.name),
      h("span", { class: "compare-changes" }, changeSummary(item)),
      risk !== "none" ? h("span", { class: `risk-badge risk-${risk}` }, riskLabel(risk)) : null,
    );
    row.addEventListener("click", () => {
      this.body.focus();
      this.activate(item.id);
    });
    return row;
  }

  private handleKey(event: KeyboardEvent): void {
    const shown = this.shownItems();
    if (shown.length === 0) return;
    const index = shown.findIndex((item) => item.id === this.active);
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const next = event.key === "ArrowDown" ? Math.min(index + 1, shown.length - 1) : Math.max(index - 1, 0);
      this.activate(shown[next].id);
    } else if (event.key === " " && index >= 0 && selectable(shown[index])) {
      event.preventDefault();
      const id = shown[index].id;
      if (this.selection.has(id)) this.selection.delete(id);
      else this.selection.add(id);
      this.render();
      this.host.selectionChanged();
    }
  }
}

function skippedRow(item: SkippedObject): HTMLElement {
  return h("li", {}, h("span", { class: "compare-skipped-name" }, `${item.kind} ${item.name}`), h("span", { class: "compare-skipped-reason" }, item.reason));
}
