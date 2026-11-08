/**
 * The design view of a table, view or collection, from `describe_object` (`ObjectDescription`): columns with their kind
 * and hints, keys, foreign keys in both directions, indexes, engine-specific details and the DDL.
 */
import type { ColumnInfo, ObjectDescription, ObjectRef, ValueKind } from "../../core/contract";
import type { ConnectionApi } from "../../core/connectionApi";
import { showError } from "../../core/errorPanel";
import { copyText, h, spacer, toolButton } from "../../core/dom";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import { DataGrid, gridSetup } from "../grid/grid";
import { textColumn, textRows } from "../grid/legacy";
import "./design.css";

const COLUMN_HEADERS = ["Name", "Type", "Kind", "Nullable", "Key", "Default", "Identity / generated", "Comment"].map(textColumn);
const INDEX_HEADERS = ["Name", "Columns", "Unique", "Method", "Definition"].map(textColumn);

function kindText(kind: ValueKind): string {
  return kind.kind === "array" ? `array of ${kindText(kind.of)}` : kind.kind;
}

function keyText(column: ColumnInfo, description: ObjectDescription): string {
  const parts: string[] = [];
  if (column.hints.primary || description.primaryKey?.columns.includes(column.name)) parts.push("PK");
  if (description.uniqueKeys.some((key) => key.columns.includes(column.name))) parts.push("unique");
  const reference = description.foreignKeys.find((key) => key.columns.includes(column.name));
  if (reference) parts.push(`→ ${reference.refTable}`);
  if (column.hints.vectorDims != null) parts.push(`vector(${column.hints.vectorDims})`);
  if (column.hints.readOnly) parts.push("read-only");
  return parts.join(", ");
}

export class DesignView implements TabContent {
  readonly element: HTMLElement;
  private readonly columns = new DataGrid();
  private readonly indexes = new DataGrid();
  private readonly relations = h("div", { class: "design-relations" });
  private readonly extra = h("div", { class: "design-extra" });
  private readonly ddl = h("pre", { class: "ddl" });
  private readonly comment = h("p", { class: "design-comment" });

  constructor(ctx: ViewContext) {
    if (!ctx.object) throw new Error("This view needs a table, view or collection");
    const object = ctx.object;
    const label = object.schema ? `${object.schema}.${object.name}` : object.name;
    const toolbar = h(
      "div",
      { class: "toolbar" },
      h("span", { class: "toolbar-title" }, object.database ? `${object.database}.${label}` : label),
      spacer(),
      toolButton("refresh", "Reload", () => void this.load(ctx.connection, object), "Describe the object again"),
      toolButton("copy", "Copy DDL", () => void copyText(this.ddl.textContent ?? "")),
    );
    this.comment.hidden = true;
    this.element = h(
      "div",
      { class: "view design-view" },
      toolbar,
      this.comment,
      h("div", { class: "design-columns" }, this.columns.element),
      h(
        "div",
        { class: "design-lower" },
        h("div", { class: "section-title" }, "Indexes"),
        h("div", { class: "design-indexes" }, this.indexes.element),
        this.relations,
        this.extra,
        h("div", { class: "section-title" }, "DDL"),
        this.ddl,
      ),
    );
    void this.load(ctx.connection, object);
  }

  dispose(): void {
    this.columns.dispose();
    this.indexes.dispose();
  }

  private async load(connection: Pick<ConnectionApi, "describe">, object: ObjectRef): Promise<void> {
    try {
      const description = await connection.describe(object);
      this.render(description);
    } catch (error) {
      showError(error, "Load the table design");
    }
  }

  private render(description: ObjectDescription): void {
    this.comment.hidden = !description.comment;
    this.comment.textContent = description.comment ?? "";
    const rows = description.columns.map((column) => [
      column.name,
      column.typeName,
      kindText(column.kind),
      column.nullable ? "YES" : "NO",
      keyText(column, description),
      column.default ?? "",
      column.identity ?? column.generated ?? "",
      column.comment ?? "",
    ]);
    this.columns.load({ ...gridSetup(COLUMN_HEADERS, textRows(rows)), clientFilter: true });
    const indexRows = description.indexes.map((index) => [
      index.name,
      index.columns.join(", "),
      index.primary ? "primary" : index.unique ? "YES" : "NO",
      index.method ?? "",
      index.definition ?? "",
    ]);
    this.indexes.load(gridSetup(INDEX_HEADERS, textRows(indexRows)));
    this.relations.replaceChildren(...this.relationSections(description));
    this.extra.replaceChildren(
      ...(description.extra.length || description.rowEstimate !== null
        ? [
            h("div", { class: "section-title" }, "Details"),
            h(
              "dl",
              { class: "design-details" },
              ...(description.rowEstimate !== null ? [h("dt", {}, "Rows (estimate)"), h("dd", {}, String(description.rowEstimate))] : []),
              ...description.extra.flatMap(([name, value]) => [h("dt", {}, name), h("dd", {}, value)]),
            ),
          ]
        : []),
    );
    this.ddl.textContent = description.ddl ?? "(the engine reports no DDL for this object)";
  }

  private relationSections(description: ObjectDescription): HTMLElement[] {
    const sections: HTMLElement[] = [];
    if (description.foreignKeys.length > 0) {
      sections.push(
        h("div", { class: "section-title" }, "Foreign keys"),
        h(
          "ul",
          { class: "design-list" },
          ...description.foreignKeys.map((key) =>
            h(
              "li",
              {},
              h(
                "code",
                {},
                `${key.name ?? "(unnamed)"}: (${key.columns.join(", ")}) → ${[key.refSchema, key.refTable].filter(Boolean).join(".")} (${key.refColumns.join(", ")})`,
              ),
              key.onDelete || key.onUpdate ? h("span", { class: "field-type" }, ` on delete ${key.onDelete ?? "-"}, on update ${key.onUpdate ?? "-"}`) : null,
            ),
          ),
        ),
      );
    }
    if (description.referencedBy.length > 0) {
      sections.push(
        h("div", { class: "section-title" }, "Referenced by"),
        h(
          "ul",
          { class: "design-list" },
          ...description.referencedBy.map((key) =>
            h("li", {}, h("code", {}, `${[key.schema, key.table].filter(Boolean).join(".")} (${key.columns.join(", ")}) → (${key.refColumns.join(", ")})`)),
          ),
        ),
      );
    }
    return sections;
  }
}
