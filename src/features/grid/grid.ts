/**
 * The data grid: typed rows (`Value[]`, NULL explicit), virtualised rendering, per-column filter boxes, inline editing
 * per column kind (validated with `parseInput` before an edit is accepted), copy / paste, and pending edits that become
 * contract `RowChange`s (`changeSet.ts`). Query results and design tables use it read-only.
 */
import { buildChanges, isEditableColumn, type BuiltChange } from "./changeSet";
import { openCellEditor } from "./cellEditor";
import { filterRow } from "./gridFilterRow";
import { pasteBlock } from "./gridPaste";
import { beginBoolEdit, beginTextEdit, type InlineEditHost } from "./inlineEdit";
import { isJsonKind } from "./cellValue";
import { matchesFilter, type ColumnFilters } from "./columnFilter";
import { openColumnsPopover } from "./columnsPopover";
import { nearestVisibleColumn, normalizeHidden, setColumnHidden, stepVisibleColumn, visibleColumnIndexes } from "./columnVisibility";
import type { ColumnInfo, Field, Value } from "../../core/contract";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { toast } from "../../core/dialogs";
import { clamp, copyText, h, icon } from "../../core/dom";
import { EXPORT_LABELS, exportRows, type ExportFormat } from "../importExport/exporters";
import { density, onDensityChange } from "../../core/density";
import { displayValue, isNumericKind, valuesEqual } from "../../core/values";
import { sortIndexes } from "../../core/valueOrder";
import {
  EXPORT_FORMATS,
  MOVES,
  NULL,
  TEXT_KINDS,
  TYPE_LABELS,
  preview,
  rowState,
  rowText,
  typeKind,
  gridSetup,
  type GridRow,
  type GridSetup,
  type Position,
  type SortState,
} from "./gridModel";
export { gridSetup, type CellView, type GridSetup, type SortState } from "./gridModel";
import "./grid-virtual.css";
import "./grid-tools.css";

/** Whether two column lists have the same names in the same order (a streaming reload of the same result set). */
function sameColumnNames(a: ColumnInfo[], b: ColumnInfo[]): boolean {
  return a.length === b.length && a.every((column, index) => column.name === b[index].name);
}

/** Whether rows about to load show the same cells as the rows shown now (a reload with nothing new). */
function sameRows(shown: readonly GridRow[], next: readonly Value[][]): boolean {
  if (shown.length !== next.length) return false;
  return shown.every((row, index) => {
    const cells = next[index];
    return row.current === cells || (row.current.length === cells.length && row.current.every((cell, column) => valuesEqual(cell, cells[column])));
  });
}

const TOOLTIP_THRESHOLD = 60;
/** Rows rendered beyond the viewport on each side. */
const OVERSCAN = 12;
/** Window bounds snap to this many rows (even, so zebra striping keeps its parity). */
const WINDOW_CHUNK = 16;
/** Viewport assumed before the grid is attached and sized. */
const DEFAULT_VIEWPORT = 800;
/** Row height (22px compact, 26px comfortable cell incl. its 1px border), refined by measuring once. */
let rowHeight = density() === "compact" ? 22 : 26;
let rowHeightMeasured = false;
onDensityChange(() => {
  rowHeight = density() === "compact" ? 22 : 26;
  rowHeightMeasured = false;
});

export class DataGrid {
  readonly element: HTMLElement;
  private setup: GridSetup = gridSetup([], []);
  private rows: GridRow[] = [];
  private tbody: HTMLTableSectionElement = h("tbody");
  private selection: Position | null = null;
  private editing = false;
  private filters: ColumnFilters = {};
  /** The header sort of a `clientSort` grid (kept across `load` while the column exists). */
  private clientSort: SortState | null = null;
  /** Changes whenever a row is added, removed or edited, or other rows load: the sort cache is then stale. */
  private rowGeneration = 0;
  private sortCache: { generation: number; column: number; descending: boolean; order: number[] } | null = null;
  private hidden: string[] = [];
  /** Indexes of the columns that are drawn, in order (hidden columns left out). */
  private displayed: number[] = [];
  private visible: number[] = [];
  /** Row index -> position in `visible`. */
  private visiblePosition = new Map<number, number>();
  private pendingFilterFocus: string | null = null;
  private readonly onChange: () => void;
  // Virtualization: only visible positions [windowStart, windowEnd) have <tr> elements.
  private windowStart = 0;
  private windowEnd = 0;
  private topSpacer: HTMLTableCellElement = h("td");
  private bottomSpacer: HTMLTableCellElement = h("td");
  private headerCells: HTMLElement[] = [];
  private columnWidths: number[] = [];
  private frame: number | null = null;
  private editingRow: number | null = null;
  private commitEdit: (() => void) | null = null;
  private resizeObserver: ResizeObserver | null = null;
  private disposed = false;

