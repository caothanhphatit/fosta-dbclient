/**
 * The right pane of a compare tab: the selected item's header (status, risk, script warnings), its changes table and the
 * side-by-side DDL diff (target on the left, source on the right).
 */
import type { DiffItem, ScriptWarning } from "../../core/contract";
import { h, icon } from "../../core/dom";
import { DataGrid, gridSetup } from "../grid/grid";
import { textColumn, textRows } from "../grid/textColumns";
import { changeRows, displayRisk, riskLabel } from "./compareModel";
import { DdlDiff } from "./ddlDiff";

const CHANGE_HEADERS = ["Part", "Name", "Change", "Field", "Source", "Target", "Note"].map(textColumn);

const STATUS_TEXT: Record<DiffItem["status"], string> = {
  different: "Different: the script alters the target",
  onlyInSource: "Only in the source: the script creates it on the target",
  onlyInTarget: "Only in the target: the script drops it (only when checked)",
  identical: "Identical on both sides",
};

export class CompareDetails {
  readonly element: HTMLElement;
  private readonly title = h("div", { class: "compare-details-title" });
  private readonly status = h("div", { class: "compare-details-status" });
  private readonly warnings = h("ul", { class: "compare-item-warnings" });
  private readonly changes = new DataGrid();
  private readonly changesBox = h("div", { class: "compare-changes-grid" }, this.changes.element);
  private readonly changesTitle = h("div", { class: "section-title" }, "Changes");
  private readonly ddl = new DdlDiff();

  constructor() {
    this.element = h(
      "div",
      { class: "compare-details" },
      h("div", { class: "compare-details-head" }, this.title, this.status, this.warnings),
      this.changesTitle,
      this.changesBox,
      h("div", { class: "section-title" }, "DDL"),
      this.ddl.element,
    );
    this.clear("Select an object to see its differences");
  }

  dispose(): void {
    this.changes.dispose();
  }

  clear(message: string): void {
    this.title.replaceChildren(h("span", { class: "compare-details-placeholder" }, message));
    this.status.textContent = "";
    this.warnings.replaceChildren();
    this.changes.load(gridSetup(CHANGE_HEADERS, []));
    this.ddl.clear("");
  }

  /** `labels`: what the two DDL panes are (the target and the source). */
  show(item: DiffItem, warnings: ScriptWarning[], labels: { target: string; source: string }): void {
    const risk = displayRisk(item);
    this.title.replaceChildren(
      ...[
        icon(item.kind === "view" ? "view" : "table"),
        h("span", { class: "compare-details-name" }, item.name),
        h("span", { class: "compare-details-kind" }, item.kind),
        risk !== "none" ? h("span", { class: `risk-badge risk-${risk}` }, riskLabel(risk)) : null,
      ].filter((node): node is HTMLElement => node !== null),
    );
    this.status.textContent = STATUS_TEXT[item.status];
    this.warnings.replaceChildren(...warnings.map((warning) => h("li", { class: `risk-${warning.risk}` }, warning.message)));
    const rows = changeRows(item);
    this.changesBox.hidden = rows.length === 0;
    this.changesTitle.hidden = rows.length === 0;
    this.changes.load(gridSetup(CHANGE_HEADERS, textRows(rows)));
    this.ddl.show(item.targetDdl, item.sourceDdl, { left: `Target · ${labels.target}`, right: `Source · ${labels.source}` });
  }
}
