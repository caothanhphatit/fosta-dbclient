/**
 * The definition of an object without rows: a function, sequence, type or extension. It shows what `describe_object`
 * reports (comment, facts, composite fields, enum labels as chips) and the DDL, through the shared view registry.
 */
import type { ConnectionApi } from "../../core/connectionApi";
import type { ObjectDescription, ObjectRef } from "../../core/contract";
import { copyText, h, spacer, toolButton } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import "./definition.css";
import { enumLabels, shortType } from "./typeDisplay";

export class DefinitionView implements TabContent {
  readonly element: HTMLElement;
  private readonly comment = h("p", { class: "design-comment" });
  private readonly details = h("div", { class: "definition-details" });
  private readonly ddl = h("pre", { class: "ddl" });

  constructor(ctx: ViewContext) {
    if (!ctx.object) throw new Error("This view needs an object");
    const object = ctx.object;
    const name = [object.schema, object.name].filter(Boolean).join(".");
    this.comment.hidden = true;
    this.element = h(
      "div",
      { class: "view definition-view" },
      h(
        "div",
        { class: "toolbar" },
        h("span", { class: "toolbar-title" }, `${name} · ${object.kind}`),
        spacer(),
        toolButton("refresh", "Reload", () => void this.load(ctx.connection, object), "Describe the object again"),
        toolButton("copy", "Copy DDL", () => void copyText(this.ddl.textContent ?? "")),
      ),
      this.comment,
      this.details,
      h("div", { class: "section-title" }, "Definition"),
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
    this.comment.hidden = !description.comment;
    this.comment.textContent = description.comment ?? "";
    const labels = enumLabels(description.extra);
    const facts = description.extra.filter(([name]) => name !== "labels");
    this.details.replaceChildren(
      ...(labels.length > 0
        ? [
            h("div", { class: "section-title" }, "Labels"),
            h("div", { class: "definition-chips" }, ...labels.map((label) => h("span", { class: "definition-chip" }, label))),
          ]
        : []),
      ...(description.columns.length > 0
        ? [
            h("div", { class: "section-title" }, "Fields"),
            h(
              "dl",
              { class: "design-details" },
              ...description.columns.flatMap((column) => [h("dt", {}, column.name), h("dd", {}, shortType(column.typeName))]),
            ),
          ]
        : []),
      ...(facts.length > 0
        ? [
            h("div", { class: "section-title" }, "Details"),
            h("dl", { class: "design-details" }, ...facts.flatMap(([name, value]) => [h("dt", {}, name), h("dd", {}, value)])),
          ]
        : []),
    );
    this.ddl.textContent = description.ddl ?? "(the server reports no definition for this object)";
  }
}

export const definitionView = async () => (ctx: ViewContext) => new DefinitionView(ctx);
