import { FILTER_HELP, matchesFilter, type ColumnFilters } from "./columnFilter";
import { showContextMenu, type MenuItem } from "./contextMenu";
import { toast } from "./dialogs";
import { clamp, copyText, h, icon } from "./dom";
import { EXPORT_LABELS, exportRows, type ExportFormat } from "./exporters";
import { rowKey } from "./sql";
import type { Cell, CellValue, ColumnMeta, RowChange } from "./types";
import "./grid-virtual.css";

export interface SortState {
  column: string;
  descending: boolean;
}

export interface GridSetup {
  columns: ColumnMeta[];
  rows: Cell[][];
  editable: boolean;
  primaryKey: string[];
  rowOffset: number;
  sort: SortState | null;
  onSort: ((column: string) => void) | null;
  tableName: string | null;
  onSelect: ((row: Cell[] | null) => void) | null;
  extraMenu: ((row: Cell[]) => MenuItem[]) | null;
  columnFilters: ColumnFilters;
  onColumnFilter: ((filters: ColumnFilters) => void) | null;
  clientFilter: boolean;
}

interface GridRow {
  original: Cell[] | null;
  current: Cell[];
  deleted: boolean;
  element: HTMLTableRowElement | null;
}

interface Position {
  row: number;
  column: number;
}

type RowState = "clean" | "modified" | "new" | "deleted";

const PREVIEW_LIMIT = 300;
const FILTER_DEBOUNCE_MS = 350;
const TOOLTIP_THRESHOLD = 60;
/** Rows rendered beyond the viewport on each side. */
const OVERSCAN = 12;
/** Window bounds snap to this many rows (even, so zebra striping keeps its parity). */
const WINDOW_CHUNK = 16;
/** Viewport assumed before the grid is attached and sized. */
const DEFAULT_VIEWPORT = 800;
/** Row height (26px cell incl. its 1px border), refined by measuring once. */
let rowHeight = 26;
let rowHeightMeasured = false;
const MOVES: Record<string, Position> = {
  ArrowUp: { row: -1, column: 0 },
  ArrowDown: { row: 1, column: 0 },
  ArrowLeft: { row: 0, column: -1 },
  ArrowRight: { row: 0, column: 1 },
};

export function gridSetup(columns: ColumnMeta[], rows: Cell[][]): GridSetup {
  return {
    columns,
    rows,
    editable: false,
    primaryKey: [],
    rowOffset: 0,
    sort: null,
    onSort: null,
    tableName: null,
    onSelect: null,
    extraMenu: null,
    columnFilters: {},
    onColumnFilter: null,
    clientFilter: false,
  };
}

export function textColumn(name: string): ColumnMeta {
  return { name, typeName: "", numeric: false, binary: false, primary: false, vectorDims: null };
}

const EXPORT_FORMATS: ExportFormat[] = ["csv", "json", "markdown", "insert"];

type TypeKind = "number" | "text" | "date" | "bool" | "json" | "binary" | "vector";

const TYPE_LABELS: Record<TypeKind, string> = { number: "123", text: "abc", date: "date", bool: "bool", json: "json", binary: "bin", vector: "vec" };

/** Coarse family of a column type, for the badge under its name. */
function typeKind(column: ColumnMeta): TypeKind {
  const type = column.typeName.toLowerCase();
  if (column.vectorDims != null) return "vector";
  if (/^(bit|bool|boolean|tinyint\(1\))/.test(type)) return "bool";
  if (/date|time|year/.test(type)) return "date";
  if (/json|object|array|document/.test(type)) return "json";
  if (column.binary || /blob|binary|image|objectid/.test(type)) return "binary";
  if (column.numeric) return "number";
  return "text";
}

export class DataGrid {
  readonly element: HTMLElement;
  private setup: GridSetup = gridSetup([], []);
  private rows: GridRow[] = [];
  private tbody: HTMLTableSectionElement = h("tbody");
  private selection: Position | null = null;
  private editing = false;
  private filters: ColumnFilters = {};
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

