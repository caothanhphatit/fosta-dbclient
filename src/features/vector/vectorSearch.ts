import { showError } from "../../core/errorPanel";
import { api, errorText } from "../../core/api";
import { toast } from "../../core/dialogs";
import { clamp, copyText, formatNumber, h, icon, separator, spacer, toolButton } from "../../core/dom";
import { capabilityOf } from "../../core/engines";
import { DataGrid, gridSetup } from "../grid/grid";
import { keyCells, legacyGridData } from "../grid/legacy";
import type { Navigator } from "../../core/navigator";
import { RecordPanel } from "../table/recordPanel";
import { makeSplitter } from "../../core/splitter";
import { describeKey } from "../../core/sql";
import type { TabContent } from "../../core/tabs";
import type { CellValue, Metric, QueryVector, TableTarget, VectorInfo, VectorSearchResult } from "../../core/types";

type SourceKind = QueryVector["kind"];

const SOURCES: { kind: SourceKind; label: string; hint: string }[] = [
  { kind: "row", label: "Similar to row", hint: "Right-click a row in the table → Find similar" },
  { kind: "text", label: "Text", hint: "Embedded with the configured embedding API" },
  { kind: "literal", label: "Vector", hint: "Paste a vector such as [0.12, -0.03, …]" },
];
const METRICS: { value: Metric; label: string }[] = [
  { value: "auto", label: "Auto (index metric)" },
  { value: "cosine", label: "Cosine" },
  { value: "euclidean", label: "Euclidean" },
];
const DEFAULT_K = 10;

export class VectorSearchView implements TabContent {
  readonly element: HTMLElement;
  private readonly target: TableTarget;
  private readonly navigator: Navigator;
  private readonly columnSelect = h("select", { class: "input" });
  private readonly metricSelect = h("select", { class: "input" }, ...METRICS.map((metric) => h("option", { value: metric.value }, metric.label)));
  private readonly kInput = h("input", { class: "input k-input", type: "number", min: 1, max: 1000, value: String(DEFAULT_K) });
  private readonly filterInput = h("input", { class: "input mono", placeholder: "Optional WHERE, e.g. tenant_id = 't1'", spellcheck: "false" });
  private readonly queryText = h("textarea", { class: "input query-text", spellcheck: "false", rows: 3 });
  private planGrid: DataGrid | null = null;
  private readonly sourceTabs = h("div", { class: "segmented" });
  private readonly sourceHint = h("div", { class: "field-hint" });
  private readonly status = h("div", { class: "search-status" });
  private readonly planHost = h("div", { class: "plan-host" });
  private readonly grid: DataGrid;
  private readonly record: RecordPanel;
  private readonly searchButton: HTMLButtonElement;
  private source: SourceKind;
  private seed: CellValue[] | null;
  private last: VectorSearchResult | null = null;
  private info: VectorInfo | null = null;

  constructor(target: TableTarget, column: string, navigator: Navigator, seed: CellValue[] | null) {
    this.target = target;
    this.navigator = navigator;
    this.seed = seed;
    this.source = seed ? "row" : "text";
    this.grid = new DataGrid();
    this.record = new RecordPanel();
    this.searchButton = toolButton("vector", "Search", () => void this.search(), "Search (Ctrl+Enter)");
    this.queryText.addEventListener("keydown", (event) => {
      if ((event.ctrlKey || event.metaKey) && event.key === "Enter") void this.search();
    });
    this.columnSelect.addEventListener("change", () => {
      if (this.info) this.describeIndex(this.info);
    });

    const form = h(
      "div",
      { class: "search-form" },
      h(
        "div",
        { class: "search-row" },
        this.sourceTabs,
        spacer(),
        labelled("Column", this.columnSelect),
        labelled("Top K", this.kInput),
        labelled("Distance", this.metricSelect),
      ),
      this.queryText,
      this.sourceHint,
      h("div", { class: "search-row" }, h("div", { class: "filter-box wide" }, icon("filter"), this.filterInput)),
    );
    const toolbar = h(
      "div",
      { class: "toolbar" },
      this.searchButton,
      separator(),
      toolButton("copy", "Copy SQL", () => void this.copySql(false), "Copy the query with a ? placeholder for the vector"),
      toolButton("copy", "Copy runnable SQL", () => void this.copySql(true), "Copy the query with the vector inlined"),
      toolButton("design", "Plan", () => this.planHost.classList.toggle("open"), "Show EXPLAIN output"),
      spacer(),
      h("span", { class: "toolbar-title" }, `${target.database}.${target.table}`),
    );
    const splitter = h("div", { class: "splitter-x" });
    const content = h(
      "div",
      { class: "table-content" },
      h("div", { class: "search-results" }, this.status, this.planHost, this.grid.element),
      splitter,
      this.record.element,
    );
    let panelWidth = 340;
    content.style.setProperty("--record-width", `${panelWidth}px`);
    makeSplitter(splitter, "x", (delta) => {
      panelWidth = clamp(panelWidth - delta, 220, 900);
      content.style.setProperty("--record-width", `${panelWidth}px`);
    });
    this.element = h("div", { class: "view vector-search" }, toolbar, form, content);
    this.renderSources();
    void this.loadColumns(column);
  }

  setSeed(column: string, seed: CellValue[]): void {
    this.seed = seed;
    this.source = "row";
    this.columnSelect.value = column;
    this.renderSources();
    void this.search();
  }

