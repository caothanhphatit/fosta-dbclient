/**
 * The Aggregations section of a collection workspace: a pipeline built as stage cards (`stageCard.ts`), each with its
 * editor and the preview of its output. The preview (`mongo.aggregatePreview`) runs on a bounded sample of the
 * collection (size and first / random in the header) after each edit (debounced, never asking: a workspace that asks
 * before reads waits for Preview) and with Preview; every stage shows its count, time and first documents, the stage
 * that failed shows the error, and stages after one that does not parse or writes (`$out` / `$merge`) say why they did
 * not run. Run executes the whole pipeline (`mongo.aggregate`, through the gateway: `$out` / `$merge` are confirmed
 * there) and shows the result in a panel with the Documents section's List / JSON / Table renderers; each run goes to
 * the query history. Code exports the enabled stages as mongosh, Node.js or Python; Save / Saved keep pipelines with
 * the saved queries. The stages, the sample options and the active stage are kept with the open tabs (a pipeline too
 * large to keep is left out; save it by name).
 */
import { api } from "../../core/api";
import type { ObjectRef } from "../../core/contract";
import { confirmDialog, openModal, toast } from "../../core/dialogs";
import { formatNumber, h, icon, iconButton } from "../../core/dom";
import { renderError } from "../../core/errorPanel";
import { errorText } from "../../core/errors";
import { makeSplitter } from "../../core/splitter";
import type { HistoryEntry, SavedQuery } from "../../core/types";
import type { SectionContent, SectionContext } from "../../features/objectWorkspace/sectionWorkspace";
import { gatewayProblem } from "../../features/safety/safety";
import { SchemaCache } from "../../lang/schemaCache";
import { exportPipeline } from "./codegen";
import { openCodeExport } from "./codeExportDialog";
import { DocTree } from "./docTree";
import { documentPaths, type Doc, type Json } from "./documents";
import { STORE_FIELD_PATHS, STORE_QUERY } from "./documentsView";
import { MODES, type DocumentsRenderer, type RendererContext } from "./documentModes";
import { MongoApi, type AggregatePreviewResult, type AggregateResult } from "./mongoApi";
import {
  clampSampleSize,
  duplicateStage,
  enabledPipeline,
  insertStage,
  lookupField,
  moveStage,
  moveStageBy,
  newStage,
  operatorOfText,
  PREVIEW_LIMIT,
  previewPlan,
  readAggregationsState,
  removeStage,
  sampledFields,
  savedStateOf,
  stagesFromQuery,
  stagesOfTexts,
  templateOf,
  textForOperator,
  toggleStage,
  updateStage,
  type PipelinePlan,
  type PipelineStage,
  type SampleMode,
} from "./pipelineModel";
import { EMPTY_QUERY, oneLine, type DocumentsMode, type QueryState } from "./queryModel";
import { pipelineOfSaved, stateOfSaved, type AggregationsRequest, type DocumentsRequest } from "./savedQueries";
import { SavedQueries } from "./savedQueriesMenu";
import { StageCard, type StagePreview } from "./stageCard";
import "./mongodb.css";
import "./aggregations.css";

/** Wait after an edit before the preview runs again. */
const PREVIEW_DELAY_MS = 600;
/** The time bound of one preview (all stages together). */
const PREVIEW_MAX_TIME_MS = 15_000;
/** Documents a full run returns at most. */
const RUN_LIMIT = 200;

export function aggregationsSectionFactory(ctx: SectionContext): SectionContent {
  return new AggregationsSection(ctx);
}

let stageCounter = 0;
const nextId = () => `stage-${++stageCounter}`;

class AggregationsSection implements SectionContent {
  readonly element: HTMLElement;
  private readonly api: MongoApi;
  private readonly schema = new SchemaCache();
  private readonly database: string;
  private readonly collection: string;
  private readonly saved: SavedQueries;
  private readonly list = h("div", { class: "ag-stages", role: "list", "aria-label": "Pipeline stages" });
  private readonly info = h("div", { class: "ag-info", role: "status" });
  private readonly sampleInput: HTMLInputElement;
  private readonly sampleButtons = new Map<SampleMode, HTMLButtonElement>();
  private readonly autoInput: HTMLInputElement;
  private readonly results: ResultsPanel;
  private readonly cards = new Map<string, StageCard>();
  private stages: PipelineStage[];
  private sampleSize: number;
  private sampleMode: SampleMode;
  private autoPreview: boolean;
  private active: number;
  private plan: PipelinePlan | null = null;
  private preview: AggregatePreviewResult | null = null;
  /** The stages the preview shown ran (its results hold for the stages that are still the same, from the first). */
  private previewPipeline: Doc[] | null = null;
  private lastError: string | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private generation = 0;
  private dragging: string | null = null;
  private disposed = false;

