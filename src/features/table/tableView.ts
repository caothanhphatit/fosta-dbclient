import { api, errorText } from "../../core/api";
import { combinedCondition, combinedJsonFilter, type ColumnFilters } from "../grid/columnFilter";
import { driverInfo, type DriverInfo } from "../../core/drivers";
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { confirmDialog, toast } from "../../core/dialogs";
import { clamp, copyText, formatNumber, h, icon, iconButton, secondaryTool, separator, spacer, toolButton } from "../../core/dom";
import { EXPORT_LABELS, type ExportFormat } from "../importExport/exporters";
import { exportDialog, importDialog } from "../importExport/importExport";
import { DataGrid, type SortState } from "../grid/grid";
import { openColumnsPopover } from "../grid/columnsPopover";
import { columnStorageKey, loadHiddenColumns, saveHiddenColumns } from "../grid/columnVisibility";
import type { Navigator } from "../../core/navigator";
import { RecordPanel } from "./recordPanel";
import { withConfirmation } from "../safety/confirmationDialog";
import { makeSplitter } from "../../core/splitter";
import { equalsFilter, rowKey, sqlDialect, type SqlDialect } from "../../core/sql";
import { LatestRequest } from "../../core/latest";
import type { TabContent } from "../../core/tabs";
import type { Cell, ColumnMeta, TableRelations, TableTarget, VectorInfo } from "../../core/types";

const PAGE_SIZES = [100, 200, 500, 1000];
const EXPORT_FORMATS: ExportFormat[] = ["csv", "json", "markdown", "insert"];
const DEFAULT_PANEL_WIDTH = 340;
const NARROW_WINDOW = 1200;
const READ_ONLY_NOTE = "Read-only workspace: rows cannot be edited, added or deleted here. Change the workspace's safety rules to edit.";

interface ReloadOptions {
  offset?: number;
  sort?: SortState | null;
  force?: boolean;
}

export class TableView implements TabContent {
  readonly element: HTMLElement;
  readonly target: TableTarget;
  private readonly navigator: Navigator;
  private readonly driver: DriverInfo;
  /** How filters and foreign-key navigation quote names and values for this engine. */
  private readonly dialect: SqlDialect;
  private readonly grid: DataGrid;
  private readonly record: RecordPanel;
  private readonly filter: HTMLInputElement;
  private readonly pageInfo = h("span", { class: "pager-info" }, "Loading…");
  /** Page loads overlap when the user pages quickly; only the newest one is shown. */
  private readonly loads = new LatestRequest();
  private readonly vectorTools = h("div", { class: "toolbar-group" });
  private readonly content: HTMLElement;
  private readonly pendingText = h("span", { class: "pending-text" });
  private pendingBar: HTMLElement | null = null;
  private offset = 0;
  private limit = 200;
  private total = 0;
  private sort: SortState | null = null;
  private columns: ColumnMeta[] = [];
  private columnFilters: ColumnFilters = {};
  private primaryKey: string[] = [];
  private relations: TableRelations | null = null;
  private vectorInfo: VectorInfo | null = null;
  private panelWidth = DEFAULT_PANEL_WIDTH;
  private readonly columnsKey: string;
  private hiddenColumns: string[];

  /** Shown while a filter suggested by the assistant waits for the user; nothing has run yet. */
  private suggestionBar: HTMLElement | null = null;