  constructor(onChange: () => void = () => undefined) {
    this.onChange = onChange;
    this.element = h("div", { class: "grid", tabIndex: 0 });
    this.element.addEventListener("scroll", () => this.scheduleWindow(), { passive: true });
    if (typeof ResizeObserver !== "undefined") {
      this.resizeObserver = new ResizeObserver(() => this.scheduleWindow());
      this.resizeObserver.observe(this.element);
    }
    this.element.addEventListener("keydown", (event) => this.handleKey(event));
    this.element.addEventListener("mousedown", (event) => this.handlePointer(event));
    this.element.addEventListener("dblclick", (event) => this.handleDoubleClick(event));
    this.element.addEventListener("contextmenu", (event) => this.handleContextMenu(event));
  }

  load(setup: GridSetup): void {
    const sameColumns = sameColumnNames(this.setup.columns, setup.columns);
    this.setup = setup;
    // Rows streaming into a result grid reload it: the filters typed and the sort picked there stay.
    if (!(setup.clientFilter && sameColumns)) this.filters = { ...setup.columnFilters };
    if (!setup.clientSort || !setup.columns.some((column) => column.name === this.clientSort?.column)) this.clientSort = null;
    this.applyHidden(setup.hiddenColumns);
    // A reload with the same rows (a result list refreshed while nothing changed) keeps the sorted order.
    if (!sameRows(this.rows, setup.rows)) this.rowGeneration += 1;
    this.rows = setup.rows.map((row, index) => ({
      original: row,
      current: [...row],
      deleted: false,
      version: setup.rowVersions?.[index] ?? null,
      touched: new Set<number>(),
      edited: false,
      removed: false,
      element: null,
    }));
    this.selection = null;
    this.render();
    this.onChange();
    this.selectFirstVisible();
  }

  selectedRow(): Value[] | null {
    return this.selection ? (this.rows[this.selection.row]?.current ?? null) : null;
  }

  columns(): ColumnInfo[] {
    return this.setup.columns;
  }

  hiddenColumns(): string[] {
    return [...this.hidden];
  }

  /** Column names with an active per-column filter. */
  filteredColumns(): Set<string> {
    return new Set(Object.keys(this.filters));
  }

  /** Hides exactly these columns (at least one column stays visible); data and pending edits are kept. */
  setHiddenColumns(hidden: string[]): void {
    const next = normalizeHidden(
      this.setup.columns.map((column) => column.name),
      hidden,
    );
    if (next.length === this.hidden.length && next.every((name, index) => name === this.hidden[index])) return;
    this.commitEdit?.();
    this.applyHidden(next);
    if (this.selection) this.selection = { ...this.selection, column: nearestVisibleColumn(this.displayed, this.selection.column) };
    const scrollTop = this.element.scrollTop;
    this.render();
    this.element.scrollTop = scrollTop;
    this.updateWindow(false);
    this.setup.onHiddenColumns?.(this.hiddenColumns());
  }

  /** Opens the selected cell in the large editor (Shift+Enter). */
  openSelectedInEditor(): void {
    if (!this.selection) {
      toast("Select a cell first");
      return;
    }
    this.openEditor(this.selection);
  }

  /** Opens a column of the selected row in the large editor (from the record panel). */
  openEditorForColumn(column: number): void {
    if (!this.selection) return;
    this.openEditor({ row: this.selection.row, column });
  }

  private applyHidden(hidden: string[]): void {
    const names = this.setup.columns.map((column) => column.name);
    this.hidden = normalizeHidden(names, hidden);
    this.displayed = visibleColumnIndexes(names, this.hidden);
  }

  private hideColumn(column: number): void {
    const names = this.setup.columns.map((meta) => meta.name);
    this.setHiddenColumns(setColumnHidden(names, this.hidden, names[column], true));
  }

  /** Rows currently shown (after client-side filters), as edited. */
  pageRows(): Value[][] {
    return this.visible.map((index) => this.rows[index].current);
  }

  exportPage(format: ExportFormat): string {
    return exportRows(format, this.setup.columns, this.pageRows(), this.setup.tableName, this.setup.dialect);
  }

  private selectFirstVisible(): void {
    const first = this.visible[0];
    if (first === undefined) this.setup.onSelect?.(null);
    else this.select({ row: first, column: this.selection?.column ?? this.displayed[0] ?? 0 });
  }

  private get filtersEnabled(): boolean {
    return this.setup.clientFilter || this.setup.onColumnFilter !== null;
  }

