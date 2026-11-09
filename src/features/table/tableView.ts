/**
 * The data view of a table, view or collection (ADR 0001: it gets its `ConnectionApi` through the `ViewContext`):
 *
 * - Pages come from `browse_object`: by keyset while the engine hands out `nextKeyset` (cheap at any depth), by offset
 *   otherwise (a jump to the last page, sorts the engine cannot seek on). The total is the engine's estimate by default
 *   (`~`), and exact after "Count".
 * - Column filter boxes send structured conditions (`Filter.conditions`; the engine binds the values). The filter box in
 *   the toolbar sends the engine's raw filter language (`Filter.raw`: SQL WHERE, a MongoDB query document) and appears
 *   only when the connection has the `rawFilter` capability. One request carries one filter, so while the raw box is in
 *   use the column boxes are paused (kept, shown disabled with a note); clearing the box brings them back.
 * - Edits are saved with `apply_edits` through the gateway's confirmation flow, with optimistic concurrency (`original`,
 *   row versions); no transaction is held while the user edits. A `CONFLICT` opens the conflict dialog.
 */
import type {
  ColumnInfo,
  ConnectInfo,
  EngineDescriptor,
  Field,
  Filter,
  FilterCond,
  ObjectDescription,
  ObjectRef,
  Page,
  RawFilterLang,
  RowChangeSet,
  SortKey,
  Total,
  Value,
} from "../../core/contract";
import type { ConnectionApi } from "../../core/connectionApi";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { confirmDialog, openModal, toast } from "../../core/dialogs";
import { clamp, copyText, formatNumber, h, icon, iconButton, secondaryTool, separator, spacer, toolButton } from "../../core/dom";
import { hasCapability } from "../../core/engines";
import { showError } from "../../core/errorPanel";
import { LatestRequest } from "../../core/latest";
import type { Navigator } from "../../core/navigator";
import { makeSplitter } from "../../core/splitter";
import { dialectOf, type SqlDialect } from "../../core/sql";
import type { TabContent } from "../../core/tabs";
import type { TableTarget, VectorInfo } from "../../core/types";
import { displayValue } from "../../core/values";
import type { ViewContext } from "../../core/views";
import { api } from "../../core/api";
import { forced, type BuiltChange } from "../grid/changeSet";
import { conditionsFilter, filterConditions, type ColumnFilters } from "../grid/columnFilter";
import { openColumnsPopover } from "../grid/columnsPopover";
import { columnStorageKey, loadHiddenColumns, saveHiddenColumns } from "../grid/columnVisibility";
import { openConflictDialog } from "../grid/conflictDialog";
import { conflictPayload, conflictRows, type ConflictPayload, type ConflictRow } from "../grid/conflicts";
import { DataGrid, type SortState } from "../grid/grid";
import { textColumn } from "../grid/textColumns";
import { keyCells } from "../vector/vectorRows";
import { EXPORT_LABELS, type ExportFormat } from "../importExport/exporters";
import { exportDialog, importDialog } from "../importExport/importExport";
import { withConfirmation } from "../safety/confirmationDialog";
import { RecordPanel, referencingFilters, referencingObject } from "./recordPanel";

const PAGE_SIZES = [100, 200, 500, 1000];
const EXPORT_FORMATS: ExportFormat[] = ["csv", "json", "markdown", "insert"];
const DEFAULT_PANEL_WIDTH = 340;
const NARROW_WINDOW = 1200;
const READ_ONLY_NOTE = "Read-only workspace: rows cannot be edited, added or deleted here. Change the workspace's safety rules to edit.";
const PAUSED_NOTE =
  "Column filters are paused while the filter box is in use: one request carries one filter. Clear the box (Enter) to filter by column again.";
const NULL: Value = { t: "null", v: null };

/** Where a page starts: after a keyset or at an offset (`offset` is also the row number of its first row). */
interface PageCursor {
  offset: number;
  after: Value[] | null;
}

interface ReloadOptions {
  cursor?: PageCursor;
  /** Pages before the new one, for Previous (keyset pages cannot be computed backwards). */
  history?: PageCursor[];
  sort?: SortState | null;
  force?: boolean;
  /** Ask for an exact total ("Count"). */
  exact?: boolean;
}