  /**
   * `suggested`: the filter comes from the assistant. It is only pre-filled and marked as such;
   * the table does not load until the user applies it (or opens the table without it).
   */
  constructor(target: TableTarget, navigator: Navigator, initialFilter = "", suggested = false) {
    this.target = target;
    this.navigator = navigator;
    this.columnsKey = columnStorageKey(target.connectionId, target.database, target.table);
    this.hiddenColumns = loadHiddenColumns(this.columnsKey);
    this.driver = driverInfo(target.driver);
    this.dialect = sqlDialect(target.driver);
    this.grid = new DataGrid(() => this.refreshButtons());
    this.record = new RecordPanel(navigator);
    this.filter = h("input", { class: "input", placeholder: this.driver.filterPlaceholder, spellcheck: "false", value: initialFilter });
    this.filter.addEventListener("keydown", (event) => {
      if (event.key === "Enter") this.applyFilter();
    });
    const exportButton = secondaryTool(toolButton("copy", "Copy as", () => undefined, "Copy this page to the clipboard"));
    const exportFileButton = secondaryTool(toolButton("download", "Export", () => this.exportFile(), "Export rows to a file in Downloads"));
    const importButton = secondaryTool(
      toolButton("upload", "Import", () => importDialog(this.target, this.columns, () => void this.reload({ force: true })), "Import rows from CSV or JSON"),
    );
    importButton.hidden = !this.editable;
    exportButton.addEventListener("click", (event) => showContextMenu(event, this.exportMenu()));
    const recordButton = secondaryTool(toolButton("design", "Record", () => this.toggleRecord(), "Show or hide the record panel"));
    const columnsButton = secondaryTool(toolButton("columns", "Columns", () => openColumnsPopover(this.grid, columnsButton), "Show or hide columns"));
    const editorButton = secondaryTool(
      toolButton("expand", "Cell editor", () => this.grid.openSelectedInEditor(), "Open the selected cell in a larger editor (Shift+Enter)"),
    );

    // Saving and discarding live in the pending-changes bar (Apply / Discard), which shows up as soon as there is an edit.
    const editTools = this.editable
      ? [
          toolButton("plus", "Add", () => this.grid.addRow(), "Add row"),
          toolButton("minus", "Delete", () => this.grid.deleteSelectedRow(), "Delete selected row"),
          separator(),
        ]
      : [];
    const toolbar = h(
      "div",
      { class: "toolbar" },
      toolButton("refresh", "Refresh", () => void this.reload({}), "Reload (F5)"),
      separator(),
      ...editTools,
      h("div", { class: "filter-box" }, icon("filter"), this.filter),
      // One right-aligned group: when the pane is narrow it wraps onto its own row as a whole, pager included.
      h(
        "div",
        { class: "toolbar-end" },
        this.vectorTools,
        columnsButton,
        editorButton,
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
    this.suggestionBar = suggested && initialFilter ? this.suggestion(initialFilter) : null;
    // A Read-only workspace blocks every save, so the grid says so up front instead of failing at Apply.
    const readOnlyBar =
      this.target.kind === "table" && this.writesBlocked
        ? h("div", { class: "readonly-bar", role: "status" }, icon("view"), h("span", {}, READ_ONLY_NOTE))
        : null;
    this.element = h("div", { class: "view table-view" }, toolbar, readOnlyBar, this.suggestionBar, this.content, this.pendingBar);
    this.element.addEventListener("keydown", (event) => this.handleShortcut(event));
    void this.loadMetadata();
    if (this.suggestionBar) this.pageInfo.textContent = "Not loaded";
    else void this.reload({});
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
    void this.reload({ offset: 0 });
  }

  canClose(): boolean | Promise<boolean> {
    return !this.grid.hasChanges() || confirmDialog(`Discard unsaved changes in ${this.target.table}?`, "Discard");
  }

  /** Tables are editable unless the workspace's rules block writes; views never are. */
  private get editable(): boolean {
    return this.target.kind === "table" && !this.writesBlocked;
  }

  private get writesBlocked(): boolean {
    return this.navigator.writesBlocked(this.target.connectionId);
  }

  private async loadMetadata(): Promise<void> {
    const { connectionId, database, table } = this.target;
    const [relations, vectorInfo] = await Promise.allSettled([
      api.tableRelations(connectionId, database, table),
      api.vectorInfo(connectionId, database, table),
    ]);
    this.relations = relations.status === "fulfilled" ? relations.value : null;
    this.vectorInfo = vectorInfo.status === "fulfilled" ? vectorInfo.value : null;
    this.renderVectorTools();
    // The first page usually arrives before the metadata; refresh the record
    // panel so foreign-key links show for the row that is already selected.
    this.showRecord(this.grid.selectedRow());
  }

  private renderVectorTools(): void {
    const columns = this.vectorInfo?.columns ?? [];
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

  private exportFile(): void {
    if (this.suggestionBar) {
      toast("Apply the suggested filter (or open the table without it) first");
      return;
    }
    exportDialog({
      target: this.target,
      columns: this.columns,
      pageRows: this.grid.pageRows(),
      request: {
        connectionId: this.target.connectionId,
        database: this.target.database,
        table: this.target.table,
        orderBy: this.sort?.column ?? null,
        descending: this.sort?.descending ?? false,
        filter: this.currentFilter(),
      },
      total: this.total,
    });
  }

  private currentFilter(): string | null {
    return this.driver.filterKind === "json"
      ? combinedJsonFilter(this.columns, this.columnFilters, this.filter.value)
      : combinedCondition(this.columns, this.columnFilters, this.filter.value, this.dialect);
  }

  private exportMenu(): MenuItem[] {
    return EXPORT_FORMATS.map((format) => ({
      label: EXPORT_LABELS[format],
      run: () => {
        void copyText(this.grid.exportPage(format)).then(() => toast(`Copied as ${EXPORT_LABELS[format]}`, "success"));
      },
    }));
  }

  private rowMenu(row: Cell[]): MenuItem[] {
    const vectorItems: MenuItem[] = this.columns
      .filter((column) => column.vectorDims != null)
      .map((column) => ({
        label: `Find similar by ${column.name}`,
        icon: "vector",
        run: () => this.navigator.openVectorSearch(this.target, column.name, rowKey(this.columns, row, this.primaryKey)),
      }));
    const relatedItems: MenuItem[] = (this.relations?.incoming ?? []).map((relation) => ({
      label: `Open related ${relation.table}`,
      icon: "table",
      run: () =>
        this.navigator.openTable(
          { ...this.target, database: relation.database, table: relation.table, kind: "table" },
          equalsFilter(
            relation.columns.map((column, index) => {
              const position = this.columns.findIndex((meta) => meta.name === relation.referencedColumns[index]);
              return { column, value: row[position] ?? null, meta: this.columns[position] };
            }),
            this.dialect,
          ),
        ),
    }));
    return [...vectorItems, ...relatedItems];
  }

  private showRecord(row: Cell[] | null): void {
    this.record.show(
      row && {
        target: this.target,
        columns: this.columns,
        row,
        primaryKey: this.primaryKey,
        relations: this.relations,
        openEditor: (column) => this.grid.openEditorForColumn(column),
      },
    );
  }

  private toggleRecord(): void {
    this.content.classList.toggle("record-hidden");
  }

  private resizeRecord(delta: number): void {
    this.panelWidth = clamp(this.panelWidth + delta, 220, 900);
    this.content.style.setProperty("--record-width", `${this.panelWidth}px`);
  }

  private pager(): HTMLElement {
    const sizes = h(
      "select",
      { class: "input page-size", title: "Rows per page" },
      ...PAGE_SIZES.map((size) => h("option", { value: String(size), selected: size === this.limit }, `${size} rows`)),
    );
    sizes.addEventListener("change", () => {
      this.limit = Number(sizes.value);
      void this.reload({ offset: 0 });
    });
    return h(
      "div",
      { class: "pager" },
      iconButton("first", "First page", () => this.goTo(0)),
      iconButton("prev", "Previous page", () => this.goTo(this.offset - this.limit)),
      this.pageInfo,
      iconButton("chevron", "Next page", () => this.goTo(this.offset + this.limit)),
      iconButton("last", "Last page", () => this.goTo(this.lastPageOffset())),
      sizes,
    );
  }

  private lastPageOffset(): number {
    return Math.floor(Math.max(this.total - 1, 0) / this.limit) * this.limit;
  }

  private goTo(offset: number): void {
    const target = Math.min(Math.max(offset, 0), this.lastPageOffset());
    if (target !== this.offset) void this.reload({ offset: target });
  }

  private applyColumnFilters(filters: ColumnFilters): void {
    this.columnFilters = filters;
    void this.reload({ offset: 0 });
  }

  private toggleSort(column: string): void {
    const current = this.sort?.column === column ? this.sort : null;
    const next = !current ? { column, descending: false } : current.descending ? null : { column, descending: true };
    void this.reload({ offset: 0, sort: next });
  }

  private async reload(options: ReloadOptions): Promise<void> {
    // An assistant suggestion runs only through applyFilter(), never by a refresh, sort or page change.
    if (this.suggestionBar) {
      toast("Apply the suggested filter (or open the table without it) first");
      return;
    }
    if (!options.force && this.grid.hasChanges() && !(await confirmDialog("Discard unsaved changes?", "Discard"))) return;
    const offset = options.offset ?? this.offset;
    const sort = options.sort === undefined ? this.sort : options.sort;
    const ticket = this.loads.begin();
    this.element.classList.add("busy");
    try {
      const filter = this.currentFilter();
      const page = await api.browseTable({
        connectionId: this.target.connectionId,
        database: this.target.database,
        table: this.target.table,
        offset,
        limit: this.limit,
        orderBy: sort?.column ?? null,
        descending: sort?.descending ?? false,
        filter,
      });
      // A newer page change (or a closed tab) wins over this answer.
      if (!this.loads.isCurrent(ticket)) return;
      this.offset = offset;
      this.sort = sort;
      this.total = page.total;
      this.columns = page.columns;
      this.primaryKey = page.primaryKey;
      this.grid.load({
        columns: page.columns,
        rows: page.rows,
        editable: this.editable,
        readOnlyReason: this.writesBlocked ? "The workspace is Read-only" : null,
        primaryKey: page.primaryKey,
        rowOffset: offset,
        sort,
        onSort: (column) => this.toggleSort(column),
        tableName: this.target.table,
        dialect: this.dialect,
        onSelect: (row) => this.showRecord(row),
        extraMenu: (row) => this.rowMenu(row),
        columnFilters: this.columnFilters,
        onColumnFilter: (filters) => this.applyColumnFilters(filters),
        clientFilter: false,
        hiddenColumns: this.hiddenColumns,
        onHiddenColumns: (hidden) => {
          this.hiddenColumns = hidden;
          saveHiddenColumns(this.columnsKey, hidden);
        },
        loadVector: (row, column) => {
          const { connectionId, database, table } = this.target;
          return api.fetchVector(connectionId, database, table, column.name, rowKey(page.columns, row, page.primaryKey));
        },
      });
      this.pageInfo.textContent = describePage(offset, page.rows.length, page.total, page.elapsedMs);
    } catch (error) {
      if (this.loads.isCurrent(ticket)) toast(errorText(error), "error");
    } finally {
      if (this.loads.isCurrent(ticket)) this.element.classList.remove("busy");
    }
  }

  dispose(): void {
    this.loads.close();
    this.grid.dispose();
  }

  private async submit(): Promise<void> {
    const changes = this.grid.changes();
    if (changes.length === 0) return;
    const noKeyWarning = `${this.target.table} has no primary key. Rows will be matched by all of their original values. Continue?`;
    if (this.primaryKey.length === 0 && !(await confirmDialog(noKeyWarning, "Continue"))) return;
    try {
      const affected = await withConfirmation((confirmation) =>
        api.applyChanges(this.target.connectionId, this.target.database, this.target.table, changes, "gridSave", confirmation),
      );
      if (affected === null) return;
      toast(`Saved ${changes.length} change(s), ${affected} row(s) affected`, "success");
      await this.reload({ force: true });
    } catch (error) {
      toast(errorText(error), "error");
    }
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
      void this.reload({});
    }
  }
}

function describePage(offset: number, count: number, total: number, elapsedMs: number): string {
  if (count === 0) return `0 of ${formatNumber(total)}`;
  return `${formatNumber(offset + 1)}–${formatNumber(offset + count)} of ${formatNumber(total)} · ${elapsedMs} ms`;
}
