/**
 * The record panel beside a grid: every field of the selected row (typed values: JSON, documents and arrays pretty-printed,
 * NULL spelled out), links along foreign keys in both directions (from `ObjectDescription`), and vector tools. Everything
 * it does goes through callbacks from the owning view, so it holds no api or engine knowledge of its own.
 */
import type { ColumnInfo, ForeignKeyInfo, ObjectDescription, ObjectRef, ReferencingKey, Value } from "../../core/contract";
import { showError } from "../../core/errorPanel";
import { copyText, h, icon, iconButton } from "../../core/dom";
import { displayValue } from "../../core/values";
import { editText } from "../grid/cellValue";
import { formatFilter, type ColumnFilters } from "../grid/columnFilter";

const LONG_TEXT = 120;
const SPARK_WIDTH = 280;
const SPARK_HEIGHT = 48;
const NULL: Value = { t: "null", v: null };

export interface RecordVectorTools {
  load(column: ColumnInfo): Promise<number[]>;
  findSimilar(column: ColumnInfo): void;
}

export interface RecordContext {
  columns: ColumnInfo[];
  row: Value[];
  keyColumns: string[];
  /** The object (for foreign-key links), when it was described. */
  description: ObjectDescription | null;
  /** Opens a column of this row in the large cell editor. */
  openEditor: ((column: number) => void) | null;
  /** Opens another object's data with column filters (foreign keys). */
  openRelated: ((object: ObjectRef, filters: ColumnFilters) => void) | null;
  /** Vector columns: load the whole vector, search for similar rows (engines with the `vector` capability). */
  vector: RecordVectorTools | null;
}

/** The filter text that selects exactly `value`. */
export function equalsFilterText(value: Value): string {
  return value.t === "null" ? "null" : formatFilter("eq", [displayValue(value) ?? ""]);
}

export class RecordPanel {
  readonly element: HTMLElement;
  private readonly body = h("div", { class: "record-body" });

  constructor() {
    this.element = h("aside", { class: "record-panel" }, h("div", { class: "panel-header" }, h("span", {}, "Record")), this.body);
    this.show(null);
  }

  show(context: RecordContext | null): void {
    if (!context) {
      this.body.replaceChildren(h("div", { class: "empty-hint" }, "No row selected"));
      return;
    }
    const fields = context.columns.map((_, index) => this.field(context, index, context.row[index] ?? NULL));
    this.body.replaceChildren(...fields, ...this.relatedSection(context));
  }

  private field(context: RecordContext, index: number, value: Value): HTMLElement {
    const column = context.columns[index];
    const isKey = Boolean(column.hints.primary) || context.keyColumns.includes(column.name);
    const reference = context.description?.foreignKeys.find((key) => key.columns.includes(column.name));
    const vector = column.hints.vectorDims != null;
    const shown = displayValue(value);
    const long = shown !== null && !vector && (shown.length > LONG_TEXT || shown.includes("\n") || isStructured(value));
    const openEditor = context.openEditor;
    const header = h(
      "div",
      { class: "field-head" },
      isKey ? icon("key", "key-icon") : null,
      h("span", { class: "field-name" }, column.name),
      h("span", { class: "field-type" }, vector ? `vector(${column.hints.vectorDims})` : column.typeName),
      reference && shown !== null && context.openRelated ? this.referenceLink(context, reference) : null,
      long && openEditor ? iconButton("expand", "Open in editor (Shift+Enter in the grid)", () => openEditor(index)) : null,
      shown !== null && !vector ? iconButton("copy", "Copy value", () => void copyText(shown)) : null,
    );
    return h("div", { class: "record-field" }, header, this.valueView(context, column, value));
  }

