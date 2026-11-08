/** The grid's filter row: one box per column with an operator button, applied after a short pause. */
import { columnCondition, FILTER_HELP, FILTER_OPERATORS, parseFilterText, withOperator, type ColumnFilters } from "./columnFilter";
import { isJsonKind, KIND_HINTS } from "./cellValue";
import { OPERATOR_MARKS, TEXT_KINDS } from "./gridModel";
import type { ColumnInfo, FilterOp } from "../../core/contract";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { h, icon } from "../../core/dom";

const FILTER_DEBOUNCE_MS = 350;

export interface FilterRowSource {
  columns: ColumnInfo[];
  /** Indexes of the visible columns, in order. */
  displayed: number[];
  filters: ColumnFilters;
  /** Set while filtering is unavailable; the text is the reason. */
  paused: string | null;
  apply(column: string, text: string): void;
}

export function filterRow(source: FilterRowSource): HTMLTableRowElement {
  const { paused } = source;
  const cells = source.displayed.map((index) => {
    const column = source.columns[index];
    if (column.hints.vectorDims != null) return h("th", { class: "filter-cell" });
    const text = source.filters[column.name] ?? "";
    const condition = text ? columnCondition(column, text) : null;
    const invalid = condition instanceof Error;
    const input = h("input", {
      class: `column-filter${text ? " active" : ""}${invalid ? " invalid" : ""}`,
      value: text,
      placeholder:
        TEXT_KINDS.has(column.kind.kind) || isJsonKind(column.kind) || column.kind.kind === "bytes" ? "filter" : (KIND_HINTS[column.kind.kind] ?? "filter"),
      title: paused ?? (invalid ? `${condition.message} (not applied)` : FILTER_HELP),
      spellcheck: "false",
      disabled: paused !== null,
      "data-column": column.name,
    });
    let pending: ReturnType<typeof setTimeout> | null = null;
    const apply = (value: string) => {
      if (pending) clearTimeout(pending);
      pending = null;
      source.apply(column.name, value);
    };
    input.addEventListener("keydown", (event) => {
      event.stopPropagation();
      if (event.key === "Enter") apply(input.value);
      else if (event.key === "Escape") apply("");
    });
    // Filters apply as you type, after a short pause, so Enter is optional.
    input.addEventListener("input", () => {
      if (pending) clearTimeout(pending);
      pending = setTimeout(() => apply(input.value), FILTER_DEBOUNCE_MS);
    });
    const op = parseFilterText(text, column.kind)?.op ?? null;
    const opButton = h(
      "button",
      { class: `filter-op${op ? " active" : ""}`, type: "button", title: "Choose an operator", disabled: paused !== null, tabIndex: -1 },
      op ? OPERATOR_MARKS[op] : icon("filter"),
    );
    opButton.addEventListener("click", (event) => showContextMenu(event, operatorMenu(column, input, op, apply)));
    return h("th", { class: "filter-cell" }, h("div", { class: "filter-wrap" }, opButton, input));
  });
  return h(
    "tr",
    { class: `filter-row${paused ? " paused" : ""}` },
    h("th", { class: "row-number filter-cell", title: paused ?? FILTER_HELP }, icon("filter")),
    ...cells,
  );
}

function operatorMenu(column: ColumnInfo, input: HTMLInputElement, current: FilterOp | null, apply: (text: string) => void): MenuItem[] {
  const plainOp = TEXT_KINDS.has(column.kind.kind) || isJsonKind(column.kind) || column.kind.kind === "bytes" ? "contains" : "eq";
  return FILTER_OPERATORS.map((item) => ({
    label: `${item.label}   ${item.op === plainOp ? "x" : item.syntax}`,
    icon: item.op === current ? "check" : undefined,
    run: () => {
      const text = withOperator(input.value, item.op);
      input.value = text;
      const parsed = parseFilterText(text, column.kind);
      const complete =
        parsed !== null && (parsed.values.length === 0 ? item.op === "isNull" || item.op === "isNotNull" : parsed.values.every((value) => value !== ""));
      if (complete) {
        apply(text);
        return;
      }
      // Wait for the value: put the cursor where it goes.
      input.focus();
      const at = item.op === "endsWith" ? Math.max(0, text.length - 1) : text.length;
      input.setSelectionRange(at, at);
    },
  }));
}