/** Object kinds whose rows can be edited (views never are). */
const EDITABLE_KINDS = new Set(["table", "collection"]);

export class TableView implements TabContent {
  readonly element: HTMLElement;
  /** The pre-contract address the vector views and the assistant still use. */
  readonly target: TableTarget;
  private readonly connection: ConnectionApi;
  private readonly object: ObjectRef;
  private readonly engine: EngineDescriptor;
  private readonly info: ConnectInfo;
  private readonly navigator: Navigator;
  private readonly label: string;
  /** How "Copy as INSERT" quotes names and values. */
  private readonly dialect: SqlDialect;
  /** The engine's raw filter language when the connection has `rawFilter`; null hides the filter box. */
  private readonly rawLanguage: RawFilterLang | null;
  private readonly grid: DataGrid;
  private readonly record: RecordPanel;
  private readonly filter: HTMLInputElement;
  private readonly pageInfo = h("span", { class: "pager-info" }, "Loading…");
  private readonly countButton: HTMLButtonElement;
  private readonly pagerButtons: { first: HTMLButtonElement; prev: HTMLButtonElement; next: HTMLButtonElement; last: HTMLButtonElement };
  /** Page loads overlap when the user pages quickly; only the newest one is shown. */
  private readonly loads = new LatestRequest();
  private readonly vectorTools = h("div", { class: "toolbar-group" });
  private readonly content: HTMLElement;
  private readonly pendingText = h("span", { class: "pending-text" });
  private pendingBar: HTMLElement | null = null;
  private cursor: PageCursor = { offset: 0, after: null };
  private history: PageCursor[] = [];
  private page: Page | null = null;
  private limit = 200;
  private total: Total | null = null;
  private exact = false;
  private sort: SortState | null = null;
  private columns: ColumnInfo[] = [];
  private columnFilters: ColumnFilters;
  /** The raw filter of the page on screen (what the box held when it was applied). */
  private appliedRaw = "";
  private keyColumns: string[] = [];
  private description: ObjectDescription | null = null;
  private vectorInfo: VectorInfo | null = null;
  private panelWidth = DEFAULT_PANEL_WIDTH;
  private readonly columnsKey: string;
  private hiddenColumns: string[];

  /** The first page load (or nothing, while an assistant suggestion waits); `startTransfer` waits for it. */
  private firstLoad: Promise<void> = Promise.resolve();

  /** Shown while a filter suggested by the assistant waits for the user; nothing has run yet. */
  private suggestionBar: HTMLElement | null = null;

