import { api, errorText } from "./api";
import { toast } from "./dialogs";
import { copyText, h, icon, iconButton } from "./dom";
import type { Navigator } from "./navigator";
import { equalsFilter, rowKey } from "./sql";
import type { Cell, ColumnMeta, Relation, TableRelations, TableTarget } from "./types";

const JSON_START = /^\s*[{[]/;
const LONG_TEXT = 120;
const SPARK_WIDTH = 280;
const SPARK_HEIGHT = 48;

export interface RecordContext {
  target: TableTarget;
  columns: ColumnMeta[];
  row: Cell[];
  primaryKey: string[];
  relations: TableRelations | null;
}

export class RecordPanel {
  readonly element: HTMLElement;
  private readonly body = h("div", { class: "record-body" });
  private readonly navigator: Navigator;

  constructor(navigator: Navigator) {
    this.navigator = navigator;
    this.element = h("aside", { class: "record-panel" }, h("div", { class: "panel-header" }, h("span", {}, "Record")), this.body);
    this.show(null);
  }

  show(context: RecordContext | null): void {
    if (!context) {
      this.body.replaceChildren(h("div", { class: "empty-hint" }, "Select a row to see all of its fields"));
      return;
    }
    const outgoing = context.relations?.outgoing ?? [];
    const fields = context.columns.map((column, index) => this.field(context, column, context.row[index], outgoing));
    this.body.replaceChildren(...fields, ...this.relatedSection(context));
  }

  private field(context: RecordContext, column: ColumnMeta, value: Cell, outgoing: Relation[]): HTMLElement {
    const isKey = column.primary || context.primaryKey.includes(column.name);
    const reference = outgoing.find((relation) => relation.columns.includes(column.name));
    const header = h(
      "div",
      { class: "field-head" },
      isKey ? icon("key", "key-icon") : null,
      h("span", { class: "field-name" }, column.name),
      h("span", { class: "field-type" }, column.vectorDims != null ? `vector(${column.vectorDims})` : column.typeName),
      reference && value !== null ? this.referenceLink(context, reference) : null,
      value !== null && column.vectorDims == null ? iconButton("copy", "Copy value", () => void copyText(value)) : null,
    );
    return h("div", { class: "record-field" }, header, this.valueView(context, column, value));
  }

  private valueView(context: RecordContext, column: ColumnMeta, value: Cell): HTMLElement {
    if (value === null) return h("div", { class: "field-value null" }, "NULL");
    if (column.vectorDims != null) return this.vectorView(context, column, value);
    const json = JSON_START.test(value) ? prettyJson(value) : null;
    if (json) return h("pre", { class: "field-value json" }, json);
    if (value.length > LONG_TEXT || value.includes("\n")) return h("pre", { class: "field-value text" }, value);
    return h("div", { class: "field-value" }, value);
  }

  private vectorView(context: RecordContext, column: ColumnMeta, summary: string): HTMLElement {
    const details = h("div", { class: "vector-details" });
    const key = rowKey(context.columns, context.row, context.primaryKey);
    const loadValues = async () => {
      try {
        const values = await api.fetchVector(context.target.connectionId, context.target.database, context.target.table, column.name, key);
        details.replaceChildren(sparkline(values), h("div", { class: "vector-stats" }, describeVector(values)));
        return values;
      } catch (error) {
        toast(errorText(error), "error");
        return null;
      }
    };
    const actions = h(
      "div",
      { class: "vector-actions" },
      actionLink("Find similar", () => this.navigator.openVectorSearch(context.target, column.name, key)),
      actionLink("Show values", () => void loadValues()),
      actionLink("Copy vector", () => void loadValues().then((values) => values && copyText(`[${values.join(",")}]`))),
    );
    return h("div", { class: "field-value vector" }, h("div", { class: "mono" }, summary), actions, details);
  }

  private referenceLink(context: RecordContext, relation: Relation): HTMLElement {
    const label = `→ ${relation.referencedTable}`;
    return actionLink(label, () => {
      const filter = equalsFilter(
        relation.columns.map((column, index) => ({
          column: relation.referencedColumns[index],
          value: context.row[context.columns.findIndex((meta) => meta.name === column)] ?? null,
        })),
      );
      this.navigator.openTable({ ...context.target, database: relation.referencedDatabase, table: relation.referencedTable, kind: "table" }, filter);
    });
  }

  private relatedSection(context: RecordContext): HTMLElement[] {
    const incoming = context.relations?.incoming ?? [];
    if (incoming.length === 0) return [];
    const links = incoming.map((relation) =>
      h(
        "div",
        { class: "related-row" },
        icon("table"),
        actionLink(`${relation.table}`, () => this.openReferencing(context, relation)),
        h("span", { class: "field-type" }, `via ${relation.columns.join(", ")}`),
      ),
    );
    return [h("div", { class: "section-title" }, "Referenced by"), h("div", { class: "related-list" }, ...links)];
  }

  private openReferencing(context: RecordContext, relation: Relation): void {
    const filter = equalsFilter(
      relation.columns.map((column, index) => ({
        column,
        value: context.row[context.columns.findIndex((meta) => meta.name === relation.referencedColumns[index])] ?? null,
      })),
    );
    this.navigator.openTable({ ...context.target, database: relation.database, table: relation.table, kind: "table" }, filter);
  }
}

function actionLink(label: string, run: () => void): HTMLElement {
  const link = h("button", { class: "link-button", type: "button" }, label);
  link.addEventListener("click", run);
  return link;
}

function prettyJson(value: string): string | null {
  try {
    return JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    return null;
  }
}

function describeVector(values: number[]): string {
  const norm = Math.sqrt(values.reduce((sum, value) => sum + value * value, 0));
  const min = Math.min(...values);
  const max = Math.max(...values);
  return `${values.length} dims · ‖v‖ ${norm.toFixed(4)} · min ${min.toFixed(4)} · max ${max.toFixed(4)}`;
}

function sparkline(values: number[]): SVGSVGElement {
  const namespace = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(namespace, "svg");
  svg.setAttribute("viewBox", `0 0 ${SPARK_WIDTH} ${SPARK_HEIGHT}`);
  svg.setAttribute("class", "sparkline");
  const limit = Math.max(...values.map(Math.abs), Number.EPSILON);
  const middle = SPARK_HEIGHT / 2;
  const step = SPARK_WIDTH / Math.max(values.length - 1, 1);
  const points = values.map((value, index) => `${(index * step).toFixed(1)},${(middle - (value / limit) * (middle - 2)).toFixed(1)}`);
  const baseline = document.createElementNS(namespace, "line");
  baseline.setAttribute("x1", "0");
  baseline.setAttribute("x2", String(SPARK_WIDTH));
  baseline.setAttribute("y1", String(middle));
  baseline.setAttribute("y2", String(middle));
  baseline.setAttribute("class", "spark-baseline");
  const line = document.createElementNS(namespace, "polyline");
  line.setAttribute("points", points.join(" "));
  line.setAttribute("class", "spark-line");
  svg.append(baseline, line);
  return svg;
}