  private applyColumnFilter(column: string, text: string): void {
    const next = { ...this.filters };
    if (text.trim()) next[column] = text;
    else delete next[column];
    if (JSON.stringify(next) === JSON.stringify(this.filters)) return;
    this.filters = next;
    this.pendingFilterFocus = column;
    if (this.setup.onColumnFilter) {
      this.setup.onColumnFilter(next);
      return;
    }
    this.selection = null;
    this.render();
    this.selectFirstVisible();
  }

  private computeVisible(): number[] {
    const keep = (row: GridRow) => !row.removed;
    const active = this.setup.clientFilter
      ? this.setup.columns.map((column, index) => ({ column, index, text: this.filters[column.name] ?? "" })).filter(({ text }) => text.trim() !== "")
      : [];
    const visible = (rowIndex: number) => {
      const row = this.rows[rowIndex];
      return keep(row) && active.every(({ column, index, text }) => matchesFilter(row.current[index] ?? NULL, column, text));
    };
    return this.sortedOrder().filter(visible);
  }

  /**
   * Every row index in the order of the header sort (`clientSort` grids), else as loaded. The sorted order is kept
   * until the rows or the sort change, so a render (a filter typed, a column hidden, a reload with the same rows) does
   * not sort again.
   */
  private sortedOrder(): number[] {
    const sort = this.clientSort;
    const column = sort ? this.setup.columns.findIndex((item) => item.name === sort.column) : -1;
    if (!sort || column < 0) return this.rows.map((_, index) => index);
    const cached = this.sortCache;
    if (cached && cached.generation === this.rowGeneration && cached.column === column && cached.descending === sort.descending) return cached.order;
    const order = sortIndexes(
      this.rows.map((_, index) => index),
      (index) => this.rows[index].current[column],
      sort.descending,
    );
    this.sortCache = { generation: this.rowGeneration, column, descending: sort.descending, order };
    return order;
  }

  /** Header click on a `clientSort` grid: ascending, then descending, then the order the rows came in. */
  private toggleClientSort(column: string): void {
    const current = this.clientSort?.column === column ? this.clientSort : null;
    this.setClientSort(current === null ? { column, descending: false } : current.descending ? null : { column, descending: true });
  }

  private setClientSort(sort: SortState | null): void {
    this.clientSort = sort;
    this.selection = null;
    this.render();
    this.selectFirstVisible();
  }

  discard(): void {
    this.load(this.setup);
  }

  hasChanges(): boolean {
    return this.rows.some((row) => rowState(row) !== "clean");
  }

  /** Pending edits by kind, for the bar under the grid. */
  changeSummary(): { updated: number; inserted: number; deleted: number } {
    const summary = { updated: 0, inserted: 0, deleted: 0 };
    for (const row of this.rows) {
      const state = rowState(row);
      if (state === "modified") summary.updated += 1;
      else if (state === "new") summary.inserted += 1;
      else if (state === "deleted") summary.deleted += 1;
    }
    return summary;
  }

  /** The pending changes as contract `RowChange`s, each with the index of its grid row. */
  changes(): BuiltChange[] {
    return buildChanges(this.setup.columns, this.setup.keyColumns, this.rows).filter(({ row }) => !this.rows[row].removed);
  }

  /** A row as loaded (every column), for the conflict dialog; null for a new row. */
  loadedFields(index: number): Field[] | null {
    const original = this.rows[index]?.original;
    return original ? this.setup.columns.map((column, position) => ({ column: column.name, value: original[position] ?? NULL })) : null;
  }

  /**
   * Replaces a row with what the server has now, dropping its pending edit (the conflict dialog's "Take theirs" and the
   * refresh after an "Overwrite"). `values` null: the row no longer exists and leaves the grid.
   */
  replaceRow(index: number, values: Value[] | null, version: Value | null): void {
    const row = this.rows[index];
    if (!row) return;
    if (values === null) {
      row.removed = true;
      if (this.selection?.row === index) this.selection = null;
      this.render();
    } else {
      Object.assign(row, { original: values, current: [...values], deleted: false, version, touched: new Set<number>(), edited: false });
      this.refreshRow(index);
      if (this.selection?.row === index) this.setup.onSelect?.(row.current);
    }
    this.onChange();
  }

  addRow(): void {
    if (!this.setup.editable) return;
    const row: GridRow = {
      original: null,
      current: this.setup.columns.map(() => NULL),
      deleted: false,
      version: null,
      touched: new Set<number>(),
      edited: true,
      removed: false,
      element: null,
    };
    this.rowGeneration += 1;
    const index = this.rows.push(row) - 1;
    this.visiblePosition.set(index, this.visible.push(index) - 1);
    this.element.querySelector(".grid-empty")?.remove();
    this.updateWindow(true);
    this.select({ row: index, column: this.displayed[0] ?? 0 });
    this.onChange();
  }