  private async loadColumns(preferred: string): Promise<void> {
    if (!capabilityOf(this.target.connectionId, "vector")) {
      this.setStatus("error", "This connection has no vector support (the engine does not report the vector capability).");
      return;
    }
    try {
      const info = await api.vectorInfo(this.target.connectionId, this.target.database, this.target.table);
      this.info = info;
      this.columnSelect.replaceChildren(
        ...info.columns.map((column) => h("option", { value: column.name, selected: column.name === preferred }, `${column.name} (${column.dims})`)),
      );
      this.describeIndex(info);
      if (this.seed) void this.search();
    } catch (error) {
      showError(error, "Vector search");
    }
  }

  private describeIndex(info: VectorInfo): void {
    if (!info.supported) {
      this.setStatus("error", "This server has no vector functions. MariaDB 11.7 or newer is required.");
      return;
    }
    const index = info.indexes.find((item) => item.column === this.columnSelect.value);
    this.setStatus(
      index ? "info" : "warn",
      index
        ? `Vector index ${index.name} · ${index.metric} · M=${index.m ?? "default"}`
        : "No vector index on this column: every search scans the whole table.",
    );
  }

  private renderSources(): void {
    const buttons = SOURCES.map((source) => {
      const button = h("button", { class: `segment${source.kind === this.source ? " active" : ""}`, type: "button" }, source.label);
      button.addEventListener("click", () => {
        this.source = source.kind;
        this.renderSources();
      });
      return button;
    });
    this.sourceTabs.replaceChildren(...buttons);
    const current = SOURCES.find((source) => source.kind === this.source);
    this.queryText.hidden = this.source === "row";
    this.queryText.placeholder = this.source === "text" ? "What are employees entitled to for annual leave?" : "[0.0123, -0.0456, …]";
    const seedText = this.seed ? `Query vector from row: ${describeKey(this.seed, "mysql")}` : "No row selected yet.";
    this.sourceHint.replaceChildren(this.source === "row" ? seedText : (current?.hint ?? ""));
    if (this.source === "text")
      this.sourceHint.append(
        " · ",
        embeddingLink(() => this.navigator.configureEmbedding()),
      );
  }

  private buildQuery(): QueryVector | null {
    if (this.source === "row") return this.seed ? { kind: "row", key: this.seed } : null;
    const text = this.queryText.value.trim();
    if (!text) return null;
    return this.source === "text" ? { kind: "text", text } : { kind: "literal", text };
  }

  private async search(): Promise<void> {
    const query = this.buildQuery();
    if (!query || !this.columnSelect.value) {
      toast(this.source === "row" ? "Pick a row: right-click it in the table → Find similar" : "Enter a query first");
      return;
    }
    this.searchButton.disabled = true;
    this.element.classList.add("busy");
    try {
      const result = await api.vectorSearch({
        connectionId: this.target.connectionId,
        database: this.target.database,
        table: this.target.table,
        column: this.columnSelect.value,
        query,
        k: Number(this.kInput.value) || DEFAULT_K,
        metric: this.metricSelect.value as Metric,
        filter: this.filterInput.value.trim() || null,
      });
      this.showResult(result);
    } catch (error) {
      this.setStatus("error", errorText(error));
    } finally {
      this.searchButton.disabled = false;
      this.element.classList.remove("busy");
    }
  }

  private showResult(result: VectorSearchResult): void {
    this.last = result;
    const { columns, rows } = legacyGridData(result.result.columns, result.result.rows);
    const { connectionId, database, table } = this.target;
    this.grid.load({
      ...gridSetup(columns, rows),
      clientFilter: true,
      keyColumns: result.primaryKey,
      tableName: this.target.table,
      onSelect: (row) =>
        this.record.show(
          row && {
            columns,
            row,
            keyColumns: result.primaryKey,
            description: null,
            openEditor: (column) => this.grid.openEditorForColumn(column),
            openRelated: null,
            vector: {
              load: (column) => api.fetchVector(connectionId, database, table, column.name, keyCells(columns, row, result.primaryKey)),
              findSimilar: (column) => this.setSeed(column.name, keyCells(columns, row, result.primaryKey)),
            },
          },
        ),
    });
    const indexText = result.indexUsed ? `Used index ${result.indexUsed}` : "Full scan: the vector index was not used";
    this.setStatus(
      result.indexUsed ? "ok" : "warn",
      `${indexText} · ${formatNumber(rows.length)} result(s) · ${formatNumber(result.elapsedMs)} ms · ${result.queryDims} dims`,
    );
    this.planGrid?.dispose();
    const plan = new DataGrid();
    this.planGrid = plan;
    const planData = legacyGridData(result.plan.columns, result.plan.rows);
    plan.load(gridSetup(planData.columns, planData.rows));
    this.planHost.replaceChildren(h("div", { class: "section-title" }, "EXPLAIN"), plan.element);
  }

  dispose(): void {
    this.grid.dispose();
    this.planGrid?.dispose();
  }

  private async copySql(runnable: boolean): Promise<void> {
    if (!this.last) {
      toast("Run a search first");
      return;
    }
    await copyText(runnable ? this.last.runnableSql : this.last.sql);
    toast("SQL copied", "success");
  }

  private setStatus(kind: "ok" | "info" | "warn" | "error", text: string): void {
    this.status.className = `search-status ${kind}`;
    this.status.textContent = text;
  }
}

function labelled(label: string, control: HTMLElement): HTMLElement {
  return h("label", { class: "inline-field" }, h("span", {}, label), control);
}

function embeddingLink(open: () => void): HTMLElement {
  const link = h("button", { class: "link-button", type: "button" }, "Configure embedding API");
  link.addEventListener("click", open);
  return link;
}