  constructor(private readonly ctx: SectionContext) {
    const object = ctx.view.object;
    this.database = object?.database ?? "";
    this.collection = object?.name ?? "";
    this.api = new MongoApi(ctx.view.connection, { database: this.database, collection: this.collection });
    void this.schema.load(() => ctx.view.connection.completionSchema(this.database || null), this.database).catch(() => undefined);
    const saved = readAggregationsState(ctx.savedState);
    this.stages = stagesOfTexts(saved.stages, nextId);
    this.sampleSize = saved.sampleSize;
    this.sampleMode = saved.sampleMode;
    this.autoPreview = saved.autoPreview;
    this.active = Math.min(saved.active, Math.max(0, this.stages.length - 1));

    this.saved = new SavedQueries({
      connectionId: ctx.view.connection.connectionId,
      database: this.database,
      collection: this.collection,
      state: () => EMPTY_QUERY,
      pipeline: () => this.stages.filter((stage) => stage.enabled).map((stage) => stage.text),
      apply: (state) => void ctx.deliver("documents", { query: state } satisfies DocumentsRequest),
      applyPipeline: (pipeline) => void this.setPipeline(pipeline),
      openElsewhere: (query) => this.openSaved(query),
    });

    const savedButton = h(
      "button",
      { class: "tool-button", type: "button", title: "Saved queries and pipelines of this database", "aria-label": "Saved", "aria-haspopup": "dialog" },
      icon("bookmark"),
      h("span", {}, "Saved"),
    );
    savedButton.addEventListener("click", () => void this.saved.open(savedButton));
    const saveButton = h("button", { class: "btn", type: "button", title: "Save the enabled stages under a name" }, "Save");
    saveButton.addEventListener("click", () => void this.saved.save());

    this.sampleInput = h("input", {
      class: "input ag-sample-size",
      type: "number",
      min: "1",
      max: "10000",
      step: "100",
      value: String(this.sampleSize),
      "aria-label": "Sample size",
      title: "Documents the preview reads (at most 10,000)",
    });
    this.sampleInput.addEventListener("change", () => {
      this.sampleSize = clampSampleSize(this.sampleInput.value);
      this.sampleInput.value = String(this.sampleSize);
      this.ctx.stateChanged();
      void this.runPreview(false);
    });
    const modes = h("div", { class: "segmented ag-sample-mode", role: "group", "aria-label": "Sample" });
    for (const [mode, label, title] of [
      ["first", "First", "The first documents in natural order"],
      ["random", "Random", "A random sample ($sample), the same input for every stage"],
    ] as const) {
      const button = h("button", { class: "segment", type: "button", title, "aria-pressed": "false" }, label);
      button.addEventListener("click", () => {
        if (this.sampleMode === mode) return;
        this.sampleMode = mode;
        this.drawSampleMode();
        this.ctx.stateChanged();
        void this.runPreview(false);
      });
      this.sampleButtons.set(mode, button);
      modes.append(button);
    }
    this.autoInput = h("input", { type: "checkbox", checked: this.autoPreview, "aria-label": "Preview after each edit" });
    this.autoInput.addEventListener("change", () => {
      this.autoPreview = this.autoInput.checked;
      this.ctx.stateChanged();
      if (this.autoPreview) void this.runPreview(false);
    });
    const previewButton = h(
      "button",
      { class: "btn", type: "button", title: "Run the preview on the sample (Ctrl+Enter in a stage)" },
      icon("refresh"),
      h("span", {}, "Preview"),
    );
    previewButton.addEventListener("click", () => void this.runPreview(true));
    const codeButton = h(
      "button",
      { class: "tool-button", type: "button", title: "Export the enabled stages as mongosh, Node.js or Python" },
      icon("terminal"),
      h("span", {}, "Code"),
    );
    codeButton.addEventListener("click", () => this.exportCode());
    const runButton = h(
      "button",
      { class: "btn primary", type: "button", title: "Run the whole pipeline on the collection" },
      icon("play"),
      h("span", {}, "Run"),
    );
    runButton.addEventListener("click", () => void this.runFull());

    const bar = h(
      "div",
      { class: "ag-bar" },
      savedButton,
      saveButton,
      h("span", { class: "ag-bar-sep" }),
      h("label", { class: "ag-bar-field" }, h("span", { class: "ag-label" }, "Sample"), this.sampleInput),
      modes,
      h("label", { class: "ag-bar-check", title: "Run the preview after each edit" }, this.autoInput, h("span", {}, "Auto preview")),
      h("div", { class: "toolbar-end" }, previewButton, codeButton, runButton),
    );
    this.results = new ResultsPanel(() => this.ctx.stateChanged());
    this.list.addEventListener("dragover", (event) => this.onDragOver(event));
    this.list.addEventListener("drop", (event) => this.onDrop(event));
    this.list.addEventListener("dragleave", (event) => {
      if (!this.list.contains(event.relatedTarget as Node | null)) this.clearDropMarks();
    });
    const scroller = h("div", { class: "ag-scroll" }, this.list);
    this.element = h("div", { class: "view ag-section" }, bar, this.info, scroller, this.results.element);
    this.drawSampleMode();
    this.renderList();
    if (this.stages.length > 0) void this.runPreview(false);
    else this.showInfo("Add a stage, or open the Documents query as a pipeline (Documents › More › Open as aggregation).");
  }