  deleteSelectedRow(): void {
    if (!this.setup.editable || !this.selection) return;
    const index = this.selection.row;
    const row = this.rows[index];
    if (row.original) {
      row.deleted = !row.deleted;
      row.edited = true;
      this.refreshRow(index);
    } else {
      this.rows.splice(index, 1);
      this.rowGeneration += 1;
      this.selection = null;
      this.render();
    }
    this.onChange();
  }

  private render(): void {
    this.visible = this.computeVisible();
    this.visiblePosition = new Map(this.visible.map((index, position) => [index, position]));
    this.headerCells = [h("th", { class: "row-number" }), ...this.displayed.map((column) => this.headerCell(this.setup.columns[column], column))];
    this.columnWidths = this.headerCells.map(() => 0);
    const header = h("tr", {}, ...this.headerCells);
    const filterRow = this.filtersEnabled ? this.filterRow() : null;
    this.tbody = h("tbody");
    for (const row of this.rows) row.element = null;
    this.editingRow = null;
    this.commitEdit = null;
    this.windowStart = 0;
    this.windowEnd = 0;
    const span = this.displayed.length + 1;
    this.topSpacer = h("td", { colspan: span });
    this.bottomSpacer = h("td", { colspan: span });
    const table = h(
      "table",
      { class: "grid-table" },
      h("thead", {}, header, filterRow),
      h("tbody", { class: "grid-spacer", "aria-hidden": "true" }, h("tr", {}, this.topSpacer)),
      this.tbody,
      h("tbody", { class: "grid-spacer", "aria-hidden": "true" }, h("tr", {}, this.bottomSpacer)),
    );
    const empty = this.visible.length === 0 ? h("div", { class: "grid-empty" }, this.emptyMessage()) : null;
    this.element.replaceChildren(table, ...(empty ? [empty] : []));
    this.updateWindow(true);
    this.restoreFilterFocus();
  }

  /** The data index of the first row in view (under the header); -1 without rows. */
  topRow(): number {
    if (this.visible.length === 0) return -1;
    return this.visible[clamp(Math.floor(this.element.scrollTop / rowHeight), 0, this.visible.length - 1)];
  }

  /** Scrolls so the row at data index `index` is the first in view. */
  scrollToRow(index: number): void {
    const position = this.visiblePosition.get(index);
    if (position === undefined) return;
    this.element.scrollTop = position * rowHeight;
    this.updateWindow(false);
  }

  /** Stops observing the grid's size; call when the view that owns the grid closes or replaces it. */
  dispose(): void {
    this.disposed = true;
    this.resizeObserver?.disconnect();
    this.resizeObserver = null;
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    this.frame = null;
  }

