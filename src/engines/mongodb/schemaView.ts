/**
 * The Schema section of a collection workspace: the shape of a sample of the collection (`mongo.schema`). The header
 * picks the sample (size; first documents, a random sample or the documents matching a filter) and whether to look
 * for references; Analyze reads it. Below, one row per dotted path in a tree: presence bar, type chips (mixed types
 * highlighted), array lengths, top values as mini bars or min / max, and an inferred reference. Selecting a path shows
 * its details and actions: "Filter by this field" hands a filter to the Documents query bar, "Copy path" copies it.
 *
 * The result is kept with the tab (compacted, `schemaModel.ts`) and in the tab's store (`mongo.schema`), and its paths
 * join the field paths completion offers; the assistant gets a summary of paths and types, never values.
 */
import { formatNumber, h, icon, iconButton, copyText } from "../../core/dom";
import { showMenuBelow, type MenuItem } from "../../core/contextMenu";
import { toast } from "../../core/dialogs";
import { errorText } from "../../core/errors";
import type { SectionContent, SectionContext } from "../../features/objectWorkspace/sectionWorkspace";
import { readable } from "./bson";
import type { Json } from "./documents";
import { STORE_FIELD_PATHS, STORE_QUERY } from "./documentsView";
import { MongoApi } from "./mongoApi";
import { clampSampleSize, MAX_SAMPLE_SIZE } from "./pipelineModel";
import { parseField, type QueryState } from "./queryModel";
import type { DocumentsRequest } from "./savedQueries";
import {
  parents,
  pathFilters,
  readSchemaState,
  savedSchemaState,
  schemaFieldPaths,
  schemaSummary,
  STORE_SCHEMA,
  typeLabel,
  visiblePaths,
  type SchemaPath,
  type SchemaResult,
  type SchemaSampleMode,
} from "./schemaModel";
import "./mongodb.css";
import "./schema.css";

export function schemaSectionFactory(ctx: SectionContext): SectionContent {
  return new SchemaSection(ctx);
}

const MODE_LABELS: [SchemaSampleMode, string, string][] = [
  ["random", "Random", "A random sample ($sample)"],
  ["first", "First", "The first documents in natural order"],
  ["filtered", "Filtered", "The first documents matching a filter"],
];