  dispose(): void {
    this.disposed = true;
    if (this.timer !== null) clearTimeout(this.timer);
    this.saved.dispose();
    for (const card of this.cards.values()) card.dispose();
    this.cards.clear();
    this.results.dispose();
  }

  saveState(): unknown {
    return savedStateOf(this.stages, { sampleSize: this.sampleSize, sampleMode: this.sampleMode, autoPreview: this.autoPreview, active: this.active });
  }

  /** Stages handed over by another section or tab (the Documents query, a saved pipeline). */
  receive(request: unknown): void {
    const pipeline = (request as AggregationsRequest | null)?.pipeline;
    if (Array.isArray(pipeline)) void this.setPipeline(pipeline.filter((stage): stage is string => typeof stage === "string"));
  }

  assistantContext(): string | null {
    const lines = [
      `Aggregation pipeline on ${this.database}.${this.collection}: ${this.stages.length} stages (${this.stages.filter((s) => s.enabled).length} on).`,
    ];
    this.stages.forEach((stage, index) => {
      const text = oneLine(stage.text);
      lines.push(`${index + 1}. [${stage.enabled ? "on" : "off"}] ${text.length > 400 ? `${text.slice(0, 399)}…` : text}`);
    });
    const active = this.stages[this.active];
    if (active) {
      lines.push(`Active stage: ${this.active + 1} (${operatorOfText(active.text) ?? active.operator}).`);
      const input = this.inputDocuments(this.active);
      if (input) {
        const fields = sampledFields(input).slice(0, 40);
        if (fields.length > 0)
          lines.push(`Fields reaching the active stage (sampled): ${fields.map((field) => `${field.path}: ${field.types.join("|")}`).join(", ")}`);
      } else {
        const paths = (this.ctx.store.get<string[]>(STORE_FIELD_PATHS) ?? []).slice(0, 40);
        if (paths.length > 0) lines.push(`Fields of the collection (seen in Documents): ${paths.join(", ")}`);
      }
      const output = this.previewOf(this.active);
      if (output && output.error === null) lines.push(`Active stage output on the sample: ${output.count ?? 0} documents.`);
    }
    if (this.preview) lines.push(`Preview sample: ${this.preview.sample.mode} ${this.preview.sample.size} documents (${this.preview.sample.count} read).`);
    if (this.lastError) lines.push(`Last error: ${this.lastError}`);
    return lines.join("\n");
  }

  // --- The pipeline ------------------------------------------------------------------------------------------------

  /** Replaces the stages (asking first when the pipeline has stages of its own) and previews them. */
  private async setPipeline(texts: string[]): Promise<void> {
    const current = this.stages.map((stage) => stage.text);
    if (current.length > 0 && current.join("\u0000") !== texts.join("\u0000")) {
      const ok = await confirmDialog(`Replace the ${current.length} ${current.length === 1 ? "stage" : "stages"} of the pipeline?`, "Replace");
      if (!ok || this.disposed) return;
    }
    this.stages = stagesOfTexts(texts, nextId);
    this.active = 0;
    this.renderList();
    this.ctx.stateChanged();
    void this.runPreview(true);
  }

  private edit(next: PipelineStage[], options: { focus?: string; handle?: boolean } = {}): void {
    this.stages = next;
    this.renderList();
    this.ctx.stateChanged();
    if (options.focus) {
      const card = this.cards.get(options.focus);
      this.active = this.stages.findIndex((stage) => stage.id === options.focus);
      if (options.handle) card?.focusHandle();
      else card?.focus();
    }
    this.schedulePreview(0);
  }

  private addStage(at: number, operator = "$match"): void {
    const stage = newStage(nextId(), operator);
    this.edit(insertStage(this.stages, stage, at), { focus: stage.id });
  }

