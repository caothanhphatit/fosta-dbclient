/** A few DOM builders the ClickHouse views share. */
import { h } from "../../core/dom";

export function button(label: string, onClick: () => void, className = "tool-button", title = label): HTMLButtonElement {
  const element = h("button", { class: className, type: "button", title }, label);
  element.addEventListener("click", onClick);
  return element;
}

export function emptyHint(text: string): HTMLElement {
  return h("div", { class: "empty-hint" }, text);
}

export function noteLine(text: string | null | undefined): HTMLElement | null {
  return text ? h("div", { class: "ch-note" }, text) : null;
}

export interface TableOptions {
  /** Cell text per column name; the default shows the text as it is. */
  render?: (column: string, value: string | null, row: (string | null)[]) => HTMLElement | string;
  /** A last column with controls per row. */
  actions?: (row: (string | null)[]) => HTMLElement | null;
  actionsHeader?: string;
  rowClass?: (row: (string | null)[]) => string;
}

/** A grid of text cells with a sticky header. */
export function dataTable(columns: string[], rows: (string | null)[][], options: TableOptions = {}): HTMLElement {
  const head = h("tr", {}, ...columns.map((name) => h("th", {}, name)), options.actions ? h("th", {}, options.actionsHeader ?? "") : null);
  const body = rows.map((row) =>
    h(
      "tr",
      { class: options.rowClass?.(row) ?? "" },
      ...columns.map((name, index) => {
        const value = row[index] ?? null;
        const content = options.render ? options.render(name, value, row) : (value ?? "");
        return h("td", { class: value === null ? "ch-null" : "" }, content);
      }),
      options.actions ? h("td", { class: "ch-actions" }, options.actions(row)) : null,
    ),
  );
  return h("table", { class: "ch-table" }, h("thead", {}, head), h("tbody", {}, ...body));
}

/** Tabs that swap a body: `show(index)` draws the page of that tab. */
export function tabBar(labels: string[], onSelect: (index: number) => void): { element: HTMLElement; select(index: number): void } {
  const buttons = labels.map((label, index) => {
    const element = h("button", { class: "ch-tab", type: "button", role: "tab" }, label);
    element.addEventListener("click", () => select(index));
    return element;
  });
  const select = (index: number): void => {
    buttons.forEach((element, position) => {
      element.classList.toggle("active", position === index);
      element.setAttribute("aria-selected", String(position === index));
    });
    onSelect(index);
  };
  return { element: h("div", { class: "ch-tabs", role: "tablist" }, ...buttons), select };
}