  /**
   * `ctx.params`: `filter` (raw filter text), `suggested` (the filter comes from the assistant: it is only pre-filled and
   * marked; the table does not load until the user applies it), `columnFilters` (foreign-key navigation).
   */
  constructor(ctx: ViewContext) {
    if (!ctx.object) throw new Error("This view needs a table, view or collection");
    this.connection = ctx.connection;
    this.object = ctx.object;
    this.engine = ctx.engine;
    this.info = ctx.info;
    this.navigator = ctx.navigator;
    this.label = this.object.schema ? `${this.object.schema}.${this.object.name}` : this.object.name;
    this.target = {
      connectionId: ctx.profile.id,
      driver: ctx.profile.driver,
      database: this.object.database ?? "",
      table: this.label,
      kind: EDITABLE_KINDS.has(this.object.kind) ? "table" : "view",
    };
    this.dialect = dialectOf(this.engine.sqlDialect);
    this.rawLanguage = hasCapability(this.info, "rawFilter") ? this.engine.filter.raw : null;
    const params = ctx.params ?? {};
    const initialFilter = typeof params.filter === "string" ? params.filter : "";
    const suggested = params.suggested === true;
    this.columnFilters = isFilters(params.columnFilters) ? { ...params.columnFilters } : {};
    this.columnsKey = columnStorageKey(this.target.connectionId, this.target.database, this.target.table);
    this.hiddenColumns = loadHiddenColumns(this.columnsKey);
    this.grid = new DataGrid(() => this.refreshButtons());
    this.record = new RecordPanel();
    this.filter = h("input", {
      class: "input",
      placeholder: this.engine.filter.placeholder,
      spellcheck: "false",
      value: this.rawLanguage ? initialFilter : "",
    });
    this.filter.addEventListener("keydown", (event) => {
      if (event.key === "Enter") this.applyFilter();
    });
    if (initialFilter && !this.rawLanguage) toast(`${this.engine.label} takes no raw filter here; the table opens unfiltered`);
    const exportButton = secondaryTool(toolButton("copy", "Copy as", () => undefined, "Copy this page to the clipboard"));
    const exportFileButton = secondaryTool(toolButton("download", "Export", () => this.exportFile(), "Export rows to a file in Downloads"));
    const importButton = secondaryTool(toolButton("upload", "Import", () => this.openImport(), "Import rows from CSV or JSON"));
    importButton.hidden = !this.editable;
    exportButton.addEventListener("click", (event) => showContextMenu(event, this.exportMenu()));
    const recordButton = secondaryTool(toolButton("design", "Record", () => this.toggleRecord(), "Show or hide the record panel"));
    const columnsButton = secondaryTool(toolButton("columns", "Columns", () => openColumnsPopover(this.grid, columnsButton), "Show or hide columns"));
    const editorButton = secondaryTool(
      toolButton("expand", "Cell editor", () => this.grid.openSelectedInEditor(), "Open the selected cell in a larger editor (Shift+Enter)"),
    );
    const queryButton = secondaryTool(toolButton("query", "Show query", () => this.showStatement(), "The statement the engine ran for this page"));
    this.countButton = h(
      "button",
      { class: "link-button pager-count", type: "button", title: "Count the matching rows exactly (may be slow on big tables)" },
      "Count",
    );
    this.countButton.addEventListener("click", () => void this.reload({ cursor: this.cursor, history: this.history, exact: true }));
    this.pagerButtons = {
      first: iconButton("first", "First page", () => this.goFirst()),
      prev: iconButton("prev", "Previous page", () => this.goPrevious()),
      next: iconButton("chevron", "Next page", () => this.goNext()),
      last: iconButton("last", "Last page", () => this.goLast()),
    };

    // Saving and discarding live in the pending-changes bar (Apply / Discard), which shows up as soon as there is an edit.
    const editTools = this.editable
      ? [
          toolButton("plus", "Add", () => this.grid.addRow(), "Add row"),
          toolButton("minus", "Delete", () => this.grid.deleteSelectedRow(), "Delete selected row"),
          separator(),
        ]
      : [];
    const filterBox = this.rawLanguage
      ? h("div", { class: "filter-box", title: `Raw ${this.engine.label} filter (${this.rawLanguage})` }, icon("filter"), this.filter)
      : null;
    const toolbar = h(
      "div",
      { class: "toolbar" },
      toolButton("refresh", "Refresh", () => void this.reload({ cursor: this.cursor, history: this.history }), "Reload (F5)"),
      separator(),
      ...editTools,
      filterBox ?? spacer(),
      // One right-aligned group: when the pane is narrow it wraps onto its own row as a whole, pager included.
      h(
        "div",
        { class: "toolbar-end" },
        this.vectorTools,
        columnsButton,
        editorButton,
        queryButton,
        importButton,
        exportFileButton,
        exportButton,
        recordButton,
        this.pager(),
      ),
    );
    const splitter = h("div", { class: "splitter-x" });
    makeSplitter(splitter, "x", (delta) => this.resizeRecord(-delta));
    this.content = h("div", { class: "table-content" }, this.grid.element, splitter, this.record.element);
    this.content.style.setProperty("--record-width", `${this.panelWidth}px`);
    // In a narrow window the record panel would leave room for only a few grid columns; it starts closed there.
    if (window.innerWidth < NARROW_WINDOW) this.content.classList.add("record-hidden");
    const applyButton = h("button", { class: "btn primary", type: "button", title: "Save changes in one transaction (Ctrl+S)" }, "Apply");
    applyButton.addEventListener("click", () => void this.submit());
    const discardPending = h("button", { class: "btn", type: "button" }, "Discard");
    discardPending.addEventListener("click", () => this.grid.discard());
    this.pendingBar = h("div", { class: "pending-bar" }, icon("edit"), this.pendingText, spacer(), discardPending, applyButton);
    this.pendingBar.hidden = true;
    this.suggestionBar = suggested && initialFilter && this.rawLanguage ? this.suggestion(initialFilter) : null;
    // A Read-only workspace blocks every save, so the grid says so up front instead of failing at Apply.
    const readOnlyBar =
      EDITABLE_KINDS.has(this.object.kind) && this.writesBlocked
        ? h("div", { class: "readonly-bar", role: "status" }, icon("view"), h("span", {}, READ_ONLY_NOTE))
        : null;
    this.element = h("div", { class: "view table-view" }, toolbar, readOnlyBar, this.suggestionBar, this.content, this.pendingBar);
    this.element.addEventListener("keydown", (event) => this.handleShortcut(event));
    void this.loadMetadata();
    if (this.suggestionBar) this.pageInfo.textContent = "Not loaded";
    else this.firstLoad = this.reload({});
  }