  /** Draws the cards in the order of the stages (cards are kept by id, so editors keep their state). */
  private renderList(): void {
    this.active = Math.max(0, Math.min(this.active, this.stages.length - 1));
    const ids = new Set(this.stages.map((stage) => stage.id));
    for (const [id, card] of this.cards) {
      if (ids.has(id)) continue;
      card.dispose();
      this.cards.delete(id);
    }
    const children: HTMLElement[] = [];
    this.stages.forEach((stage, index) => {
      let card = this.cards.get(stage.id);
      if (!card) {
        card = this.createCard(stage);
        this.cards.set(stage.id, card);
      }
      card.update(stage, index);
      card.setActive(index === this.active);
      children.push(this.addButton(index, false), card.element);
    });
    if (this.stages.length === 0) {
      const first = h("button", { class: "btn primary", type: "button" }, icon("plus"), h("span", {}, "Add the first stage"));
      first.addEventListener("click", () => this.addStage(0));
      const fromQuery = h("button", { class: "btn", type: "button" }, icon("filter"), h("span", {}, "Start from the Documents query"));
      fromQuery.addEventListener("click", () => void this.startFromQuery());
      children.push(
        h(
          "div",
          { class: "ag-empty-state" },
          icon("layers", "ag-empty-icon"),
          h("div", { class: "ag-empty-title" }, "No stages yet"),
          h("div", { class: "ag-muted" }, "Each stage transforms the documents of the one before; its output on a sample shows beside it."),
          h("div", { class: "ag-empty-actions" }, first, fromQuery),
        ),
      );
    } else children.push(this.addButton(this.stages.length, true));
    this.list.replaceChildren(...children);
    this.drawPlan();
  }

  private addButton(at: number, last: boolean): HTMLElement {
    const button = h(
      "button",
      { class: `ag-add${last ? " ag-add-last" : ""}`, type: "button", title: at === 0 ? "Add a stage at the start" : `Add a stage after stage ${at}` },
      icon("plus"),
      h("span", {}, last ? "Add stage" : ""),
    );
    button.addEventListener("click", () => this.addStage(at));
    return button;
  }

  private createCard(stage: PipelineStage): StageCard {
    const id = stage.id;
    const index = () => this.stages.findIndex((item) => item.id === id);
    const card = new StageCard(stage, {
      collection: this.collection,
      schema: () => this.schema.get(),
      fields: () => {
        const input = this.inputDocuments(index());
        return input ? documentPaths(input) : (this.ctx.store.get<string[]>(STORE_FIELD_PATHS) ?? []);
      },
      changed: (text) => {
        this.stages = updateStage(this.stages, id, { text, operator: operatorOfText(text) ?? this.stages[index()]?.operator ?? "$match" });
        this.ctx.stateChanged();
        this.schedulePreview();
      },
      pickOperator: (operator) => {
        const current = this.stages[index()];
        if (!current) return;
        const text = textForOperator(current, operator);
        this.stages = updateStage(this.stages, id, { operator, text });
        card.setText(text);
        this.ctx.stateChanged();
        this.schedulePreview(0);
      },
      toggle: () => this.edit(toggleStage(this.stages, id)),
      collapse: () => {
        const current = this.stages[index()];
        if (current) this.edit(updateStage(this.stages, id, { collapsed: !current.collapsed }));
      },
      duplicate: () => {
        const copy = nextId();
        this.edit(duplicateStage(this.stages, id, copy), { focus: copy });
      },
      insertAfter: () => this.addStage(index() + 1),
      remove: () => this.edit(removeStage(this.stages, id)),
      move: (delta) => {
        const at = index();
        if (at + delta < 0 || at + delta >= this.stages.length) return;
        this.edit(moveStageBy(this.stages, id, delta), { focus: id, handle: true });
      },
      focused: () => {
        const at = index();
        if (at === this.active) return;
        this.active = at;
        for (const [other, item] of this.cards) item.setActive(other === id);
      },
      run: () => void this.runPreview(true),
    });
    card.handle.addEventListener("pointerdown", () => {
      card.element.draggable = true;
    });
    card.element.addEventListener("dragstart", (event) => {
      this.dragging = id;
      event.dataTransfer?.setData("text/plain", id);
      if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
      card.element.classList.add("dragging");
    });
    card.element.addEventListener("dragend", () => {
      card.element.draggable = false;
      card.element.classList.remove("dragging");
      this.dragging = null;
      this.clearDropMarks();
    });
    return card;
  }

  // --- Drag and drop -----------------------------------------------------------------------------------------------

