/**
 * The "Columns" popover of a data grid: filter box, one checkbox per column (name and type),
 * Show all / Hide all. At least one column always stays visible.
 */
import { h, icon } from "../../core/dom";
import { placeInViewport } from "../../core/placement";
import type { ColumnInfo } from "../../core/contract";
import { hideAllButFirst, matchColumns, setColumnHidden, visibleColumnIndexes } from "./columnVisibility";

/** What the popover needs from a grid. */
export interface ColumnsTarget {
  columns(): ColumnInfo[];
  hiddenColumns(): string[];
  setHiddenColumns(hidden: string[]): void;
  filteredColumns(): Set<string>;
}

type Anchor = HTMLElement | { x: number; y: number };

interface OpenPopover {
  element: HTMLElement;
  anchor: Anchor;
  close: () => void;
}

let active: OpenPopover | null = null;

export function closeColumnsPopover(): void {
  active?.close();
}

/** Opens the popover below `anchor` (a toolbar button) or at a point; a second click on the same button closes it. */
export function openColumnsPopover(target: ColumnsTarget, anchor: Anchor): void {
  if (active && active.anchor === anchor) {
    active.close();
    return;
  }
  active?.close();
  if (target.columns().length === 0) return;

  const filter = h("input", { class: "columns-filter", placeholder: "Filter columns", spellcheck: "false", "aria-label": "Filter columns" });
  const list = h("div", { class: "columns-list", role: "group", "aria-label": "Columns" });
  const count = h("span", { class: "columns-count" });
  const showAll = h("button", { class: "link-button", type: "button" }, "Show all");
  const hideAll = h("button", { class: "link-button", type: "button" }, "Hide all");
  const popover = h(
    "div",
    { class: "columns-popover", role: "dialog", "aria-label": "Show or hide columns" },
    h("div", { class: "columns-filter-row" }, icon("search"), filter),
    list,
    h("div", { class: "columns-actions" }, showAll, hideAll, h("span", { class: "spacer" }), count),
    h("div", { class: "columns-hint" }, "Exports include hidden columns."),
  );

  const names = () => target.columns().map((column) => column.name);
  const apply = (hidden: string[]) => {
    target.setHiddenColumns(hidden);
    render();
  };
  const render = () => {
    const columns = target.columns();
    const hidden = new Set(target.hiddenColumns());
    const visibleCount = visibleColumnIndexes(names(), hidden).length;
    const filtered = target.filteredColumns();
    const matches = matchColumns(columns, filter.value);
    list.replaceChildren(
      ...matches.map((index) => {
        const column = columns[index];
        const shown = !hidden.has(column.name);
        const input = h("input", { type: "checkbox", checked: shown, disabled: shown && visibleCount <= 1, "aria-label": `Show ${column.name}` });
        input.addEventListener("change", () => apply(setColumnHidden(names(), target.hiddenColumns(), column.name, !input.checked)));
        return h(
          "label",
          { class: `columns-item${shown ? "" : " is-hidden"}`, title: shown && visibleCount <= 1 ? "At least one column stays visible" : column.name },
          input,
          h("span", { class: "columns-name" }, column.name),
          filtered.has(column.name) ? h("span", { class: "columns-filtered", title: "This column has an active filter" }, icon("filter")) : null,
          h("span", { class: "columns-type" }, column.typeName || (column.hints.vectorDims != null ? `vector(${column.hints.vectorDims})` : column.kind.kind)),
        );
      }),
    );
    if (matches.length === 0) list.append(h("div", { class: "columns-empty" }, "No column matches"));
    count.textContent = `${visibleCount} of ${columns.length} shown`;
    showAll.disabled = hidden.size === 0;
    hideAll.disabled = visibleCount <= 1;
  };
  showAll.addEventListener("click", () => apply([]));
  hideAll.addEventListener("click", () => apply(hideAllButFirst(names())));
  filter.addEventListener("input", render);

  const onPointerDown = (event: PointerEvent) => {
    const node = event.target as Node;
    if (popover.contains(node)) return;
    // A click on the anchor button toggles; let its click handler close the popover.
    if (anchor instanceof HTMLElement && anchor.contains(node)) return;
    close();
  };
  const onKey = (event: KeyboardEvent) => {
    if (event.key !== "Escape") return;
    event.stopPropagation();
    close();
  };
  const close = () => {
    popover.remove();
    document.removeEventListener("pointerdown", onPointerDown, true);
    document.removeEventListener("keydown", onKey, true);
    window.removeEventListener("blur", close);
    if (active?.element === popover) active = null;
  };
  // Typing in the filter must not reach the grid or the app shortcuts.
  popover.addEventListener("keydown", (event) => event.stopPropagation());
  document.addEventListener("pointerdown", onPointerDown, true);
  document.addEventListener("keydown", onKey, true);
  window.addEventListener("blur", close);

  render();
  document.body.append(popover);
  place(popover, anchor);
  active = { element: popover, anchor, close };
  filter.focus();
}

function place(popover: HTMLElement, anchor: Anchor): void {
  const point = anchor instanceof HTMLElement ? anchorPoint(anchor, popover) : anchor;
  // Untransformed size: the opening animation scales the popover, so its bounding box is a few percent short.
  const placed = placeInViewport(point.x, point.y, popover.offsetWidth, popover.offsetHeight, window.innerWidth, window.innerHeight);
  popover.style.left = `${placed.left}px`;
  popover.style.top = `${placed.top}px`;
}

/** Below the button, right-aligned with it when it sits near the right edge. */
function anchorPoint(anchor: HTMLElement, popover: HTMLElement): { x: number; y: number } {
  const rect = anchor.getBoundingClientRect();
  const width = popover.offsetWidth;
  const x = rect.left + width > window.innerWidth - 4 ? rect.right - width : rect.left;
  return { x, y: rect.bottom + 4 };
}
