/**
 * The definition of an index or trigger (objects without rows): what `describe_object` reports and the stored SQL, through
 * the shared view registry.
 */
import type { ConnectionApi } from "../../core/connectionApi";
import type { ObjectDescription, ObjectRef } from "../../core/contract";
import { copyText, h, spacer, toolButton } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import "./definition.css";
import { typeLabel } from "./typeDisplay";

export class DefinitionView implements TabContent {
  readonly element: HTMLElement;
  private readonly details = h("div", { class: "definition-details" });
  private readonly ddl = h("pre", { class: "ddl" });

  constructor(ctx: ViewContext) {
    if (!ctx.object) throw new Error("This view needs an object");
    const object = ctx.object;
    const name = [object.database, object.name].filter(Boolean).join(".");
    this.element = h(
      "div",
      { class: "view definition-view" },
      h(
        "div",
        { class: "toolbar" },
        h("span", { class: "toolbar-title" }, `${name} · ${object.kind}`),
        spacer(),
        toolButton("refresh", "Reload", () => void this.load(ctx.connection, object), "Describe the object again"),
        toolButton("copy", "Copy SQL", () => void copyText(this.ddl.textContent ?? "")),
      ),
      this.details,
      h("div", { class: "section-title" }, "SQL"),
      this.ddl,
    );
    void this.load(ctx.connection, object);
  }

  private async load(connection: Pick<ConnectionApi, "describe">, object: ObjectRef): Promise<void> {
    try {
      this.render(await connection.describe(object));
    } catch (error) {
      showError(error, `Load the definition of ${object.name}`);
    }
  }

  private render(description: ObjectDescription): void {
    this.details.replaceChildren(
      ...(description.columns.length > 0
        ? [
            h("div", { class: "section-title" }, "Columns"),
            h(
              "dl",
              { class: "design-details" },
              ...description.columns.flatMap((column) => [h("dt", {}, column.name), h("dd", {}, typeLabel(column.typeName))]),
            ),
          ]
        : []),
      ...(description.extra.length > 0
        ? [
            h("div", { class: "section-title" }, "Details"),
            h("dl", { class: "design-details" }, ...description.extra.flatMap(([name, value]) => [h("dt", {}, name), h("dd", {}, value)])),
          ]
        : []),
    );
    this.ddl.textContent = description.ddl ?? "(SQLite stores no SQL for this object)";
  }
}

export const definitionView = async () => (ctx: ViewContext) => new DefinitionView(ctx);