  /** Where a drop at `y` puts the dragged stage: the card it is over and whether before or after it. */
  private dropTarget(event: DragEvent): { card: HTMLElement; after: boolean } | null {
    const cards = [...this.list.querySelectorAll<HTMLElement>(".ag-card")];
    for (const card of cards) {
      const rect = card.getBoundingClientRect();
      if (event.clientY < rect.top + rect.height / 2) return { card, after: false };
    }
    const last = cards[cards.length - 1];
    return last ? { card: last, after: true } : null;
  }

  private onDragOver(event: DragEvent): void {
    if (!this.dragging) return;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
    const target = this.dropTarget(event);
    this.clearDropMarks();
    target?.card.classList.add(target.after ? "drop-after" : "drop-before");
  }

  private onDrop(event: DragEvent): void {
    const id = this.dragging;
    if (!id) return;
    event.preventDefault();
    const target = this.dropTarget(event);
    this.clearDropMarks();
    if (!target) return;
    const targetId = target.card.dataset.id ?? "";
    const rest = this.stages.filter((stage) => stage.id !== id);
    let at = rest.findIndex((stage) => stage.id === targetId);
    if (at < 0) return;
    if (target.after) at++;
    if (this.stages.findIndex((stage) => stage.id === id) === at && targetId !== id) return;
    this.edit(moveStage(this.stages, id, at), { focus: id, handle: true });
  }

  private clearDropMarks(): void {
    for (const card of this.list.querySelectorAll(".drop-before, .drop-after")) card.classList.remove("drop-before", "drop-after");
  }

  // --- Preview -----------------------------------------------------------------------------------------------------