/** How long ago `at` was, in words. */
function agoText(at: number): string {
  const seconds = Math.max(0, Math.round((Date.now() - at) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours} h ago`;
  return new Date(at).toLocaleDateString();
}

function valueText(value: Json): HTMLElement {
  const shown = readable(value);
  return h("span", { class: `md-value md-${shown.kind}`, title: shown.title ?? shown.text }, shown.text);
}

class SchemaSection implements SectionContent {
  readonly element: HTMLElement;
  private readonly api: MongoApi;
  private readonly database: string;
  private readonly collection: string;
  private readonly sizeInput: HTMLInputElement;
  private readonly filterInput: HTMLInputElement;
  private readonly filterField: HTMLElement;
  private readonly refsInput: HTMLInputElement;
  private readonly analyzeButton: HTMLButtonElement;
  private readonly modeButtons = new Map<SchemaSampleMode, HTMLButtonElement>();
  private readonly meta = h("span", { class: "sc-meta" });
  private readonly info = h("div", { class: "sc-info", role: "status", hidden: true });
  private readonly searchInput: HTMLInputElement;
  private readonly rows = h("div", { class: "sc-rows", role: "treegrid", "aria-label": "Fields" });
  private readonly detail = h("aside", { class: "sc-detail", "aria-label": "Field details" });
  private mode: SchemaSampleMode;
  private size: number;
  private filter: string;
  private references = true;
  private result: SchemaResult | null;
  private at: number | null;
  private collapsed: Set<string>;
  private selected: string | null = null;
  private lastError: string | null = null;
  private running = false;
  private generation = 0;
  private disposed = false;

  constructor(private readonly ctx: SectionContext) {
    const object = ctx.view.object;
    this.database = object?.database ?? "";
    this.collection = object?.name ?? "";
    this.api = new MongoApi(ctx.view.connection, { database: this.database, collection: this.collection });
    const saved = readSchemaState(ctx.savedState);
    this.mode = saved.sampleMode;
    this.size = saved.sampleSize;
    this.filter = saved.filter;
    this.result = saved.result;
    this.at = saved.at;
    this.collapsed = new Set(saved.collapsed);

    this.sizeInput = h("input", {
      class: "input sc-size",
      type: "number",
      min: "1",
      max: String(MAX_SAMPLE_SIZE),
      step: "100",
      value: String(this.size),
      "aria-label": "Sample size",
      title: `Documents analysed (at most ${formatNumber(MAX_SAMPLE_SIZE)})`,
    });
    this.sizeInput.addEventListener("change", () => {
      this.size = clampSampleSize(this.sizeInput.value);
      this.sizeInput.value = String(this.size);
      this.ctx.stateChanged();
    });
    const modes = h("div", { class: "segmented sc-modes", role: "group", "aria-label": "Sample" });
    for (const [mode, label, title] of MODE_LABELS) {
      const button = h("button", { class: "segment", type: "button", title, "aria-pressed": "false" }, label);
      button.addEventListener("click", () => this.setMode(mode));
      this.modeButtons.set(mode, button);
      modes.append(button);
    }
    this.filterInput = h("input", {
      class: "input sc-filter",
      type: "text",
      spellcheck: false,
      placeholder: "{ status: 'paid' }",
      value: this.filter,
      "aria-label": "Sample filter",
    });
    this.filterInput.addEventListener("input", () => {
      this.filter = this.filterInput.value;
      this.ctx.stateChanged();
    });
    this.filterInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter") void this.analyze(true);
    });
    const fromDocuments = iconButton("filter", "Use the Documents filter", () => {
      const query = this.ctx.store.get<QueryState>(STORE_QUERY);
      this.filterInput.value = query?.filter ?? "";
      this.filter = this.filterInput.value;
      this.ctx.stateChanged();
    });
    this.filterField = h("label", { class: "sc-bar-field sc-filter-field" }, h("span", { class: "sc-label" }, "Filter"), this.filterInput, fromDocuments);
    this.refsInput = h("input", { type: "checkbox", checked: this.references, "aria-label": "Find references" });
    this.refsInput.addEventListener("change", () => (this.references = this.refsInput.checked));
    this.analyzeButton = h("button", { class: "btn primary", type: "button", title: "Read the sample and analyse it" }, icon("play"), h("span", {}, "Analyze"));
    this.analyzeButton.addEventListener("click", () => void this.analyze(true));
    const bar = h(
      "div",
      { class: "sc-bar" },
      h("label", { class: "sc-bar-field" }, h("span", { class: "sc-label" }, "Sample"), this.sizeInput),
      modes,
      this.filterField,
      h(
        "label",
        { class: "sc-bar-check", title: "Look for ObjectId fields whose values are _ids of another collection (inferred, bounded)" },
        this.refsInput,
        h("span", {}, "Find references"),
      ),
      h("div", { class: "toolbar-end" }, this.meta, this.analyzeButton),
    );
    this.searchInput = h("input", { class: "input sc-search", type: "search", placeholder: "Find a field", "aria-label": "Find a field" });
    this.searchInput.addEventListener("input", () => this.renderRows());
    const head = h(
      "div",
      { class: "sc-row sc-head", role: "row" },
      h("span", { class: "sc-cell sc-name", role: "columnheader" }, "Field"),
      h("span", { class: "sc-cell sc-presence", role: "columnheader" }, "Presence"),
      h("span", { class: "sc-cell sc-types", role: "columnheader" }, "Types"),
      h("span", { class: "sc-cell sc-values", role: "columnheader" }, "Values"),
    );
    this.rows.addEventListener("keydown", (event) => this.onKey(event));
    const table = h("div", { class: "sc-table" }, h("div", { class: "sc-table-bar" }, this.searchInput), head, this.rows);
    this.element = h("div", { class: "view sc-section" }, bar, this.info, h("div", { class: "sc-body" }, table, this.detail));
    this.drawMode();
    if (this.result) {
      this.publish(this.result);
      this.render();
    } else {
      this.render();
      void this.analyze(false);
    }
  }

  dispose(): void {
    this.disposed = true;
  }

  saveState(): unknown {
    return savedSchemaState({
      sampleMode: this.mode,
      sampleSize: this.size,
      filter: this.filter,
      result: this.result,
      at: this.at,
      collapsed: [...this.collapsed],
    });
  }

  assistantContext(): string | null {
    const lines: string[] = [];
    if (this.result) lines.push(schemaSummary(this.result, { database: this.database, collection: this.collection }));
    else lines.push(`Schema of ${this.database}.${this.collection}: not analysed yet.`);
    if (this.selected) lines.push(`Selected field: ${this.selected}`);
    if (this.lastError) lines.push(`Last error: ${this.lastError}`);
    return lines.join("\n");
  }

  // --- Running ------------------------------------------------------------------------------------------------------

  private setMode(mode: SchemaSampleMode): void {
    if (this.mode === mode) return;
    this.mode = mode;
    if (mode === "filtered" && !this.filter.trim()) {
      const query = this.ctx.store.get<QueryState>(STORE_QUERY);
      if (query?.filter.trim()) {
        this.filter = query.filter;
        this.filterInput.value = this.filter;
      }
    }
    this.drawMode();
    this.ctx.stateChanged();
    if (mode === "filtered") this.filterInput.focus();
  }

  private drawMode(): void {
    for (const [mode, button] of this.modeButtons) button.setAttribute("aria-pressed", String(mode === this.mode));
    this.filterField.hidden = this.mode !== "filtered";
  }

  /** Reads and analyses the sample; `ask`: the user asked, so a workspace that asks before reads asks. */
  private async analyze(ask: boolean): Promise<void> {
    let filter: string | undefined;
    if (this.mode === "filtered") {
      const parsed = parseField("filter", this.filter);
      if (!parsed.ok) {
        this.showInfo(`Filter: ${parsed.error.message}`, "error");
        this.lastError = `Filter: ${parsed.error.message}`;
        this.filterInput.focus();
        return;
      }
      filter = parsed.json ?? undefined;
    }
    const generation = ++this.generation;
    this.running = true;
    this.analyzeButton.disabled = true;
    this.showInfo(`Analysing ${formatNumber(this.size)} documents…`, "info");
    try {
      const result = await this.api.schema(
        {
          database: this.database,
          collection: this.collection,
          sampleMode: this.mode,
          sampleSize: this.size,
          ...(filter ? { filter } : {}),
          references: this.references,
        },
        ask,
      );
      if (this.disposed || generation !== this.generation) return;
      if (!result) {
        this.showInfo(this.result ? null : "Press Analyze to read a sample of the collection.", "info");
        return;
      }
      this.result = result;
      this.at = Date.now();
      this.lastError = null;
      this.publish(result);
      this.showInfo(result.references.note, "info");
      this.ctx.stateChanged();
      this.render();
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.lastError = errorText(error);
      if (ask || this.result) this.showInfo(this.lastError, "error");
      else this.showInfo("Press Analyze to read a sample of the collection.", "info");
    } finally {
      if (generation === this.generation) {
        this.running = false;
        this.analyzeButton.disabled = false;
        this.drawMeta();
      }
    }
  }

  /** Shares the result with the other sections: the schema, and its paths for completion. */
  private publish(result: SchemaResult): void {
    this.ctx.store.set(STORE_SCHEMA, result);
    const known = this.ctx.store.get<string[]>(STORE_FIELD_PATHS) ?? [];
    this.ctx.store.set(STORE_FIELD_PATHS, [...new Set([...schemaFieldPaths(result), ...known])].slice(0, 600));
  }

  private showInfo(message: string | null, kind: "info" | "error"): void {
    this.info.hidden = !message;
    this.info.classList.toggle("sc-info-error", kind === "error");
    this.info.replaceChildren(...(message ? [icon(kind === "error" ? "x" : "health", "sc-info-icon"), h("span", {}, message)] : []));
  }

  // --- Rendering ----------------------------------------------------------------------------------------------------

  private render(): void {
    this.drawMeta();
    this.renderRows();
    this.renderDetail();
  }

  private drawMeta(): void {
    const result = this.result;
    if (!result || this.running) {
      this.meta.textContent = "";
      return;
    }
    const parts = [
      `${formatNumber(result.sample.count)} ${result.sample.count === 1 ? "document" : "documents"} (${result.sample.mode})`,
      `${formatNumber(result.paths.length)} fields${result.truncated ? "+" : ""}`,
      `${formatNumber(result.elapsedMs)} ms`,
    ];
    if (this.at) parts.push(agoText(this.at));
    this.meta.textContent = parts.join(" · ");
    this.meta.title = this.at ? `Analysed ${new Date(this.at).toLocaleString()}` : "";
  }

  private renderRows(): void {
    const result = this.result;
    if (!result) {
      this.rows.replaceChildren(h("div", { class: "sc-empty" }, this.running ? "Reading the sample…" : "No schema yet. Press Analyze."));
      return;
    }
    if (result.paths.length === 0) {
      this.rows.replaceChildren(h("div", { class: "sc-empty" }, "The sample has no documents."));
      return;
    }
    const withChildren = parents(result.paths);
    const search = this.searchInput.value;
    const shown = visiblePaths(result.paths, this.collapsed, search);
    const rows = shown.map((entry) => this.row(entry, withChildren.has(entry.path), !!search.trim()));
    if (result.truncated || result.depthCapped)
      rows.push(
        h(
          "div",
          { class: "sc-empty" },
          result.truncated ? "More fields than the analysis keeps: only the first ones are listed." : "Some documents nest deeper than the analysis reads.",
        ),
      );
    if (shown.length === 0) rows.unshift(h("div", { class: "sc-empty" }, "No field matches."));
    this.rows.replaceChildren(...rows);
  }

  private row(entry: SchemaPath, hasChildren: boolean, flat: boolean): HTMLElement {
    const expanded = !this.collapsed.has(entry.path);
    const toggle =
      hasChildren && !flat
        ? h("button", { class: "sc-toggle", type: "button", tabindex: "-1", "aria-label": expanded ? "Collapse" : "Expand" }, icon("chevron"))
        : h("span", { class: "sc-toggle-space" });
    if (hasChildren && !flat) {
      toggle.setAttribute("aria-expanded", String(expanded));
      toggle.addEventListener("click", (event) => {
        event.stopPropagation();
        this.toggle(entry.path);
      });
    }
    const name = h(
      "span",
      { class: "sc-cell sc-name", style: `padding-left: ${(flat ? 0 : entry.depth) * 16 + 4}px` },
      toggle,
      h("span", { class: "sc-field", title: entry.path }, flat ? entry.path : entry.name),
      entry.reference
        ? h(
            "span",
            {
              class: "dt-ref sc-ref",
              title: `Inferred: ${entry.reference.matched} of ${entry.reference.probed} sampled ids are _ids of ${entry.reference.collection}`,
            },
            icon("diagram", "dt-ref-icon"),
            entry.reference.collection,
          )
        : null,
    );
    const presence = h(
      "span",
      { class: "sc-cell sc-presence", title: `${formatNumber(entry.count)} of ${formatNumber(this.result?.sample.count ?? 0)} documents` },
      h("span", { class: "sc-bar-track" }, h("span", { class: "sc-bar-fill", style: `width: ${Math.min(100, entry.percent)}%` })),
      h("span", { class: "sc-percent" }, `${entry.percent}%`),
    );
    const types = h("span", { class: "sc-cell sc-types" }, ...this.typeChips(entry));
    const values = h("span", { class: "sc-cell sc-values" }, ...this.valuesSummary(entry));
    const row = h(
      "div",
      {
        class: `sc-row${entry.path === this.selected ? " selected" : ""}`,
        role: "row",
        tabindex: entry.path === this.selected || (!this.selected && entry === this.result?.paths[0]) ? "0" : "-1",
        "aria-selected": String(entry.path === this.selected),
        "aria-level": String(entry.depth + 1),
        "data-path": entry.path,
      },
      name,
      presence,
      types,
      values,
    );
    if (hasChildren && !flat) row.setAttribute("aria-expanded", String(expanded));
    row.addEventListener("click", () => this.select(entry.path));
    return row;
  }

  private typeChips(entry: SchemaPath): HTMLElement[] {
    const total = entry.types.reduce((sum, type) => sum + type.count, 0) || 1;
    const chips = entry.types.map((type) =>
      h(
        "span",
        {
          class: `sc-chip sc-type-${type.type}${entry.mixed && type.type !== "null" ? " mixed" : ""}`,
          title: `${typeLabel(type.type)}: ${formatNumber(type.count)} (${Math.round((type.count / total) * 100)}%)`,
        },
        typeLabel(type.type),
      ),
    );
    if (entry.mixed) chips.push(h("span", { class: "sc-mixed", title: "More than one type besides null" }, "mixed"));
    return chips;
  }

  private valuesSummary(entry: SchemaPath): (HTMLElement | string)[] {
    const parts: (HTMLElement | string)[] = [];
    if (entry.array) {
      const elementTypes = entry.array.elementTypes.map((type) => typeLabel(type.type)).join(", ") || "empty";
      parts.push(
        h(
          "span",
          { class: "sc-array", title: `Arrays of ${elementTypes}` },
          `[${entry.array.minLength}–${entry.array.maxLength}, avg ${entry.array.avgLength}] `,
          h("span", { class: "sc-dim" }, elementTypes),
        ),
      );
    }
    if (entry.topValues && entry.distinct !== null && entry.distinct <= 20) {
      const top = entry.topValues.slice(0, 3);
      const max = top[0]?.count || 1;
      for (const item of top)
        parts.push(
          h(
            "span",
            { class: "sc-top", title: `${formatNumber(item.count)} times` },
            h("span", { class: "sc-top-bar", style: `width: ${Math.max(4, Math.round((item.count / max) * 28))}px` }),
            valueText(item.value),
          ),
        );
      if (entry.distinct > top.length) parts.push(h("span", { class: "sc-dim" }, `+${entry.distinct - top.length}`));
    } else if (entry.min !== null && entry.max !== null) {
      parts.push(h("span", { class: "sc-range" }, valueText(entry.min), h("span", { class: "sc-dim" }, " … "), valueText(entry.max)));
    } else if (entry.samples.length > 0 && !entry.array) {
      parts.push(h("span", { class: "sc-range" }, valueText(entry.samples[0])));
    }
    return parts;
  }

  private renderDetail(): void {
    const entry = this.result?.paths.find((item) => item.path === this.selected);
    if (!entry) {
      this.detail.replaceChildren(h("div", { class: "sc-detail-empty" }, this.result ? "Select a field to see its values and filter by it." : ""));
      return;
    }
    const filterButton = h("button", { class: "btn", type: "button", "aria-haspopup": "menu" }, icon("filter"), h("span", {}, "Filter by this field"));
    filterButton.addEventListener("click", () => showMenuBelow(filterButton, this.filterItems(entry)));
    const copyButton = h("button", { class: "btn", type: "button" }, icon("copy"), h("span", {}, "Copy path"));
    copyButton.addEventListener("click", () => void copyText(entry.path).then(() => toast(`Copied ${entry.path}`, "success")));
    const sections: HTMLElement[] = [
      h("div", { class: "sc-detail-head" }, h("div", { class: "sc-detail-path", title: entry.path }, entry.path)),
      h("div", { class: "sc-detail-actions" }, filterButton, copyButton),
      this.fact("Presence", `${entry.percent}% (${formatNumber(entry.count)} of ${formatNumber(this.result?.sample.count ?? 0)} documents)`),
      this.fact("Types", h("span", { class: "sc-chips" }, ...this.typeChips(entry))),
    ];
    if (entry.array)
      sections.push(
        this.fact("Array length", `min ${entry.array.minLength}, avg ${entry.array.avgLength}, max ${entry.array.maxLength}`),
        this.fact("Elements", entry.array.elementTypes.map((type) => `${typeLabel(type.type)} (${formatNumber(type.count)})`).join(", ") || "none"),
      );
    if (entry.min !== null && entry.max !== null) sections.push(this.fact("Range", h("span", {}, valueText(entry.min), " … ", valueText(entry.max))));
    sections.push(this.fact("Distinct", entry.distinct === null ? "many (more than 100 in the sample)" : formatNumber(entry.distinct)));
    if (entry.reference)
      sections.push(
        this.fact("Reference", `${entry.reference.collection}._id (inferred: ${entry.reference.matched} of ${entry.reference.probed} sampled ids found)`),
      );
    if (entry.topValues && entry.topValues.length > 0) {
      const max = entry.topValues[0].count || 1;
      const list = h("div", { class: "sc-top-list" });
      for (const item of entry.topValues) {
        const button = h(
          "button",
          { class: "sc-top-item", type: "button", title: "Filter the documents by this value" },
          h("span", { class: "sc-top-track" }, h("span", { class: "sc-top-fill", style: `width: ${Math.round((item.count / max) * 100)}%` })),
          valueText(item.value),
          h("span", { class: "sc-dim sc-top-count" }, formatNumber(item.count)),
        );
        const filter = pathFilters({ ...entry, topValues: [item], mixed: false }).at(-1);
        if (filter && filter.label.startsWith("Equals")) button.addEventListener("click", () => this.deliverFilter(filter.filter));
        else button.disabled = true;
        list.append(button);
      }
      sections.push(h("div", { class: "sc-fact sc-fact-block" }, h("div", { class: "sc-fact-label" }, "Top values"), list));
    } else if (entry.samples.length > 0) {
      sections.push(
        h(
          "div",
          { class: "sc-fact sc-fact-block" },
          h("div", { class: "sc-fact-label" }, "Sample values"),
          h("div", { class: "sc-samples" }, ...entry.samples.map((value) => h("div", {}, valueText(value)))),
        ),
      );
    }
    this.detail.replaceChildren(...sections);
  }

  private fact(label: string, value: string | HTMLElement): HTMLElement {
    return h("div", { class: "sc-fact" }, h("div", { class: "sc-fact-label" }, label), h("div", { class: "sc-fact-value" }, value));
  }

  private filterItems(entry: SchemaPath): MenuItem[] {
    return pathFilters(entry).map((filter) => ({ label: filter.label, detail: filter.filter, icon: "filter", run: () => this.deliverFilter(filter.filter) }));
  }

  private deliverFilter(filter: string): void {
    void this.ctx.deliver("documents", { filter } satisfies DocumentsRequest);
  }

  // --- Selection and keys -------------------------------------------------------------------------------------------

  private select(path: string, focus = true): void {
    this.selected = path;
    for (const row of this.rows.querySelectorAll<HTMLElement>(".sc-row")) {
      const on = row.dataset.path === path;
      row.classList.toggle("selected", on);
      row.setAttribute("aria-selected", String(on));
      row.tabIndex = on ? 0 : -1;
      if (on && focus) row.focus();
    }
    this.renderDetail();
  }

  private toggle(path: string): void {
    if (this.collapsed.has(path)) this.collapsed.delete(path);
    else this.collapsed.add(path);
    this.ctx.stateChanged();
    this.renderRows();
    if (this.selected) this.select(this.selected, false);
  }

  private onKey(event: KeyboardEvent): void {
    const rows = [...this.rows.querySelectorAll<HTMLElement>(".sc-row")];
    const index = rows.findIndex((row) => row === document.activeElement);
    if (index < 0) return;
    const path = rows[index].dataset.path ?? "";
    const move = (to: number) => {
      const target = rows[Math.max(0, Math.min(rows.length - 1, to))];
      if (target?.dataset.path) this.select(target.dataset.path);
    };
    switch (event.key) {
      case "ArrowDown":
        move(index + 1);
        break;
      case "ArrowUp":
        move(index - 1);
        break;
      case "Home":
        move(0);
        break;
      case "End":
        move(rows.length - 1);
        break;
      case "ArrowRight":
        if (rows[index].getAttribute("aria-expanded") === "false") this.toggle(path);
        else return;
        break;
      case "ArrowLeft":
        if (rows[index].getAttribute("aria-expanded") === "true") this.toggle(path);
        else if (path.includes(".")) this.select(path.slice(0, path.lastIndexOf(".")));
        else return;
        break;
      case "Enter": {
        const entry = this.result?.paths.find((item) => item.path === path);
        const button = this.detail.querySelector<HTMLButtonElement>(".sc-detail-actions .btn");
        if (entry && button) showMenuBelow(button, this.filterItems(entry));
        break;
      }
      default:
        return;
    }
    event.preventDefault();
  }
}