  constructor(onChange: () => void = () => undefined) {
    this.onChange = onChange;
    this.element = h("div", { class: "grid", tabIndex: 0 });
    this.element.addEventListener("scroll", () => this.scheduleWindow(), { passive: true });
    if (typeof ResizeObserver !== "undefined") new ResizeObserver(() => this.scheduleWindow()).observe(this.element);
    this.element.addEventListener("keydown", (event) => this.handleKey(event));
    this.element.addEventListener("mousedown", (event) => this.handlePointer(event));
    this.element.addEventListener("dblclick", (event) => this.handleDoubleClick(event));
    this.element.addEventListener("contextmenu", (event) => this.handleContextMenu(event));
  }

  load(setup: GridSetup): void {
    this.setup = setup;
    this.filters = { ...setup.columnFilters };
    this.rows = setup.rows.map((row) => ({ original: row, current: [...row], deleted: false, element: null }));
    this.selection = null;
    this.render();
    this.onChange();
    this.selectFirstVisible();
  }

  selectedRow(): Cell[] | null {
    return this.selection ? this.rows[this.selection.row]?.current ?? null : null;
  }

  /** Rows currently shown (after client-side filters), as edited. */
  pageRows(): Cell[][] {
    return this.visibleRows();
  }

  exportPage(format: ExportFormat): string {
    return exportRows(format, this.setup.columns, this.visibleRows(), this.setup.tableName);
  }

  private visibleRows(): Cell[][] {
    return this.visible.map((index) => this.rows[index].current);
  }