  private schedulePreview(delay = PREVIEW_DELAY_MS): void {
    this.drawPlan();
    if (!this.autoPreview) return;
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.runPreview(false);
    }, delay);
  }

  /** The documents entering stage `index`: the preview output of the last stage before it that ran; null for the first. */
  private inputDocuments(index: number): Doc[] | null {
    for (let at = index - 1; at >= 0; at--) {
      const output = this.previewOf(at);
      if (output === undefined) continue;
      if (output === null || output.error !== null) return null;
      return output.documents as Doc[];
    }
    return null;
  }

  /** The preview result of stage `index` (undefined: the stage does not run in the preview; null: no result holds for it). */
  private previewOf(index: number): AggregatePreviewResult["stages"][number] | null | undefined {
    const entry = this.plan?.entries[index];
    if (!entry || entry.kind !== "run") return undefined;
    if (!this.preview || entry.previewIndex >= this.validPrefix()) return null;
    return this.preview.stages.find((stage) => stage.index === entry.previewIndex) ?? null;
  }

  /** How many stages from the first the preview shown ran exactly as they are now (its results hold for those). */
  private validPrefix(): number {
    const ran = this.previewPipeline;
    const now = this.plan?.pipeline;
    if (!ran || !now) return 0;
    let n = 0;
    while (n < ran.length && n < now.length && JSON.stringify(ran[n]) === JSON.stringify(now[n])) n++;
    return n;
  }

  /** Computes the plan of the stages as they are and draws what each card can tell without a preview. */
  private drawPlan(): void {
    const plan = previewPlan(this.stages);
    this.plan = plan;
    const valid = this.preview ? this.validPrefix() : 0;
    this.stages.forEach((stage, index) => {
      const card = this.cards.get(stage.id);
      const entry = plan.entries[index];
      if (!card || !entry) return;
      if (entry.kind === "run") {
        if (entry.previewIndex < valid) card.showPreview(this.stagePreview(entry.previewIndex));
        else card.showPreview({ kind: "idle", text: this.autoPreview ? "Waiting for the preview…" : "Press Preview to see this stage's output." });
      } else card.showPreview(this.planNote(entry, index));
    });
  }

  private planNote(entry: PipelinePlan["entries"][number], index: number): StagePreview {
    switch (entry.kind) {
      case "disabled":
        return { kind: "note", tone: "muted", text: "Off: the preview and the run skip this stage." };
      case "invalid":
        return { kind: "note", tone: "error", text: entry.error, position: entry.position };
      case "writes": {
        const target = this.writeTarget(index);
        return {
          kind: "note",
          tone: "warn",
          text: `${entry.operator} writes the result into ${target ? `"${target}"` : "a collection"}: it is not previewed and runs only with Run, after the safety rules ask.`,
        };
      }
      case "blocked": {
        const by = this.stages[entry.by];
        const writes = by && /^\s*\{\s*["']?\$(out|merge)\b/.test(by.text);
        return {
          kind: "note",
          tone: "muted",
          text: writes ? `Not previewed: it comes after stage ${entry.by + 1}, which writes.` : `Not previewed: fix stage ${entry.by + 1} first.`,
        };
      }
      default:
        return { kind: "idle", text: "" };
    }
  }

  private writeTarget(index: number): string | null {
    const match = /\$(?:out|merge)["']?\s*:\s*(?:\{[^}]*?\binto["']?\s*:\s*)?["']([^"']+)["']/.exec(this.stages[index]?.text ?? "");
    return match ? match[1] : null;
  }

  private stagePreview(previewIndex: number): StagePreview {
    const result = this.preview?.stages.find((stage) => stage.index === previewIndex);
    if (!result) {
      const failed = this.preview?.stages.find((stage) => stage.error !== null);
      const failedAt = failed ? this.plan?.entries.findIndex((entry) => entry.kind === "run" && entry.previewIndex === failed.index) : -1;
      return { kind: "note", tone: "muted", text: failedAt !== undefined && failedAt >= 0 ? `Not run: stage ${failedAt + 1} failed.` : "Not run." };
    }
    if (result.error !== null) return { kind: "note", tone: "error", text: result.error };
    const as = this.plan ? lookupField(this.plan.pipeline[previewIndex]) : null;
    // The joined array and its first document are opened, so the join shows without a click.
    const path = as ? as.split(".") : null;
    return { kind: "result", stage: result, open: path ? [path, [...path, "0"]] : [] };
  }

  /** Runs the preview of the stages as they are now; `ask`: the user asked for it (a confirmation may be asked). */
  private async runPreview(ask: boolean): Promise<void> {
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    const plan = previewPlan(this.stages);
    this.plan = plan;
    const generation = ++this.generation;
    if (plan.pipeline.length === 0) {
      this.preview = null;
      this.previewPipeline = null;
      this.drawPlan();
      if (this.stages.length > 0) this.showInfo(plan.firstInvalid >= 0 ? `Stage ${plan.firstInvalid + 1} does not parse.` : "No stage to preview.");
      return;
    }
    plan.entries.forEach((entry, index) => {
      if (entry.kind === "run") this.cards.get(this.stages[index].id)?.showPreview({ kind: "loading" });
    });
    this.element.classList.add("busy");
    try {
      const result = await this.api.aggregatePreview(
        {
          database: this.database,
          collection: this.collection,
          pipeline: plan.pipeline,
          sampleSize: this.sampleSize,
          sampleMode: this.sampleMode,
          limit: PREVIEW_LIMIT,
          maxTimeMS: PREVIEW_MAX_TIME_MS,
        },
        ask,
      );
      if (this.disposed || generation !== this.generation) return;
      if (!result) {
        this.showInfo("The preview was not run (the confirmation was declined).");
        this.preview = null;
        this.previewPipeline = null;
        this.drawPlan();
        return;
      }
      this.preview = result;
      this.previewPipeline = plan.pipeline;
      const failed = result.stages.find((stage) => stage.error !== null);
      const failedAt = failed ? plan.entries.findIndex((entry) => entry.kind === "run" && entry.previewIndex === failed.index) : -1;
      this.lastError = failed
        ? `Stage ${failedAt + 1} (${failed.operator}): ${failed.error}`
        : plan.firstInvalid >= 0
          ? `Stage ${plan.firstInvalid + 1} does not parse`
          : null;
      const sample = result.sample;
      this.showInfo(
        `Preview on ${sample.mode === "random" ? "a random sample of" : "the first"} ${formatNumber(sample.size)} documents (${formatNumber(sample.count)} read) · ${formatNumber(result.elapsedMs)} ms` +
          (failed ? ` · stage ${failedAt + 1} failed` : ""),
        failed ? "error" : "info",
      );
      this.drawPlan();
    } catch (error) {
      if (this.disposed || generation !== this.generation) return;
      this.preview = null;
      this.previewPipeline = null;
      const problem = gatewayProblem(error);
      if (!ask && problem?.code === "CONFIRMATION_REQUIRED") {
        this.showInfo("This workspace asks before reads: press Preview to run the preview.");
        this.drawPlan();
        return;
      }
      this.lastError = errorText(error);
      this.info.replaceChildren(renderError(error, { context: `Preview the pipeline on ${this.collection}` }));
      this.info.className = "ag-info ag-info-error";
      this.drawPlan();
    } finally {
      if (!this.disposed && generation === this.generation) this.element.classList.remove("busy");
    }
  }

  private showInfo(text: string, tone: "info" | "error" = "info"): void {
    this.info.className = `ag-info${tone === "error" ? " ag-info-error" : ""}`;
    this.info.replaceChildren(h("span", {}, text));
  }

  private drawSampleMode(): void {
    for (const [mode, button] of this.sampleButtons) {
      button.classList.toggle("active", mode === this.sampleMode);
      button.setAttribute("aria-pressed", String(mode === this.sampleMode));
    }
  }

  // --- Run, export, saved ------------------------------------------------------------------------------------------

  /** The enabled stages, or null after pointing at the first that does not parse. */
  private runnable(): { pipeline: Doc[]; texts: string[] } | null {
    const built = enabledPipeline(this.stages);
    if (!built.ok) {
      const stage = this.stages[built.index];
      if (stage?.collapsed) this.edit(updateStage(this.stages, stage.id, { collapsed: false }));
      this.cards.get(stage?.id ?? "")?.focus();
      toast(`Stage ${built.index + 1} does not parse: ${built.error}`, "error");
      return null;
    }
    if (built.pipeline.length === 0) {
      toast(this.stages.length === 0 ? "Add a stage first." : "Every stage is off.", "error");
      return null;
    }
    return built;
  }

  private async runFull(): Promise<void> {
    const built = this.runnable();
    if (!built) return;
    const started = Date.now();
    const command = `db.getCollection(${JSON.stringify(this.collection)}).aggregate([${built.texts.map((text) => oneLine(text)).join(", ")}])`;
    this.results.showLoading();
    let outcome: AggregateResult | null | Error = null;
    try {
      outcome = await this.api.aggregate({ database: this.database, collection: this.collection, pipeline: built.pipeline, limit: RUN_LIMIT });
      if (this.disposed) return;
      if (!outcome) this.results.showMessage("The pipeline was not run (the confirmation was declined).");
      else {
        this.lastError = null;
        this.results.showResult(outcome);
      }
    } catch (error) {
      if (this.disposed) return;
      outcome = error instanceof Error ? error : new Error(errorText(error));
      this.lastError = errorText(error);
      this.results.showError(error, this.collection);
    }
    this.record(command, started, outcome);
  }

  private exportCode(): void {
    const built = this.runnable();
    if (!built) return;
    const target = { database: this.database, collection: this.collection };
    openCodeExport(`Pipeline on ${this.collection} as code`, (language) => exportPipeline(language, target, built.texts));
  }

  private async startFromQuery(): Promise<void> {
    const state = this.ctx.store.get<QueryState>(STORE_QUERY);
    const texts = state ? stagesFromQuery(state) : [];
    await this.setPipeline(texts.length > 0 ? texts : [templateOf("$match")]);
  }

  /** A saved query of another collection: a pipeline opens in that collection's Aggregations, a find in its Documents. */
  private openSaved(saved: SavedQuery): void {
    const kind = this.schema.get().tables.find((table) => table.name === saved.collection)?.kind === "view" ? "view" : "collection";
    const object: ObjectRef = { database: this.database, schema: null, name: saved.collection, kind };
    const pipeline = pipelineOfSaved(saved);
    const connectionId = this.ctx.view.connection.connectionId;
    if (pipeline) this.ctx.view.navigator.openWorkspace(connectionId, object, "aggregations", { pipeline } satisfies AggregationsRequest);
    else this.ctx.view.navigator.openWorkspace(connectionId, object, "documents", { query: stateOfSaved(saved) } satisfies DocumentsRequest);
  }

  /** Adds the run to the query history in the background. */
  private record(command: string, started: number, outcome: AggregateResult | null | Error): void {
    const view = this.ctx.view;
    const result = outcome && !(outcome instanceof Error) ? outcome : null;
    const entry: HistoryEntry = {
      id: "",
      sql: command,
      connectionId: view.connection.connectionId,
      connectionName: view.profile.name,
      driver: view.engine.id,
      database: this.database || null,
      startedAt: started,
      durationMs: result ? result.elapsedMs : Math.max(0, Date.now() - started),
      results: result ? [{ rowSet: true, rows: result.documents.length, affectedRows: 0, truncated: result.hasMore }] : [],
      error: outcome instanceof Error ? outcome.message : null,
      cancelled: outcome === null,
    };
    api.historyAppend(entry).catch(() => {
      // The result is shown either way.
    });
  }
}

/** The panel under the stages with the result of Run, in the Documents section's renderers. */
class ResultsPanel {
  readonly element = h("div", { class: "ag-results", hidden: true });
  private readonly summary = h("span", { class: "ag-results-summary" });
  private readonly body = h("div", { class: "ag-results-body" });
  private readonly modeButtons = new Map<DocumentsMode, HTMLButtonElement>();
  private readonly renderers = new Map<DocumentsMode, DocumentsRenderer>();
  private readonly context: RendererContext;
  private mode: DocumentsMode = "list";
  private documents: Doc[] = [];
  private height = 320;

  constructor(private readonly changed: () => void) {
    this.context = {
      refs: { targetOf: () => null, open: () => undefined },
      inspect: (title, value) => openInspector(title, value),
      edit: null,
    };
    const modes = h("div", { class: "segmented mq-modes", role: "group", "aria-label": "Show results as" });
    for (const spec of MODES) {
      const button = h("button", { class: "segment", type: "button", title: spec.title, "aria-pressed": "false" }, spec.label);
      button.addEventListener("click", () => this.setMode(spec.id));
      this.modeButtons.set(spec.id, button);
      modes.append(button);
    }
    const handle = h("div", { class: "ag-results-handle", title: "Drag to resize" });
    makeSplitter(handle, "y", (delta) => {
      this.height = Math.max(120, Math.min(window.innerHeight - 200, this.height - delta));
      this.element.style.height = `${this.height}px`;
    });
    const close = iconButton("x", "Close the results", () => {
      this.element.hidden = true;
      this.changed();
    });
    this.element.append(
      handle,
      h(
        "div",
        { class: "toolbar ag-results-bar" },
        h("span", { class: "ag-results-title" }, "Results"),
        this.summary,
        h("div", { class: "toolbar-end" }, modes, close),
      ),
      this.body,
    );
    this.element.style.height = `${this.height}px`;
    this.drawModes();
  }

  showLoading(): void {
    this.open();
    this.summary.textContent = "Running…";
    this.body.replaceChildren(h("div", { class: "ag-results-message" }, "Running the pipeline…"));
  }

  showMessage(text: string): void {
    this.open();
    this.summary.textContent = "";
    this.body.replaceChildren(h("div", { class: "ag-results-message" }, text));
  }

  showError(error: unknown, collection: string): void {
    this.open();
    this.summary.textContent = "Failed";
    this.body.replaceChildren(h("div", { class: "ag-results-message" }, renderError(error, { context: `Run the pipeline on ${collection}` })));
  }

  showResult(result: AggregateResult): void {
    this.open();
    if (result.wrote) {
      this.summary.textContent = `${formatNumber(result.elapsedMs)} ms`;
      const target = result.wrote.target ? `"${result.wrote.target}"` : "the target collection";
      this.body.replaceChildren(
        h("div", { class: "ag-results-message" }, icon("check"), h("span", {}, `${result.wrote.stage} wrote the result into ${target}.`)),
      );
      return;
    }
    const count = result.documents.length;
    this.summary.textContent = `${formatNumber(count)} ${count === 1 ? "document" : "documents"}${result.hasMore ? ` (the first ${formatNumber(count)}; more not shown)` : ""} · ${formatNumber(result.elapsedMs)} ms`;
    this.summary.title = result.statement;
    this.documents = result.documents as Doc[];
    for (const renderer of this.renderers.values()) renderer.dispose?.();
    this.renderers.clear();
    this.draw();
  }

  dispose(): void {
    for (const renderer of this.renderers.values()) renderer.dispose?.();
    this.renderers.clear();
  }

  private open(): void {
    this.element.hidden = false;
  }

  private setMode(mode: DocumentsMode): void {
    this.mode = mode;
    this.drawModes();
    if (this.documents.length > 0) this.draw();
  }

  private drawModes(): void {
    for (const [id, button] of this.modeButtons) {
      button.classList.toggle("active", id === this.mode);
      button.setAttribute("aria-pressed", String(id === this.mode));
    }
  }

  private draw(): void {
    if (this.documents.length === 0) {
      this.body.replaceChildren(h("div", { class: "ag-results-message" }, "The pipeline returned no documents."));
      return;
    }
    let renderer = this.renderers.get(this.mode);
    if (!renderer) {
      const spec = MODES.find((item) => item.id === this.mode) ?? MODES[0];
      renderer = spec.create(this.context);
      this.renderers.set(this.mode, renderer);
      renderer.show(this.documents, 0);
    }
    this.body.replaceChildren(renderer.element);
  }
}

/** A value opened from a Table cell of the results: its tree in a dialog. */
function openInspector(title: string, value: Json): void {
  const tree = new DocTree({ numbered: false, limits: { topFields: 500, childItems: 200 }, label: title });
  const host = h("div", { class: "ag-inspector" }, tree.element);
  openModal(title, host, [{ label: "Close", primary: true, run: () => undefined }], { width: 640, onClose: () => tree.dispose() });
  tree.show([(Array.isArray(value) ? Object.fromEntries(value.map((item, index) => [String(index), item])) : value) as Doc], 0);
}