  /** Opens the import or export dialog once the columns are known (the tree's Import… / Export… actions). */
  async startTransfer(kind: "import" | "export"): Promise<void> {
    await this.firstLoad;
    if (kind === "export") this.exportFile();
    else if (!this.editable) toast("This object cannot be edited here, so rows cannot be imported into it");
    else this.openImport();
  }

  private suggestion(filter: string): HTMLElement {
    const apply = h("button", { class: "btn primary", type: "button" }, "Apply filter");
    apply.addEventListener("click", () => this.applyFilter());
    const without = h("button", { class: "btn", type: "button" }, "Open without filter");
    without.addEventListener("click", () => {
      this.filter.value = "";
      this.applyFilter();
    });
    this.filter.classList.add("suggested");
    return h(
      "div",
      { class: "suggestion-bar", role: "status" },
      icon("sparkle"),
      h("span", {}, "Filter suggested by the assistant. It has not run; review it, edit it if needed, then apply it."),
      h("code", { class: "suggestion-filter" }, filter),
      spacer(),
      without,
      apply,
    );
  }

  /** Runs the filter in the box; also accepts (and clears) an assistant suggestion. */
  private applyFilter(): void {
    if (this.suggestionBar) {
      this.suggestionBar.remove();
      this.suggestionBar = null;
      this.filter.classList.remove("suggested");
    }
    this.exact = false;
    void this.reload({ cursor: { offset: 0, after: null }, history: [] });
  }

  hasUnsavedChanges(): boolean {
    return this.grid.hasChanges();
  }

  canClose(): boolean | Promise<boolean> {
    return !this.grid.hasChanges() || confirmDialog(`Discard unsaved changes in ${this.label}?`, "Discard");
  }

  /** Tables and collections are editable when the engine edits rows and the workspace's rules allow writes; views never. */
  private get editable(): boolean {
    return EDITABLE_KINDS.has(this.object.kind) && hasCapability(this.info, "editRows") && !this.writesBlocked;
  }

  private get writesBlocked(): boolean {
    return this.navigator.writesBlocked(this.target.connectionId);
  }

  private async loadMetadata(): Promise<void> {
    const vector = hasCapability(this.info, "vector");
    const [description, vectorInfo] = await Promise.allSettled([
      hasCapability(this.info, "describe") ? this.connection.describe(this.object) : Promise.resolve(null),
      vector ? api.vectorInfo(this.target.connectionId, this.target.database, this.target.table) : Promise.resolve(null),
    ]);
    this.description = description.status === "fulfilled" ? description.value : null;
    this.vectorInfo = vectorInfo.status === "fulfilled" ? vectorInfo.value : null;
    this.renderVectorTools();
    // The first page usually arrives before the metadata; refresh the record
    // panel so foreign-key links show for the row that is already selected.
    this.showRecord(this.grid.selectedRow());
  }

  private get hasVectors(): boolean {
    return Boolean(this.vectorInfo?.supported && this.vectorInfo.columns.length > 0);
  }

  private renderVectorTools(): void {
    const columns = this.hasVectors ? (this.vectorInfo?.columns ?? []) : [];
    if (columns.length === 0) {
      this.vectorTools.replaceChildren();
      return;
    }
    const first = columns[0].name;
    this.vectorTools.replaceChildren(
      separator(),
      secondaryTool(toolButton("vector", "Vector search", () => this.navigator.openVectorSearch(this.target, first), "Similarity search on this table")),
      secondaryTool(toolButton("health", "Vector health", () => this.navigator.openVectorHealth(this.target, first), "Check vector data and index quality")),
    );
  }