  private selectFirstVisible(): void {
    const first = this.visible[0];
    if (first === undefined) this.setup.onSelect?.(null);
    else this.select({ row: first, column: this.selection?.column ?? 0 });
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
    const indexes = this.rows.map((_, index) => index);
    if (!this.setup.clientFilter) return indexes;
    const active = this.setup.columns
      .map((column, index) => ({ index, text: this.filters[column.name] ?? "" }))
      .filter(({ text }) => text.trim() !== "");
    return indexes.filter((rowIndex) => active.every(({ index, text }) => matchesFilter(this.rows[rowIndex].current[index], text)));
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

  changes(): RowChange[] {
    return this.rows.flatMap((row) => this.changeOf(row));
  }

  addRow(): void {
    if (!this.setup.editable) return;
    const row: GridRow = { original: null, current: this.setup.columns.map(() => null), deleted: false, element: null };
    const index = this.rows.push(row) - 1;
    this.visiblePosition.set(index, this.visible.push(index) - 1);
    this.element.querySelector(".grid-empty")?.remove();
    this.updateWindow(true);
    this.select({ row: index, column: 0 });
    this.onChange();
  }

  deleteSelectedRow(): void {
    if (!this.setup.editable || !this.selection) return;
    const index = this.selection.row;
    const row = this.rows[index];
    if (row.original) {
      row.deleted = !row.deleted;
      this.refreshRow(index);
    } else {
      this.rows.splice(index, 1);
      this.selection = null;
      this.render();
    }
    this.onChange();
  }

  private changeOf(row: GridRow): RowChange[] {
    const original = row.original;
    if (original === null) {
      return [{ kind: "insert", values: this.cellsWhere(row.current, (index) => row.current[index] !== null) }];
    }
    if (row.deleted) return [{ kind: "delete", key: this.keyOf(original) }];
    const values = this.cellsWhere(row.current, (index) => row.current[index] !== original[index]);
    return values.length ? [{ kind: "update", key: this.keyOf(original), values }] : [];
  }

  private keyOf(original: Cell[]): CellValue[] {
    return rowKey(this.setup.columns, original, this.setup.primaryKey);
  }

  private cellsWhere(values: Cell[], include: (index: number) => boolean): CellValue[] {
    return this.setup.columns.flatMap((column, index) => (include(index) ? [{ column: column.name, value: values[index] }] : []));
  }

  private render(): void {
    this.visible = this.computeVisible();
    this.visiblePosition = new Map(this.visible.map((index, position) => [index, position]));
    this.headerCells = [h("th", { class: "row-number" }), ...this.setup.columns.map((column) => this.headerCell(column))];
    this.columnWidths = this.headerCells.map(() => 0);
    const header = h("tr", {}, ...this.headerCells);
    const filterRow = this.filtersEnabled ? this.filterRow() : null;
    this.tbody = h("tbody");
    for (const row of this.rows) row.element = null;
    this.editingRow = null;
    this.commitEdit = null;
    this.windowStart = 0;
    this.windowEnd = 0;
    const span = this.setup.columns.length + 1;
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

  private scheduleWindow(): void {
    if (this.frame !== null) return;
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
    return Object.keys(this.filters).length ? "No rows match the column filters" : "No rows";
  }

  private filterRow(): HTMLTableRowElement {
    const cells = this.setup.columns.map((column) => {
      if (column.vectorDims != null) return h("th", { class: "filter-cell" });
      const input = h("input", {
        class: `column-filter${this.filters[column.name] ? " active" : ""}`,
        value: this.filters[column.name] ?? "",
        placeholder: "filter",
        title: FILTER_HELP,
        spellcheck: "false",
        "data-column": column.name,
      });
      let pending: ReturnType<typeof setTimeout> | null = null;
      const apply = (text: string) => {
        if (pending) clearTimeout(pending);
        pending = null;
        this.applyColumnFilter(column.name, text);
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
      return h("th", { class: "filter-cell" }, input);
    });
    return h("tr", { class: "filter-row" }, h("th", { class: "row-number filter-cell", title: FILTER_HELP }, icon("filter")), ...cells);
  }

  private restoreFilterFocus(): void {
    const column = this.pendingFilterFocus;
    this.pendingFilterFocus = null;
    if (!column) return;
    const input = [...this.element.querySelectorAll<HTMLInputElement>("input.column-filter")].find((item) => item.dataset.column === column);
    if (!input) return;
    input.focus({ preventScroll: true });
    input.setSelectionRange(input.value.length, input.value.length);
  }

  private headerCell(column: ColumnMeta): HTMLElement {
    const sort = this.setup.sort?.column === column.name ? (this.setup.sort.descending ? "▼" : "▲") : "";
    const isKey = column.primary || this.setup.primaryKey.includes(column.name);
    const onSort = this.setup.onSort;
    const kind = typeKind(column);
    const cell = h(
      "th",
      { class: onSort ? "sortable" : "", title: column.typeName ? `${column.name} · ${column.typeName}${isKey ? " · primary key" : ""}` : column.name },
      h("div", { class: "th-content" }, isKey ? icon("key", "key-icon") : null, h("span", { class: "th-name" }, column.name), h("span", { class: "sort-mark" }, sort)),
      column.typeName ? h("div", { class: "th-type" }, h("span", { class: `type-badge type-${kind}` }, TYPE_LABELS[kind]), h("span", { class: "th-type-name" }, column.typeName)) : null,
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
      ...row.current.map((value, column) => this.renderCell(row, value, column)),
    );
    row.element = element;
    if (current) element.cells[this.selection!.column + 1]?.classList.add("selected");
    return element;
  }

  private renderCell(row: GridRow, value: Cell, column: number): HTMLTableCellElement {
    const modified = row.original !== null && row.original[column] !== value;
    const meta = this.setup.columns[column];
    const classes = [
      "cell",
      meta?.numeric ? "numeric" : "",
      meta?.vectorDims != null ? "vector" : "",
      value === null ? "null" : "",
      modified ? "modified" : "",
    ];
    const title = value !== null && value.length > TOOLTIP_THRESHOLD ? preview(value) : null;
    return h("td", { class: classes.filter(Boolean).join(" "), title }, value === null ? "NULL" : preview(value));
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
    const column = clamp(position.column, 0, this.setup.columns.length - 1);
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
    return this.rows[position.row]?.element?.cells[position.column + 1] ?? null;
  }

  private positionOf(target: EventTarget | null): Position | null {
    const cell = target instanceof Element ? target.closest("td") : null;
    const row = cell?.parentElement;
    if (!cell || !(row instanceof HTMLTableRowElement) || row.dataset.row === undefined) return null;
    return { row: Number(row.dataset.row), column: Math.max(0, cell.cellIndex - 1) };
  }

  private handlePointer(event: MouseEvent): void {
    if (event.target instanceof HTMLInputElement) return;
    const position = this.positionOf(event.target);
    if (position) this.select(position);
  }

  private handleDoubleClick(event: MouseEvent): void {
    const position = this.positionOf(event.target);
    if (position) this.beginEdit(position);
  }

  private handleContextMenu(event: MouseEvent): void {
    const position = this.positionOf(event.target);
    if (!position) return;
    this.select(position);
    showContextMenu(event, this.menuItems(position));
  }

  private menuItems(position: Position): MenuItem[] {
    const row = this.rows[position.row];
    const value = row.current[position.column];
    const copyItems: MenuItem[] = [
      { label: "Copy value", icon: "copy", run: () => void copyText(value ?? "") },
      { label: "Copy row", run: () => void copyText(row.current.map((cell) => cell ?? "NULL").join("\t")) },
      ...EXPORT_FORMATS.map((format): MenuItem => ({
        label: `Copy page as ${EXPORT_LABELS[format]}`,
        run: () => void this.copyExport(format),
      })),
    ];
    const extra = this.setup.extraMenu?.(row.current) ?? [];
    const readOnlyItems: MenuItem[] = extra.length ? [...extra, "separator", ...copyItems] : copyItems;
    if (!this.setup.editable) return readOnlyItems;
    return [
      ...readOnlyItems,
      "separator",
      { label: "Set NULL", disabled: row.deleted, run: () => this.setCell(position, null) },
      { label: "Set empty string", disabled: row.deleted, run: () => this.setCell(position, "") },
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
    return this.setup.columns[column]?.vectorDims != null;
  }

  private setCell(position: Position, value: Cell): void {
    const row = this.rows[position.row];
    if (!row || row.deleted || this.isReadOnlyColumn(position.column)) return;
    row.current[position.column] = value;
    this.refreshRow(position.row);
    this.onChange();
  }

  private handleKey(event: KeyboardEvent): void {
    if (this.editing || !this.selection) return;
    const move = MOVES[event.key];
    if (move) {
      event.preventDefault();
      const position = this.visiblePosition.get(this.selection.row) ?? -1;
      const target = this.visible[clamp(position + move.row, 0, this.visible.length - 1)];
      if (target !== undefined) this.select({ row: target, column: this.selection.column + move.column });
      return;
    }
    const command = event.ctrlKey || event.metaKey;
    if (command && event.key.toLowerCase() === "c") {
      event.preventDefault();
      void copyText(this.rows[this.selection.row].current[this.selection.column] ?? "");
    } else if (event.key === "Enter" || event.key === "F2") {
      event.preventDefault();
      this.beginEdit(this.selection);
    } else if (event.key.length === 1 && !command && !event.altKey) {
      event.preventDefault();
      this.beginEdit(this.selection, event.key);
    }
  }

  private beginEdit(position: Position, initial?: string): void {
    const row = this.rows[position.row];
    const cell = this.cellAt(position);
    if (!this.setup.editable || !row || row.deleted || !cell || this.isReadOnlyColumn(position.column)) return;
    const original = row.current[position.column];
    const input = h("input", { class: "cell-editor", value: initial ?? original ?? "", spellcheck: "false" });
    let finished = false;
    const finish = (commit: boolean, next: Position | null, refocus: boolean) => {
      if (finished) return;
      finished = true;
      this.editing = false;
      this.editingRow = null;
      this.commitEdit = null;
      if (commit && input.value !== (original ?? "")) row.current[position.column] = input.value;
      this.refreshRow(position.row);
      if (refocus) this.element.focus({ preventScroll: true });
      if (next) this.select(next);
      this.onChange();
    };
    input.addEventListener("keydown", (event) => {
      event.stopPropagation();
      if (event.key === "Enter") finish(true, { row: position.row + 1, column: position.column }, true);
      else if (event.key === "Escape") finish(false, null, true);
      else if (event.key === "Tab") {
        event.preventDefault();
        finish(true, { row: position.row, column: position.column + (event.shiftKey ? -1 : 1) }, true);
      }
    });
    input.addEventListener("blur", () => finish(true, null, false));
    this.editing = true;
    this.editingRow = position.row;
    this.commitEdit = () => finish(true, null, true);
    cell.classList.add("editing");
    cell.replaceChildren(input);
    input.focus();
    if (initial === undefined) input.select();
  }
}

function rowState(row: GridRow): RowState {
  const original = row.original;
  if (!original) return "new";
  if (row.deleted) return "deleted";
  return row.current.some((value, index) => value !== original[index]) ? "modified" : "clean";
}

function preview(value: string): string {
  return value.length > PREVIEW_LIMIT ? `${value.slice(0, PREVIEW_LIMIT)}…` : value;
}
