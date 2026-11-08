import { showError } from "../../core/errorPanel";
import { api } from "../../core/api";
import { copyText, h, spacer, toolButton } from "../../core/dom";
import { DataGrid, gridSetup, textColumn } from "../grid/grid";
import type { TabContent } from "../../core/tabs";
import type { TableTarget } from "../../core/types";

const DESIGN_COLUMNS = ["Name", "Type", "Nullable", "Key", "Default", "Extra", "Comment"].map(textColumn);

export class DesignView implements TabContent {
  readonly element: HTMLElement;
  private readonly grid = new DataGrid();
  private readonly ddl = h("pre", { class: "ddl" });

  constructor(target: TableTarget) {
    const toolbar = h(
      "div",
      { class: "toolbar" },
      h("span", { class: "toolbar-title" }, `${target.database}.${target.table}`),
      spacer(),
      toolButton("copy", "Copy DDL", () => void copyText(this.ddl.textContent ?? "")),
    );
    this.element = h(
      "div",
      { class: "view design-view" },
      toolbar,
      h("div", { class: "design-columns" }, this.grid.element),
      h("div", { class: "section-title" }, "DDL"),
      this.ddl,
    );
    void this.load(target);
  }

  dispose(): void {
    this.grid.dispose();
  }

  private async load(target: TableTarget): Promise<void> {
    try {
      const design = await api.describeTable(target.connectionId, target.database, target.table);
      const rows = design.columns.map((column) => [
        column.name,
        column.columnType,
        column.nullable ? "YES" : "NO",
        column.key,
        column.defaultValue,
        column.extra,
        column.comment,
      ]);
      this.grid.load({ ...gridSetup(DESIGN_COLUMNS, rows), clientFilter: true });
      this.ddl.textContent = design.ddl;
    } catch (error) {
      showError(error, "Load the table design");
    }
  }
}