  private openImport(): void {
    importDialog({
      title: this.label,
      where: `${this.target.database}.${this.label} (${this.engine.label})`,
      object: this.object,
      columns: this.columns.length ? this.columns : (this.description?.columns ?? []),
      connection: this.connection,
      onDone: () => void this.reload({ cursor: this.cursor, history: this.history, force: true }),
    });
  }

  private exportFile(): void {
    if (this.suggestionBar) {
      toast("Apply the suggested filter (or open the table without it) first");
      return;
    }
    let filter: Filter;
    try {
      filter = this.currentFilter();
    } catch (error) {
      showError(error, "Export");
      return;
    }
    exportDialog({
      title: this.label,
      columns: this.columns,
      pageRows: this.grid.pageRows(),
      filtered: filter.kind !== "none",
      total: this.total,
      dialect: this.dialect,
      read: (cursor, limit) =>
        this.connection.browse({
          ...this.request(filter, cursor, this.sort),
          limit,
          estimateTotal: false,
          exactTotal: false,
        }),
    });
  }

  /** The filter of the next request: the raw box when it is in use, else the column boxes' valid conditions. */
  private currentFilter(): Filter {
    const raw = this.filter.value.trim();
    if (this.rawLanguage && raw) return { kind: "raw", language: this.rawLanguage, text: raw };
    return conditionsFilter(this.conditions());
  }

  /** Conditions of the column boxes. Before the first page the columns are not known; boxes then parse as text columns. */
  private conditions(): FilterCond[] {
    const known = this.columns.length ? this.columns : (this.description?.columns ?? []);
    const columns = Object.keys(this.columnFilters).map((name) => known.find((column) => column.name === name) ?? textColumn(name));
    return filterConditions(columns, this.columnFilters).conditions;
  }

  private request(filter: Filter, cursor: PageCursor, sort: SortState | null) {
    const keys: SortKey[] = sort ? [{ column: sort.column, descending: sort.descending }] : [];
    return {
      object: this.object,
      filter,
      sort: keys,
      offset: cursor.offset,
      after: cursor.after,
      limit: this.limit,
      estimateTotal: true,
      exactTotal: this.exact,
      columns: null,
    };
  }

  private exportMenu(): MenuItem[] {
    return EXPORT_FORMATS.map((format) => ({
      label: EXPORT_LABELS[format],
      run: () => {
        void copyText(this.grid.exportPage(format)).then(() => toast(`Copied as ${EXPORT_LABELS[format]}`, "success"));
      },
    }));
  }

  private rowMenu(row: Value[]): MenuItem[] {
    const vectorItems: MenuItem[] = this.hasVectors
      ? this.columns
          .filter((column) => column.hints.vectorDims != null)
          .map((column) => ({
            label: `Find similar by ${column.name}`,
            icon: "vector",
            run: () => this.navigator.openVectorSearch(this.target, column.name, keyCells(this.columns, row, this.keyColumns)),
          }))
      : [];
    const relatedItems: MenuItem[] = (this.description?.referencedBy ?? []).map((key) => ({
      label: `Open related ${key.table}`,
      icon: "table",
      run: () => this.openRelated(referencingObject(this.description, key), referencingFilters(this.columns, row, key)),
    }));
    return [...vectorItems, ...relatedItems];
  }

  private openRelated(object: ObjectRef, filters: ColumnFilters): void {
    this.navigator.openData(this.target.connectionId, object, filters);
  }

  private showRecord(row: Value[] | null): void {
    this.record.show(
      row && {
        columns: this.columns,
        row,
        keyColumns: this.keyColumns,
        description: this.description,
        openEditor: (column) => this.grid.openEditorForColumn(column),
        openRelated: (object, filters) => this.openRelated(object, filters),
        vector: this.hasVectors
          ? {
              load: (column) => this.fetchVector(row, column),
              findSimilar: (column) => this.navigator.openVectorSearch(this.target, column.name, keyCells(this.columns, row, this.keyColumns)),
            }
          : null,
      },
    );
  }

