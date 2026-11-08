/** Parts and mutations of one table: what the table is stored as, and the mutations (edits, ALTER … UPDATE/DELETE) that ran or still run. */
import { h } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import { ClickHouseApi, type TableRef } from "./clickhouseApi";
import { button, dataTable, emptyHint, tabBar } from "./kit";
import { cellOf, formatBytes, numberCell, oneLine, type OperationTable } from "./model";
import "./clickhouse.css";

export class PartsView implements TabContent {
  readonly element: HTMLElement;
  private readonly api: ClickHouseApi;
  private readonly ref: TableRef;
  private readonly body = h("div", { class: "ch-scroll" });
  private readonly status = h("span", { class: "ch-status" });
  private readonly tabs: ReturnType<typeof tabBar>;
  private tab = 0;
  private disposed = false;
  private timer: number | null = null;

  constructor(ctx: ViewContext) {
    if (!ctx.object?.database) throw new Error("This view needs a table");
    this.api = new ClickHouseApi(ctx.connection);
    this.ref = { database: ctx.object.database, table: ctx.object.name };
    this.tabs = tabBar(["Parts", "Mutations"], (index) => {
      this.tab = index;
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
        h("span", { class: "spacer" }),
        this.status,
        h("span", { class: "toolbar-title" }, `${this.ref.database}.${this.ref.table}`),
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

  private async load(): Promise<void> {
    if (this.timer !== null) window.clearTimeout(this.timer);
    this.timer = null;
    const tab = this.tab;
    try {
      this.status.textContent = "Loading…";
      const table = await (tab === 0 ? this.api.parts(this.ref) : this.api.mutations(this.ref));
      if (this.disposed || tab !== this.tab) return;
      this.status.textContent = "";
      this.body.replaceChildren(...(tab === 0 ? this.drawParts(table) : this.drawMutations(table)).filter((item) => item !== null));
      // A mutation that is still running is worth watching.
      if (tab === 1 && table.rows.some((row) => cellOf(table, row, "is_done") !== "1")) this.timer = window.setTimeout(() => void this.load(), 3000);
    } catch (error) {
      if (this.disposed) return;
      this.status.textContent = "";
      this.body.replaceChildren(emptyHint("Could not read the table's parts."));
      showError(error, tab === 0 ? "Read the parts" : "Read the mutations");
    }
  }

  private drawParts(table: OperationTable): (HTMLElement | null)[] {
    if (table.rows.length === 0) return [emptyHint("This table has no parts (it is empty, or its engine does not store parts).")];
    const active = table.rows.filter((row) => cellOf(table, row, "active") === "1");
    const rows = active.reduce((sum, row) => sum + Number(cellOf(table, row, "rows") || 0), 0);
    const bytes = active.reduce((sum, row) => sum + Number(cellOf(table, row, "bytes_on_disk") || 0), 0);
    const summary = h(
      "div",
      { class: "ch-note" },
      `${active.length} active part(s) · ${numberCell(String(rows))} rows · ${formatBytes(bytes)} on disk. Inactive parts are merged away and removed after a while.`,
    );
    return [
      summary,
      dataTable(table.columns, table.rows, {
        render: (column, value) =>
          column === "bytes_on_disk" && value !== null ? formatBytes(Number(value)) : column === "rows" && value !== null ? numberCell(value) : (value ?? ""),
        rowClass: (row) => (cellOf(table, row, "active") === "1" ? "" : "ch-inactive"),
      }),
    ];
  }

  private drawMutations(table: OperationTable): (HTMLElement | null)[] {
    const intro = h(
      "div",
      { class: "ch-note" },
      "Edits of this table run as mutations. Each rewrites the data parts that hold matching rows, in the background.",
    );
    if (table.rows.length === 0) return [intro, emptyHint("No mutations yet.")];
    return [
      intro,
      dataTable(table.columns, table.rows, {
        render: (column, value) =>
          column === "is_done"
            ? value === "1"
              ? "done"
              : "running"
            : column === "command" && value !== null
              ? h("span", { class: "mono", title: value }, oneLine(value, 200))
              : (value ?? ""),
        rowClass: (row) => (cellOf(table, row, "latest_fail_reason") ? "ch-failed" : ""),
        actionsHeader: "",
        actions: (row) => {
          if (cellOf(table, row, "is_done") === "1") return null;
          const id = cellOf(table, row, "mutation_id");
          return button("Kill", () => void this.killMutation(id), "link-button", "KILL MUTATION: stops it (data already rewritten stays)");
        },
      }),
    ];
  }

  private async killMutation(id: string): Promise<void> {
    try {
      if ((await this.api.killMutation(this.ref, id)) !== null) void this.load();
    } catch (error) {
      showError(error, "Kill the mutation");
    }
  }
}

export const partsViewFactory = (ctx: ViewContext): PartsView => new PartsView(ctx);