  private scheduleWindow(): void {
    if (this.frame !== null || this.disposed) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = null;
      this.updateWindow(false);
    });
  }

  /** Visible positions that should have rows given the current scroll offset. */
  private windowRange(): { start: number; end: number } {
    const total = this.visible.length;
    const viewport = this.element.clientHeight || DEFAULT_VIEWPORT;
    const first = Math.floor(this.element.scrollTop / rowHeight);
    const last = first + Math.ceil(viewport / rowHeight);
    const start = Math.max(0, Math.floor((first - OVERSCAN) / WINDOW_CHUNK) * WINDOW_CHUNK);
    const end = Math.min(total, Math.ceil((last + OVERSCAN) / WINDOW_CHUNK) * WINDOW_CHUNK);
    return { start: Math.min(start, end), end };
  }

  /** Renders the rows in view (plus overscan) and sizes the spacers for the rest. */
  private updateWindow(force: boolean): void {
    const { start, end } = this.windowRange();
    if (force || start !== this.windowStart || end !== this.windowEnd) this.applyWindow(start, end);
    this.topSpacer.style.height = `${this.windowStart * rowHeight}px`;
    this.bottomSpacer.style.height = `${(this.visible.length - this.windowEnd) * rowHeight}px`;
    if (!rowHeightMeasured && this.element.isConnected) this.measureRowHeight();
  }

  private applyWindow(start: number, end: number): void {
    // An open editor would lose focus with its row; commit it first.
    if (this.editingRow !== null) {
      const position = this.visiblePosition.get(this.editingRow);
      if (position === undefined || position < start || position >= end) this.commitEdit?.();
    }
    const oldStart = this.windowStart;
    const oldEnd = this.windowEnd;
    const keepStart = Math.max(start, oldStart);
    const keepEnd = Math.min(end, oldEnd);
    for (let position = oldStart; position < oldEnd; position += 1) {
      if (position >= keepStart && position < keepEnd) continue;
      const row = this.rows[this.visible[position]];
      if (!row) continue;
      row.element?.remove();
      row.element = null;
    }
    const build = (from: number, to: number): DocumentFragment => {
      const fragment = document.createDocumentFragment();
      for (let position = from; position < to; position += 1) {
        const index = this.visible[position];
        const row = this.rows[index];
        fragment.append(row.element ?? this.renderRow(row, index));
      }
      return fragment;
    };
    if (keepStart < keepEnd) {
      this.tbody.prepend(build(start, keepStart));
      this.tbody.append(build(keepEnd, end));
    } else {
      this.tbody.replaceChildren(build(start, end));
    }
    this.windowStart = start;
    this.windowEnd = end;
    this.lockColumnWidths();
  }

  /** Columns only grow as new rows scroll in, so the layout does not jitter. */
  private lockColumnWidths(): void {
    if (!this.element.isConnected || this.windowEnd === this.windowStart) return;
    const widths = this.headerCells.map((cell) => Math.ceil(cell.getBoundingClientRect().width));
    widths.forEach((width, column) => {
      if (width <= this.columnWidths[column]) return;
      this.columnWidths[column] = width;
      this.headerCells[column].style.minWidth = `${width}px`;
    });
  }

  private measureRowHeight(): void {
    const first = this.tbody.rows[0];
    const height = first?.getBoundingClientRect().height ?? 0;
    if (height <= 0) return;
    rowHeightMeasured = true;
    if (Math.abs(height - rowHeight) < 0.01) return;
    rowHeight = height;
    this.updateWindow(true);
  }

  /** Scrolls a row that is outside the rendered window into it. */
  private ensureRendered(index: number): void {
    if (this.rows[index]?.element) return;
    const position = this.visiblePosition.get(index);
    if (position === undefined) return;
    const top = position * rowHeight;
    const header = this.element.querySelector("thead")?.getBoundingClientRect().height ?? 0;
    if (top < this.element.scrollTop) this.element.scrollTop = top;
    else this.element.scrollTop = Math.max(0, top + rowHeight + header - (this.element.clientHeight || DEFAULT_VIEWPORT));
    this.updateWindow(false);
    if (!this.rows[index]?.element) {
      // Scrolling was clamped (e.g. a detached grid); render a window around the row.
      const start = Math.max(0, Math.floor((position - OVERSCAN) / WINDOW_CHUNK) * WINDOW_CHUNK);
      this.applyWindow(start, Math.min(this.visible.length, start + WINDOW_CHUNK * 4));
      this.topSpacer.style.height = `${this.windowStart * rowHeight}px`;
      this.bottomSpacer.style.height = `${(this.visible.length - this.windowEnd) * rowHeight}px`;
    }
  }

  private emptyMessage(): string {
    if (this.setup.columns.length === 0) return "No data";
    return Object.keys(this.filters).length && !this.setup.filtersPaused ? "No rows match the column filters" : "No rows";
  }

  private filterRow(): HTMLTableRowElement {
    return filterRow({
      columns: this.setup.columns,
      displayed: this.displayed,
      filters: this.filters,
      paused: this.setup.filtersPaused,
      apply: (column, text) => this.applyColumnFilter(column, text),
    });
  }

  private restoreFilterFocus(): void {
    const column = this.pendingFilterFocus;
    this.pendingFilterFocus = null;
    if (!column) return;
    const input = [...this.element.querySelectorAll<HTMLInputElement>("input.column-filter")].find((item) => item.dataset.column === column);
    if (!input || input.disabled) return;
    input.focus({ preventScroll: true });
    input.setSelectionRange(input.value.length, input.value.length);
  }

  private headerCell(column: ColumnInfo, index: number): HTMLElement {
    const state = this.setup.clientSort ? this.clientSort : this.setup.sort;
    const sort = state?.column === column.name ? (state.descending ? "▼" : "▲") : "";
    const isKey = Boolean(column.hints.primary) || this.setup.keyColumns.includes(column.name);
    const onSort = this.setup.clientSort ? (name: string) => this.toggleClientSort(name) : this.setup.onSort;
    const kind = typeKind(column);
    const typeName = column.hints.vectorDims != null && !column.typeName ? `vector(${column.hints.vectorDims})` : column.typeName;
    const cell = h(
      "th",
      {
        class: onSort ? "sortable" : "",
        title: typeName ? `${column.name} · ${typeName}${isKey ? " · key" : ""}${column.nullable ? "" : " · not null"}` : column.name,
        "data-column-index": index,
      },
      h(
        "div",
        { class: "th-content" },
        isKey ? icon("key", "key-icon") : null,
        h("span", { class: "th-name" }, column.name),
        h("span", { class: "sort-mark" }, sort),
      ),
      typeName
        ? h("div", { class: "th-type" }, h("span", { class: `type-badge type-${kind}` }, TYPE_LABELS[kind]), h("span", { class: "th-type-name" }, typeName))
        : null,
    );
    if (onSort) cell.addEventListener("click", () => onSort(column.name));
    return cell;
  }

  private renderRow(row: GridRow, index: number): HTMLTableRowElement {
    const label = row.original ? String(this.setup.rowOffset + index + 1) : "*";
    const current = this.selection?.row === index ? " current" : "";
    const element = h(
      "tr",
      { class: `state-${rowState(row)}${current}`, "data-row": index },
      h("td", { class: "row-number" }, label),
      ...this.displayed.map((column) => this.renderCell(row, row.current[column] ?? NULL, column, index)),
    );
    row.element = element;
    const selected = current ? this.displayed.indexOf(this.selection!.column) : -1;
    if (selected >= 0) element.cells[selected + 1]?.classList.add("selected");
    return element;
  }

  private renderCell(row: GridRow, value: Value, column: number, index: number): HTMLTableCellElement {
    const view = this.setup.cellView?.(index, column, value) ?? null;
    if (view) {
      const title = view.title ?? (view.text.length > TOOLTIP_THRESHOLD ? preview(view.text) : null);
      return h("td", { class: ["cell", view.className ?? ""].filter(Boolean).join(" "), title }, preview(view.text));
    }
    const modified = row.edited && row.original !== null && !valuesEqual(row.original[column] ?? NULL, value);
    const meta = this.setup.columns[column];
    const text = displayValue(value);
    const classes = [
      "cell",
      meta && isNumericKind(meta.kind) ? "numeric" : "",
      meta?.hints.vectorDims != null ? "vector" : "",
      text === null ? "null" : "",
      value.t === "bool" ? "bool" : "",
      modified ? "modified" : "",
    ];
    const title = text !== null && text.length > TOOLTIP_THRESHOLD ? preview(text) : null;
    return h("td", { class: classes.filter(Boolean).join(" "), title }, text === null ? "NULL" : preview(text));
  }

  private refreshRow(index: number): void {
    const row = this.rows[index];
    const previous = row?.element;
    // Rows outside the rendered window are drawn fresh when they scroll in.
    if (!previous) return;
    const next = this.renderRow(row, index);
    previous.replaceWith(next);
    if (this.selection?.row === index) this.cellAt(this.selection)?.classList.add("selected");
  }

  private select(position: Position): void {
    const row = clamp(position.row, 0, this.rows.length - 1);
    const column = nearestVisibleColumn(this.displayed, clamp(position.column, 0, this.setup.columns.length - 1));
    if (row < 0 || column < 0) return;
    const rowChanged = this.selection?.row !== row;
    this.ensureRendered(row);
    this.cellAt(this.selection)?.classList.remove("selected");
    this.selection = { row, column };
    const cell = this.cellAt(this.selection);
    cell?.classList.add("selected");
    cell?.scrollIntoView({ block: "nearest", inline: "nearest" });
    if (rowChanged) {
      for (const element of this.tbody.querySelectorAll("tr.current")) element.classList.remove("current");
      this.rows[row].element?.classList.add("current");
      this.setup.onSelect?.(this.rows[row].current);
    }
  }

  private cellAt(position: Position | null): HTMLElement | null {
    if (!position) return null;
    const displayIndex = this.displayed.indexOf(position.column);
    if (displayIndex < 0) return null;
    return this.rows[position.row]?.element?.cells[displayIndex + 1] ?? null;
  }

  private positionOf(target: EventTarget | null): Position | null {
    const cell = target instanceof Element ? target.closest("td") : null;
    const row = cell?.parentElement;
    if (!cell || !(row instanceof HTMLTableRowElement) || row.dataset.row === undefined) return null;
    const column = this.displayed[Math.max(0, cell.cellIndex - 1)] ?? 0;
    return { row: Number(row.dataset.row), column };
  }

  /** Data column index of a header cell under `target`, if any. */
  private headerColumnOf(target: EventTarget | null): number | null {
    const cell = target instanceof Element ? target.closest<HTMLElement>("th[data-column-index]") : null;
    return cell ? Number(cell.dataset.columnIndex) : null;
  }

  private handlePointer(event: MouseEvent): void {
    if (event.target instanceof HTMLInputElement || event.target instanceof HTMLButtonElement) return;
    const position = this.positionOf(event.target);
    if (position) this.select(position);
  }

  private handleDoubleClick(event: MouseEvent): void {
    const position = this.positionOf(event.target);
    if (position && !this.setup.onCellOpen?.(position.row, position.column)) this.beginEdit(position);
  }

  private handleContextMenu(event: MouseEvent): void {
    const header = this.headerColumnOf(event.target);
    if (header !== null) {
      showContextMenu(event, this.headerMenuItems(header, event));
      return;
    }
    const position = this.positionOf(event.target);
    if (!position) return;
    this.select(position);
    showContextMenu(event, this.menuItems(position));
  }

  private headerMenuItems(column: number, event: MouseEvent): MenuItem[] {
    const name = this.setup.columns[column]?.name ?? "";
    const anchor = { x: event.clientX, y: event.clientY };
    const sorted = this.clientSort?.column === name ? this.clientSort : null;
    const sortItems: MenuItem[] = this.setup.clientSort
      ? [
          { label: "Sort ascending", disabled: sorted?.descending === false, run: () => this.setClientSort({ column: name, descending: false }) },
          { label: "Sort descending", disabled: sorted?.descending === true, run: () => this.setClientSort({ column: name, descending: true }) },
          { label: "Clear sort", disabled: this.clientSort === null, run: () => this.setClientSort(null) },
          "separator",
        ]
      : [];
    return [
      ...sortItems,
      { label: `Hide column "${name}"`, icon: "minus", disabled: this.displayed.length <= 1, run: () => this.hideColumn(column) },
      { label: "Show all columns", disabled: this.hidden.length === 0, run: () => this.setHiddenColumns([]) },
      { label: "Columns…", icon: "columns", run: () => openColumnsPopover(this, anchor) },
    ];
  }

  private menuItems(position: Position): MenuItem[] {
    const row = this.rows[position.row];
    const value = row.current[position.column] ?? NULL;
    const column = this.setup.columns[position.column];
    const copyItems: MenuItem[] = [
      { label: "Open in editor", icon: "expand", run: () => this.openEditor(position) },
      "separator",
      { label: "Copy value", icon: "copy", run: () => void copyText(displayValue(value) ?? "") },
      { label: "Copy row", run: () => void copyText(rowText(row.current)) },
      ...EXPORT_FORMATS.map((format): MenuItem => ({
        label: `Copy page as ${EXPORT_LABELS[format]}`,
        run: () => void this.copyExport(format),
      })),
    ];
    const extra = [...(this.setup.cellMenu?.(position.row, position.column) ?? []), ...(this.setup.extraMenu?.(row.current) ?? [])];
    const readOnlyItems: MenuItem[] = extra.length ? [...extra, "separator", ...copyItems] : copyItems;
    if (!this.setup.editable) return readOnlyItems;
    const locked = row.deleted || !column || !isEditableColumn(column);
    return [
      ...readOnlyItems,
      { label: "Paste", disabled: locked, run: () => void this.pasteFromClipboard(position) },
      "separator",
      { label: "Set NULL", disabled: locked, run: () => this.setCell(position, NULL) },
      ...(column && TEXT_KINDS.has(column.kind.kind)
        ? [{ label: "Set empty string", disabled: locked, run: () => this.setCell(position, { t: "text", v: "" }) }]
        : []),
      "separator",
      { label: "Add row", icon: "plus", run: () => this.addRow() },
      { label: row.deleted ? "Undo delete" : "Delete row", icon: "trash", danger: !row.deleted, run: () => this.deleteSelectedRow() },
    ];
  }

  private async copyExport(format: ExportFormat): Promise<void> {
    await copyText(this.exportPage(format));
    toast(`Copied ${this.visible.length} row(s) as ${EXPORT_LABELS[format]}`, "success");
  }

  private isReadOnlyColumn(column: number): boolean {
    const meta = this.setup.columns[column];
    return !meta || !isEditableColumn(meta);
  }

  private setCell(position: Position, value: Value): void {
    const row = this.rows[position.row];
    if (!row || row.deleted || this.isReadOnlyColumn(position.column)) return;
    row.current[position.column] = value;
    this.rowGeneration += 1;
    row.touched.add(position.column);
    row.edited = true;
    this.refreshRow(position.row);
    if (this.selection?.row === position.row) this.setup.onSelect?.(row.current);
    this.onChange();
  }

  /** Why a cell cannot be changed in the editor, or null when it can. */
  private readOnlyReason(position: Position): string | null {
    const row = this.rows[position.row];
    const column = this.setup.columns[position.column];
    if (!this.setup.editable) return this.setup.readOnlyReason ?? "This grid is read-only";
    if (row?.deleted) return "The row is marked for deletion";
    if (column?.hints.vectorDims != null) return "Vector values are read-only here";
    if (column?.hints.readOnly) return "This column is read-only (computed or generated)";
    return null;
  }

  private openEditor(position: Position): void {
    const row = this.rows[position.row];
    const column = this.setup.columns[position.column];
    if (!row || !column) return;
    if (this.editing) this.commitEdit?.();
    const original = row.original;
    const loadVector = this.setup.loadVector;
    const label = row.original ? `row ${this.setup.rowOffset + position.row + 1}` : "new row";
    openCellEditor({
      column,
      value: row.current[position.column] ?? NULL,
      context: [this.setup.tableName, label].filter(Boolean).join(" · "),
      readOnlyReason: this.readOnlyReason(position),
      loadVector: column.hints.vectorDims != null && original && loadVector ? () => loadVector(original, column) : null,
      save: (value) => this.setCell(position, value),
      onClose: () => this.element.focus({ preventScroll: true }),
    });
  }

  private handleKey(event: KeyboardEvent): void {
    if (this.editing || !this.selection) return;
    const move = MOVES[event.key];
    if (move) {
      event.preventDefault();
      const position = this.visiblePosition.get(this.selection.row) ?? -1;
      const target = this.visible[clamp(position + move.row, 0, this.visible.length - 1)];
      const column = stepVisibleColumn(this.displayed, this.selection.column, move.column);
      if (target !== undefined) this.select({ row: target, column });
      return;
    }
    const command = event.ctrlKey || event.metaKey;
    if (command && event.key.toLowerCase() === "c") {
      event.preventDefault();
      void copyText(displayValue(this.rows[this.selection.row].current[this.selection.column] ?? NULL) ?? "");
    } else if (command && event.key.toLowerCase() === "v") {
      event.preventDefault();
      void this.pasteFromClipboard(this.selection);
    } else if (event.key === "Enter" && event.shiftKey && !command) {
      event.preventDefault();
      this.openEditor(this.selection);
    } else if (event.key === "Enter" || event.key === "F2") {
      event.preventDefault();
      if (event.key === "Enter" && this.setup.onCellOpen?.(this.selection.row, this.selection.column)) return;
      this.beginEdit(this.selection);
    } else if (event.key.length === 1 && !command && !event.altKey) {
      event.preventDefault();
      this.beginEdit(this.selection, event.key);
    }
  }

  /**
   * Pastes text at `position`: one value, or a tab/newline-separated block (as copied from a spreadsheet or with "Copy
   * row") over the cells to the right and below. Each field is parsed for its column's kind; `NULL` is NULL. Fields that
   * do not fit are skipped and reported.
   */
  pasteText(position: Position, text: string): void {
    if (!this.setup.editable) return;
    const { pasted, errors, changedRows } = pasteBlock(
      { columns: this.setup.columns, rows: this.rows, visible: this.visible, visiblePosition: this.visiblePosition, displayed: this.displayed },
      position,
      text,
    );
    for (const rowIndex of changedRows) this.refreshRow(rowIndex);
    if (pasted > 0) {
      const selected = this.selection ? this.rows[this.selection.row] : null;
      if (selected) this.setup.onSelect?.(selected.current);
      this.onChange();
    }
    if (errors.length > 0) toast(`${errors.length} pasted value(s) skipped. ${errors[0]}`, "error");
  }

  private async pasteFromClipboard(position: Position): Promise<void> {
    if (!this.setup.editable) return;
    let text: string;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      toast("The clipboard cannot be read here; use the cell editor (Shift+Enter) and paste into it");
      return;
    }
    this.pasteText(position, text);
  }

  private beginEdit(position: Position, initial?: string): void {
    const row = this.rows[position.row];
    const cell = this.cellAt(position);
    const column = this.setup.columns[position.column];
    if (!this.setup.editable || !row || row.deleted || !cell || !column || this.isReadOnlyColumn(position.column)) return;
    const value = row.current[position.column] ?? NULL;
    // JSON, documents, arrays and multi-line text get the large editor.
    if (isJsonKind(column.kind) || value.t === "doc" || value.t === "array" || (value.t === "text" && value.v.includes("\n"))) {
      this.openEditor(position);
      return;
    }
    if (column.kind.kind === "bool") {
      beginBoolEdit(this.inlineHost(), position, row, cell, value, initial);
      return;
    }
    beginTextEdit(this.inlineHost(), position, row, cell, column, value, initial);
  }

  private inlineHost(): InlineEditHost {
    return {
      displayed: () => this.displayed,
      begin: (row, commit) => {
        this.editing = true;
        this.editingRow = row;
        this.commitEdit = commit;
      },
      end: () => {
        this.editing = false;
        this.editingRow = null;
        this.commitEdit = null;
      },
      refreshRow: (index) => this.refreshRow(index),
      focusGrid: () => this.element.focus({ preventScroll: true }),
      select: (position) => this.select(position),
      selectedRow: () => this.selection?.row,
      rowSelected: (values) => this.setup.onSelect?.(values),
      changed: () => this.onChange(),
      openLargeEditor: (position) => this.openEditor(position),
    };
  }
}