  private fetchVector(row: Value[], column: ColumnInfo): Promise<number[]> {
    const { connectionId, database, table } = this.target;
    return api.fetchVector(connectionId, database, table, column.name, keyCells(this.columns, row, this.keyColumns));
  }

  private toggleRecord(): void {
    this.content.classList.toggle("record-hidden");
  }

  private resizeRecord(delta: number): void {
    this.panelWidth = clamp(this.panelWidth + delta, 220, 900);
    this.content.style.setProperty("--record-width", `${this.panelWidth}px`);
  }

  private showStatement(): void {
    const statement = this.page?.statement;
    if (!statement) {
      toast(this.page ? "The engine reported no statement for this page" : "Load a page first");
      return;
    }
    openModal(
      "Query for this page",
      h("div", { class: "form-grid" }, h("pre", { class: "ddl statement-text" }, statement)),
      [
        { label: "Copy", alignLeft: true, run: async () => void (await copyText(statement)) },
        { label: "Close", primary: true, run: () => undefined },
      ],
      { width: 680 },
    );
  }

  private pager(): HTMLElement {
    const sizes = h(
      "select",
      { class: "input page-size", title: "Rows per page" },
      ...PAGE_SIZES.map((size) => h("option", { value: String(size), selected: size === this.limit }, `${size} rows`)),
    );
    sizes.addEventListener("change", () => {
      this.limit = Number(sizes.value);
      void this.reload({ cursor: { offset: 0, after: null }, history: [] });
    });
    const { first, prev, next, last } = this.pagerButtons;
    return h("div", { class: "pager" }, first, prev, this.pageInfo, this.countButton, next, last, sizes);
  }

  private lastPageOffset(): number | null {
    if (!this.total) return null;
    return Math.floor(Math.max(this.total.value - 1, 0) / this.limit) * this.limit;
  }

  private goFirst(): void {
    if (this.cursor.offset > 0 || this.cursor.after) void this.reload({ cursor: { offset: 0, after: null }, history: [] });
  }

  private goPrevious(): void {
    if (this.cursor.offset === 0) return;
    const history = [...this.history];
    // Keyset pages cannot be computed backwards: Previous returns to the cursor of the page before. After a jump by offset
    // (Last) there is no history, so it steps back by offset.
    const previous = history.pop() ?? { offset: Math.max(0, this.cursor.offset - this.limit), after: null };
    void this.reload({ cursor: previous, history });
  }

  private goNext(): void {
    const page = this.page;
    if (!page?.hasMore) return;
    const offset = this.cursor.offset + page.rows.length;
    void this.reload({ cursor: { offset, after: page.nextKeyset ?? null }, history: [...this.history, this.cursor] });
  }

  private goLast(): void {
    const offset = this.lastPageOffset();
    if (offset === null) {
      toast("The total is not known yet: press Count first");
      return;
    }
    if (offset !== this.cursor.offset) void this.reload({ cursor: { offset, after: null }, history: [] });
  }

  private applyColumnFilters(filters: ColumnFilters): void {
    this.columnFilters = filters;
    this.exact = false;
    void this.reload({ cursor: { offset: 0, after: null }, history: [] });
  }

  private toggleSort(column: string): void {
    const current = this.sort?.column === column ? this.sort : null;
    const next = !current ? { column, descending: false } : current.descending ? null : { column, descending: true };
    void this.reload({ cursor: { offset: 0, after: null }, history: [], sort: next });
  }

