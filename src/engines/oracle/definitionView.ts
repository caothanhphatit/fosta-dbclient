/**
 * The definition of an Oracle object without rows: the source of a package (spec and body), procedure or function, a
 * sequence or a synonym. It shows what `describe_object` reports: status (VALID / INVALID), timestamps, compile errors
 * from ALL_ERRORS, sequence settings or the synonym's target, and the DDL, which runs as is in a query tab (each unit ends
 * with a `/` line).
 */
import type { ConnectionApi } from "../../core/connectionApi";
import type { ObjectDescription, ObjectRef } from "../../core/contract";
import { copyText, h, spacer, toolButton } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import "./oracle.css";

/** Labels for the facts the backend reports under `extra`. */
const LABELS: Record<string, string> = {
  status: "Status",
  bodyStatus: "Body status",
  created: "Created",
  lastDdl: "Last changed",
  target: "Points to",
  minValue: "Minimum",
  maxValue: "Maximum",
  incrementBy: "Increment",
  cycle: "Cycles",
  cacheSize: "Cache",
  lastNumber: "Next cached value",
  ddl: "Definition",
};

/** Facts and compile errors, split out of `extra`. */
export function splitExtra(extra: [string, string][]): { facts: [string, string][]; errors: string[] } {
  const errors = extra.filter(([name]) => /^error\d+$/.test(name)).map(([, text]) => text);
  const facts = extra.filter(([name]) => !/^error\d+$/.test(name)).map(([name, value]): [string, string] => [LABELS[name] ?? name, value]);
  return { facts, errors };
}

export class OracleDefinitionView implements TabContent {
  readonly element: HTMLElement;
  private readonly details = h("div", { class: "definition-details" });
  private readonly errors = h("div", { class: "oracle-compile-errors" });
  private readonly ddl = h("pre", { class: "ddl" });

  constructor(ctx: ViewContext) {
    if (!ctx.object) throw new Error("This view needs an object");
    const object = ctx.object;
    const name = [object.database ?? object.schema, object.name].filter(Boolean).join(".");
    this.element = h(
      "div",
      { class: "view oracle-definition-view" },
      h(
        "div",
        { class: "toolbar" },
        h("span", { class: "toolbar-title" }, `${name} · ${object.kind}`),
        spacer(),
        toolButton("refresh", "Reload", () => void this.load(ctx.connection, object), "Describe the object again"),
        toolButton("copy", "Copy source", () => void copyText(this.ddl.textContent ?? "")),
      ),
      this.details,
      this.errors,
      h("div", { class: "section-title" }, "Source"),
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
    const { facts, errors } = splitExtra(description.extra);
    this.details.replaceChildren(
      ...(description.comment ? [h("p", { class: "design-comment" }, description.comment)] : []),
      ...(facts.length > 0
        ? [
            h(
              "dl",
              { class: "design-details" },
              ...facts.flatMap(([name, value]) => [h("dt", {}, name), h("dd", { class: value === "INVALID" ? "oracle-invalid" : "" }, value)]),
            ),
          ]
        : []),
    );
    this.errors.replaceChildren(
      ...(errors.length > 0 ? [h("div", { class: "section-title" }, "Compile errors"), h("ul", {}, ...errors.map((text) => h("li", {}, text)))] : []),
    );
    this.ddl.textContent = description.ddl ?? "(the server reports no definition for this object)";
  }
}

export const definitionView = async () => (ctx: ViewContext) => new OracleDefinitionView(ctx);
