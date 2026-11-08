/**
 * The server view: the queries running now (with Stop, which is `KILL QUERY`) and the recent queries from `system.query_log`
 * with their duration, memory and rows. The log is written a few seconds after a query ends, and is absent when the server
 * has it switched off; the view says so.
 */
import { h } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import { ClickHouseApi } from "./clickhouseApi";
import { button, dataTable, emptyHint, noteLine, tabBar } from "./kit";
import { cellOf, formatMillis, isFailure, numberCell, oneLine, type OperationTable } from "./model";
import "./clickhouse.css";

const LOG_SIZES = [50, 100, 500];
const REFRESH_MS = 3000;

export class ServerView implements TabContent {
  readonly element: HTMLElement;
  private readonly api: ClickHouseApi;
  private readonly body = h("div", { class: "ch-scroll" });
  private readonly status = h("span", { class: "ch-status" });
  private readonly size = h("select", { class: "input" }, ...LOG_SIZES.map((count) => h("option", { value: String(count) }, `last ${count}`)));
  private readonly failedOnly = h("input", { type: "checkbox" });
  private readonly logControls: HTMLElement;
  private readonly watchBox = h("input", { type: "checkbox" });
  private readonly watchControls: HTMLElement;
  private readonly tabs: ReturnType<typeof tabBar>;
  private tab = 0;
  private disposed = false;
  private timer: number | null = null;

  constructor(ctx: ViewContext) {
    this.api = new ClickHouseApi(ctx.connection);
    this.size.value = "100";
    this.size.addEventListener("change", () => void this.load());
    this.failedOnly.addEventListener("change", () => void this.load());
    this.watchBox.checked = true;
    this.watchBox.addEventListener("change", () => this.schedule());
    this.logControls = h("span", { class: "ch-controls" }, this.size, h("label", { class: "inline-field" }, this.failedOnly, h("span", {}, "Failed only")));
    this.watchControls = h("label", { class: "inline-field" }, this.watchBox, h("span", {}, "Refresh every 3 s"));
    this.tabs = tabBar(["Running queries", "Query log"], (index) => {
      this.tab = index;
      this.logControls.hidden = index !== 1;
      this.watchControls.hidden = index !== 0;
      void this.load();
    });
    this.element = h(
      "div",
      { class: "view ch-view" },
      h(
        "div",
        { class: "toolbar ch-toolbar" },
        this.tabs.element,
        button("Refresh", () => void this.load(), "tool-button primary"),
        this.logControls,
        this.watchControls,
        h("span", { class: "spacer" }),
        this.status,
        h("span", { class: "toolbar-title" }, ctx.profile.name),
      ),
      this.body,
    );
    this.tabs.select(0);
  }

  onActivate(): void {
    void this.load();
  }

  dispose(): void {
    this.disposed = true;
    if (this.timer !== null) window.clearTimeout(this.timer);
  }

  private schedule(): void {
    if (this.timer !== null) window.clearTimeout(this.timer);
    this.timer = null;
    if (!this.disposed && this.tab === 0 && this.watchBox.checked) this.timer = window.setTimeout(() => void this.load(), REFRESH_MS);
  }

  private async load(): Promise<void> {
    if (this.timer !== null) window.clearTimeout(this.timer);
    this.timer = null;
    const tab = this.tab;
    try {
      this.status.textContent = "Loading…";
      const table = await (tab === 0 ? this.api.processes() : this.api.queryLog(Number(this.size.value) || 100, this.failedOnly.checked));
      if (this.disposed || tab !== this.tab) return;
      this.status.textContent = "";
      this.body.replaceChildren(...(tab === 0 ? this.drawProcesses(table) : this.drawLog(table)).filter((item) => item !== null));
      this.schedule();
    } catch (error) {
      if (this.disposed) return;
      this.status.textContent = "";
      this.body.replaceChildren(emptyHint(tab === 0 ? "The running queries could not be read." : "The query log could not be read."));
      showError(error, tab === 0 ? "Read the running queries" : "Read the query log");
    }
  }

  private drawProcesses(table: OperationTable): (HTMLElement | null)[] {
    if (table.rows.length === 0) return [emptyHint("No other query is running.")];
    return [
      dataTable(table.columns, table.rows, {
        render: (column, value) =>
          column === "query" && value !== null
            ? h("span", { class: "mono", title: value }, oneLine(value, 240))
            : column === "read_rows" && value !== null
              ? numberCell(value)
              : (value ?? ""),
        actionsHeader: "",
        actions: (row) => {
          const id = cellOf(table, row, "query_id");
          return button("Stop", () => void this.stop(id), "link-button", "KILL QUERY: stops this query on the server");
        },
      }),
    ];
  }

  private drawLog(table: OperationTable): (HTMLElement | null)[] {
    const note = noteLine(table.note);
    if (table.rows.length === 0) return [note, emptyHint("Nothing to show.")];
    return [
      note,
      dataTable(table.columns, table.rows, {
        render: (column, value, row) => {
          if (value === null) return "";
          if (column === "query_duration_ms") return formatMillis(Number(value));
          if (column === "read_rows" || column === "written_rows") return numberCell(value);
          if (column === "query") return h("span", { class: "mono", title: value }, oneLine(value, 240));
          if (column === "type") return h("span", { class: isFailure(value) ? "ch-bad" : "" }, value === "QueryFinish" ? "finished" : value);
          if (column === "query_id") return h("span", { class: "mono", title: cellOf(table, row, "query_id") }, value.slice(0, 8));
          return value;
        },
        rowClass: (row) => (isFailure(cellOf(table, row, "type")) ? "ch-failed" : ""),
      }),
    ];
  }

  private async stop(queryId: string): Promise<void> {
    try {
      if ((await this.api.killQuery(queryId)) !== null) void this.load();
    } catch (error) {
      showError(error, "Stop the query");
    }
  }
}

export const serverViewFactory = (ctx: ViewContext): ServerView => new ServerView(ctx);