  private async reload(options: ReloadOptions): Promise<void> {
    // An assistant suggestion runs only through applyFilter(), never by a refresh, sort or page change.
    if (this.suggestionBar) {
      toast("Apply the suggested filter (or open the table without it) first");
      return;
    }
    if (!options.force && this.grid.hasChanges() && !(await confirmDialog("Discard unsaved changes?", "Discard"))) return;
    const cursor = options.cursor ?? this.cursor;
    const history = options.history ?? this.history;
    const sort = options.sort === undefined ? this.sort : options.sort;
    if (options.exact) this.exact = true;
    // A newer page change or closing the tab cancels this read on the server, which frees its connection.
    const requestId = `browse-${crypto.randomUUID()}`;
    const ticket = this.loads.begin(() => void this.connection.cancelRequest(requestId).catch(() => undefined));
    this.element.classList.add("busy");
    try {
      const filter = this.currentFilter();
      const page = await this.connection.browse(this.request(filter, cursor, sort), requestId).finally(() => this.loads.finish(ticket));
      // A newer page change (or a closed tab) wins over this answer.
      if (!this.loads.isCurrent(ticket)) return;
      this.cursor = cursor;
      this.history = history;
      this.sort = sort;
      this.page = page;
      if (page.total) this.total = page.total;
      else if (cursor.offset === 0) this.total = null;
      // The last page tells the exact number of rows.
      if (!page.hasMore && !this.total?.exact) this.total = { value: cursor.offset + page.rows.length, exact: true };
      this.appliedRaw = filter.kind === "raw" ? filter.text : "";
      this.columns = page.columns;
      this.keyColumns = page.keyColumns;
      this.grid.load({
        columns: page.columns,
        rows: page.rows,
        rowVersions: page.rowVersions ?? null,
        editable: this.editable,
        readOnlyReason: this.writesBlocked ? "The workspace is Read-only" : null,
        keyColumns: page.keyColumns,
        rowOffset: cursor.offset,
        sort,
        onSort: (column) => this.toggleSort(column),
        tableName: this.label,
        dialect: this.dialect,
        onSelect: (row) => this.showRecord(row),
        extraMenu: (row) => this.rowMenu(row),
        columnFilters: this.columnFilters,
        onColumnFilter: (filters) => this.applyColumnFilters(filters),
        filtersPaused: this.appliedRaw ? PAUSED_NOTE : null,
        clientFilter: false,
        hiddenColumns: this.hiddenColumns,
        onHiddenColumns: (hidden) => {
          this.hiddenColumns = hidden;
          saveHiddenColumns(this.columnsKey, hidden);
        },
        loadVector: hasCapability(this.info, "vector") ? (row, column) => this.fetchVector(row, column) : null,
      });
      this.describePage();
    } catch (error) {
      if (this.loads.isCurrent(ticket)) showError(error, `Read rows of ${this.target.database}.${this.label}`);
    } finally {
      if (this.loads.isCurrent(ticket)) this.element.classList.remove("busy");
    }
  }

  private describePage(): void {
    const page = this.page;
    if (!page) return;
    const count = page.rows.length;
    const offset = this.cursor.offset;
    const total = this.total;
    const totalText = total ? `${total.exact ? "" : "~"}${formatNumber(total.value)}` : page.hasMore ? "?" : formatNumber(offset + count);
    const range = count === 0 ? `0 of ${totalText}` : `${formatNumber(offset + 1)}–${formatNumber(offset + count)} of ${totalText}`;
    this.pageInfo.textContent = `${range} · ${page.elapsedMs} ms`;
    this.pageInfo.title = total
      ? total.exact
        ? "Exact count"
        : "Estimated from the engine's statistics; press Count for the exact number"
      : "The total is unknown; press Count";
    this.countButton.hidden = Boolean(total?.exact) || (!page.hasMore && offset === 0);
    const { first, prev, next, last } = this.pagerButtons;
    first.disabled = offset === 0 && !this.cursor.after;
    prev.disabled = offset === 0;
    next.disabled = !page.hasMore;
    const lastOffset = this.lastPageOffset();
    last.disabled = !page.hasMore || lastOffset === null || lastOffset <= offset;
  }

  dispose(): void {
    this.loads.close();
    this.grid.dispose();
  }