  private valueView(context: RecordContext, column: ColumnInfo, value: Value): HTMLElement {
    const shown = displayValue(value);
    if (shown === null) return h("div", { class: "field-value null" }, "NULL");
    if (column.hints.vectorDims != null) return this.vectorView(context, column, shown);
    if (isStructured(value)) return h("pre", { class: "field-value json" }, editText(value, true));
    if (shown.length > LONG_TEXT || shown.includes("\n")) return h("pre", { class: "field-value text" }, shown);
    return h("div", { class: "field-value" }, shown);
  }

  private vectorView(context: RecordContext, column: ColumnInfo, summary: string): HTMLElement {
    const tools = context.vector;
    if (!tools) return h("div", { class: "field-value vector" }, h("div", { class: "mono" }, summary));
    const details = h("div", { class: "vector-details" });
    const loadValues = async () => {
      try {
        const values = await tools.load(column);
        details.replaceChildren(sparkline(values), h("div", { class: "vector-stats" }, describeVector(values)));
        return values;
      } catch (error) {
        showError(error, "Load the record");
        return null;
      }
    };
    const actions = h(
      "div",
      { class: "vector-actions" },
      actionLink("Find similar", () => tools.findSimilar(column)),
      actionLink("Show values", () => void loadValues()),
      actionLink("Copy vector", () => void loadValues().then((values) => values && copyText(`[${values.join(",")}]`))),
    );
    return h("div", { class: "field-value vector" }, h("div", { class: "mono" }, summary), actions, details);
  }

  private referenceLink(context: RecordContext, key: ForeignKeyInfo): HTMLElement {
    const own = context.description?.object;
    return actionLink(`→ ${key.refTable}`, () => {
      const filters: ColumnFilters = {};
      key.columns.forEach((name, index) => {
        const position = context.columns.findIndex((column) => column.name === name);
        filters[key.refColumns[index] ?? name] = equalsFilterText(context.row[position] ?? NULL);
      });
      const target: ObjectRef = {
        database: key.refDatabase ?? own?.database ?? null,
        schema: key.refSchema ?? own?.schema ?? null,
        name: key.refTable,
        kind: "table",
      };
      context.openRelated?.(target, filters);
    });
  }

  private relatedSection(context: RecordContext): HTMLElement[] {
    const incoming = context.description?.referencedBy ?? [];
    if (incoming.length === 0 || !context.openRelated) return [];
    const links = incoming.map((key) =>
      h(
        "div",
        { class: "related-row" },
        icon("table"),
        actionLink(key.schema ? `${key.schema}.${key.table}` : key.table, () => this.openReferencing(context, key)),
        h("span", { class: "field-type" }, `via ${key.columns.join(", ")}`),
      ),
    );
    return [h("div", { class: "section-title" }, "Referenced by"), h("div", { class: "related-list" }, ...links)];
  }

  private openReferencing(context: RecordContext, key: ReferencingKey): void {
    context.openRelated?.(referencingObject(context.description, key), referencingFilters(context.columns, context.row, key));
  }
}

/** The object that holds a referencing key (a table in the same database unless the key says otherwise). */
export function referencingObject(description: ObjectDescription | null, key: ReferencingKey): ObjectRef {
  return { database: key.database ?? description?.object.database ?? null, schema: key.schema ?? null, name: key.table, kind: "table" };
}

/** Column filters that select the rows of a referencing table that point at `row`. */
export function referencingFilters(columns: ColumnInfo[], row: Value[], key: ReferencingKey): ColumnFilters {
  const filters: ColumnFilters = {};
  key.columns.forEach((name, index) => {
    const position = columns.findIndex((column) => column.name === key.refColumns[index]);
    filters[name] = equalsFilterText(row[position] ?? NULL);
  });
  return filters;
}

function isStructured(value: Value): boolean {
  if (value.t === "doc" || value.t === "array") return true;
  return value.t === "json" && /^\s*[{[]/.test(value.v);
}

function actionLink(label: string, run: () => void): HTMLElement {
  const link = h("button", { class: "link-button", type: "button" }, label);
  link.addEventListener("click", run);
  return link;
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