  private async submit(): Promise<void> {
    const built = this.grid.changes();
    if (built.length === 0) return;
    const noKeyWarning = `${this.label} has no primary key. Rows will be matched by all of their comparable original values. Continue?`;
    if (this.keyColumns.length === 0 && built.some(({ change }) => change.op !== "insert") && !(await confirmDialog(noKeyWarning, "Continue"))) return;
    const set: RowChangeSet = { object: this.object, changes: built.map(({ change }) => change) };
    try {
      const result = await withConfirmation((confirmation) => this.connection.applyEdits(set, "gridSave", confirmation));
      if (result === null) return;
      const parts = [
        result.updated ? `${result.updated} updated` : "",
        result.inserted ? `${result.inserted} inserted` : "",
        result.deleted ? `${result.deleted} deleted` : "",
      ];
      toast(`Saved ${built.length} change(s): ${parts.filter(Boolean).join(", ") || "no rows affected"}`, "success");
      await this.reload({ cursor: this.cursor, history: this.history, force: true });
    } catch (error) {
      const conflict = conflictPayload(error);
      if (conflict) this.showConflicts(conflict, built);
      else showError(error, "Save changes");
    }
  }

  /** The conflict dialog for a failed save; nothing of it was applied, so every edit is still pending. */
  private showConflicts(payload: ConflictPayload, built: BuiltChange[]): void {
    const rowOf = (row: ConflictRow) => built[row.index];
    openConflictDialog({
      objectLabel: this.label,
      payload,
      rows: conflictRows(payload.conflicts, (conflict) => {
        const entry = built[conflict.index];
        return entry ? this.grid.loadedFields(entry.row) : null;
      }),
      otherPending: Math.max(0, built.length - payload.conflicts.length),
      overwrite: async (row) => {
        const entry = rowOf(row);
        if (!entry) return false;
        const change = forced(entry.change);
        const result = await withConfirmation((confirmation) =>
          this.connection.applyEdits({ object: this.object, changes: [change] }, "gridSave", confirmation),
        );
        if (result === null) return false;
        if (change.op === "delete") this.grid.replaceRow(entry.row, null, null);
        else if (change.op === "update") await this.refreshRow(entry.row, updatedKey(change.key, change.values));
        return true;
      },
      takeTheirs: async (row) => {
        const entry = rowOf(row);
        if (!entry) return false;
        if (row.missing || entry.change.op === "insert") this.grid.replaceRow(entry.row, null, null);
        else await this.refreshRow(entry.row, row.key);
        return true;
      },
      onClose: () => this.refreshButtons(),
    });
  }

  /** Reloads one row by its key (keeping every other pending edit); a row that is gone leaves the grid. */
  private async refreshRow(index: number, key: Field[]): Promise<void> {
    const conditions: FilterCond[] = key.map((field) =>
      field.value.t === "null"
        ? { column: field.column, op: "isNull", values: [] }
        : { column: field.column, op: "eq", values: [displayValue(field.value) ?? ""] },
    );
    const page = await this.connection.browse({
      object: this.object,
      filter: conditionsFilter(conditions),
      sort: [],
      offset: 0,
      after: null,
      limit: 1,
      estimateTotal: false,
      exactTotal: false,
      columns: null,
    });
    const found = page.rows[0];
    if (!found) {
      this.grid.replaceRow(index, null, null);
      return;
    }
    const values = this.columns.map((column) => found[page.columns.findIndex((item) => item.name === column.name)] ?? NULL);
    this.grid.replaceRow(index, values, page.rowVersions?.[0] ?? null);
  }

  private refreshButtons(): void {
    const dirty = this.grid.hasChanges();
    if (this.pendingBar) {
      const { updated, inserted, deleted } = this.grid.changeSummary();
      const total = updated + inserted + deleted;
      const parts = [updated ? `${updated} updated` : "", inserted ? `${inserted} new` : "", deleted ? `${deleted} deleted` : ""].filter(Boolean);
      this.pendingText.textContent = `${total} pending ${total === 1 ? "change" : "changes"} · ${parts.join(" · ")}`;
      this.pendingBar.hidden = !dirty;
    }
  }

  private handleShortcut(event: KeyboardEvent): void {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "s") {
      event.preventDefault();
      void this.submit();
    } else if (event.key === "F5") {
      event.preventDefault();
      void this.reload({ cursor: this.cursor, history: this.history });
    }
  }
}

/** The key of a row after an update that may have changed key columns. */
function updatedKey(key: Field[], values: Field[]): Field[] {
  return key.map((field) => values.find((value) => value.column === field.column) ?? field);
}

function isFilters(value: unknown): value is ColumnFilters {
  return typeof value === "object" && value !== null && Object.values(value).every((item) => typeof item === "string");
}
