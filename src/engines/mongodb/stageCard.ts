/**
 * One stage of the Aggregations section: a card with the drag handle (also moved with ArrowUp / ArrowDown when
 * focused), the operator picker (common stages with a description, picking one puts in its template), the on/off
 * switch, fold, and a menu (duplicate, insert after, move, delete); under it the stage's editor (mongosh syntax,
 * completion of the fields reaching this stage) beside its preview strip: the count and time of the stage's output on
 * the sample and its first documents as small document trees (a `$lookup`'s joined array opened), or why the stage did
 * not run. "Ask" asks the assistant to explain the stage, suggest the next one or fix its error. The section owns the
 * pipeline: the card reports edits and draws what it is given.
 */
import type { CompletionSchema } from "../../lang/types";
import { showMenuBelow } from "../../core/contextMenu";
import { formatNumber, h, icon, iconButton } from "../../core/dom";
import { placeInViewport } from "../../core/placement";
import { stageCompletionSource } from "../../lang/mongo/pipelineCompletion";
import { lineColumn } from "../../lang/mongo/shell";
import { DocTree } from "./docTree";
import type { Doc } from "./documents";
import type { PreviewStage } from "./mongoApi";
import { operatorOfText, specOf, STAGE_SPECS, type PipelineStage } from "./pipelineModel";
import { StageEditor } from "./queryEditor";
import type { StageQuestion } from "./assistantModel";

/** Documents a preview strip draws (the rest are counted). */
const STRIP_DOCUMENTS = 5;
const STRIP_LIMITS = { topFields: 14, childItems: 20 };

export type StagePreview =
  | { kind: "idle"; text: string }
  | { kind: "loading" }
  | { kind: "result"; stage: PreviewStage; open: string[][] }
  | { kind: "note"; tone: "muted" | "warn" | "error"; text: string; position?: number };

export interface StageCardOptions {
  collection: string;
  schema(): CompletionSchema;
  /** The fields reaching this stage (the previous stage's output). */
  fields(): readonly string[];
  changed(text: string): void;
  pickOperator(operator: string): void;
  toggle(): void;
  collapse(): void;
  duplicate(): void;
  insertAfter(): void;
  remove(): void;
  move(delta: -1 | 1): void;
  /** The card got the focus (its stage becomes the active one). */
  focused(): void;
  /** Ctrl+Enter in the editor. */
  run(): void;
  /** "Ask": a question about this stage for the assistant; none: no button. */
  ask?(question: StageQuestion): void;
}

export class StageCard {
  readonly element: HTMLElement;
  readonly handle: HTMLButtonElement;
  private readonly number = h("span", { class: "ag-num" });
  private readonly operatorButton: HTMLButtonElement;
  private readonly operatorLabel = h("span", { class: "ag-operator-label" });
  private readonly description = h("span", { class: "ag-desc" });
  private readonly status = h("span", { class: "ag-status" });
  private readonly enabledInput: HTMLInputElement;
  private readonly collapseButton: HTMLButtonElement;
  private readonly body: HTMLElement;
  private readonly editor: StageEditor;
  private readonly outputHead = h("div", { class: "ag-output-head" });
  private readonly docs = h("div", { class: "ag-docs" });
  private trees: DocTree[] = [];
  private stage: PipelineStage;
  private picker: { element: HTMLElement; close: () => void } | null = null;

  constructor(
    stage: PipelineStage,
    private readonly options: StageCardOptions,
  ) {
    this.stage = stage;
    this.handle = h(
      "button",
      { class: "ag-grip", type: "button", title: "Drag to move the stage (or ArrowUp / ArrowDown)", "aria-label": "Move stage" },
      h("span", { class: "ag-grip-dots", "aria-hidden": "true" }),
    );
    this.handle.addEventListener("keydown", (event) => {
      if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
      event.preventDefault();
      options.move(event.key === "ArrowUp" ? -1 : 1);
    });
    this.operatorButton = h(
      "button",
      { class: "ag-operator", type: "button", title: "Pick the stage operator", "aria-haspopup": "listbox" },
      this.operatorLabel,
      icon("chevronDown", "ag-operator-icon"),
    );
    this.operatorButton.addEventListener("click", () => this.openPicker());
    this.enabledInput = h("input", { type: "checkbox", class: "ag-switch-input", "aria-label": "Stage on" });
    this.enabledInput.addEventListener("change", () => options.toggle());
    const toggle = h(
      "label",
      { class: "ag-switch", title: "Turn the stage on or off (off: skipped by the preview and the run)" },
      this.enabledInput,
      h("span", { class: "ag-switch-track", "aria-hidden": "true" }),
    );
    this.collapseButton = iconButton("chevronDown", "Fold the stage", () => options.collapse());
    this.collapseButton.classList.add("ag-collapse");
    const more = iconButton("more", "Stage actions", () =>
      showMenuBelow(more, [
        { label: "Duplicate stage", icon: "layers", run: () => options.duplicate() },
        { label: "Add a stage after", icon: "plus", run: () => options.insertAfter() },
        "separator",
        { label: "Move up", icon: "prev", run: () => options.move(-1) },
        { label: "Move down", icon: "chevron", run: () => options.move(1) },
        "separator",
        { label: "Delete stage", icon: "trash", danger: true, run: () => options.remove() },
      ]),
    );
    const ask = options.ask;
    const askButton = ask
      ? h(
          "button",
          { class: "tool-button ag-ask", type: "button", title: "Ask the assistant about this stage", "aria-haspopup": "menu" },
          icon("sparkle"),
          h("span", {}, "Ask"),
        )
      : null;
    askButton?.addEventListener("click", () =>
      showMenuBelow(askButton, [
        { label: "Explain this stage", icon: "view", run: () => ask?.("explain") },
        { label: "Suggest the next stage", icon: "plus", run: () => ask?.("next") },
        { label: "Fix the error", icon: "health", disabled: !this.element.classList.contains("failed"), run: () => ask?.("fix") },
      ]),
    );
    const head = h(
      "div",
      { class: "ag-card-head" },
      this.handle,
      this.number,
      this.operatorButton,
      this.description,
      this.status,
      askButton,
      toggle,
      this.collapseButton,
      more,
    );
    head.addEventListener("dblclick", (event) => {
      if (event.target === head || event.target === this.description) options.collapse();
    });
    const editorHost = h("div", { class: "ag-editor" });
    this.editor = new StageEditor(editorHost, {
      doc: stage.text,
      label: "Stage",
      completion: stageCompletionSource({ collection: options.collection, schema: options.schema, fields: options.fields }),
      run: () => options.run(),
      changed: () => {
        this.syncOperator();
        options.changed(this.editor.text);
      },
      focused: () => options.focused(),
    });
    this.body = h("div", { class: "ag-card-body" }, editorHost, h("div", { class: "ag-output" }, this.outputHead, this.docs));
    this.element = h("div", { class: "ag-card", "data-id": stage.id, tabindex: "-1" }, head, this.body);
    this.element.addEventListener("focusin", () => options.focused());
    this.update(stage, 0);
  }

  get id(): string {
    return this.stage.id;
  }

  /** Draws the stage's switch, fold and position (the text is the editor's; `setText` replaces it). */
  update(stage: PipelineStage, index: number): void {
    this.stage = stage;
    this.number.textContent = String(index + 1);
    this.handle.setAttribute("aria-label", `Move stage ${index + 1}`);
    this.enabledInput.checked = stage.enabled;
    this.element.classList.toggle("disabled", !stage.enabled);
    this.element.classList.toggle("collapsed", stage.collapsed);
    this.body.hidden = stage.collapsed;
    this.collapseButton.title = stage.collapsed ? "Unfold the stage" : "Fold the stage";
    this.collapseButton.setAttribute("aria-expanded", String(!stage.collapsed));
    this.syncOperator();
  }

  setText(text: string): void {
    this.editor.text = text;
    this.syncOperator();
  }

  setActive(active: boolean): void {
    this.element.classList.toggle("active", active);
  }

  focus(): void {
    if (this.stage.collapsed) this.handle.focus();
    else this.editor.focus();
  }

  focusHandle(): void {
    this.handle.focus();
  }

  /** Draws the stage's preview, or why it has none; a parse error is also underlined in the editor. */
  showPreview(preview: StagePreview): void {
    this.element.classList.toggle("failed", preview.kind === "note" && preview.tone === "error");
    this.editor.showError(preview.kind === "note" && preview.position !== undefined ? preview.position : null);
    this.status.replaceChildren();
    this.status.className = "ag-status";
    if (preview.kind !== "result") this.clearTrees();
    switch (preview.kind) {
      case "idle":
        this.outputHead.replaceChildren(h("span", { class: "ag-muted" }, preview.text));
        return;
      case "loading":
        this.outputHead.replaceChildren(h("span", { class: "ag-muted" }, "Running the preview…"));
        this.status.textContent = "…";
        return;
      case "note": {
        const where =
          preview.position !== undefined
            ? (() => {
                const { line, column } = lineColumn(this.editor.text, preview.position);
                const link = h("button", { class: "link-button", type: "button", title: "Go to the error" }, `Line ${line}, column ${column}`);
                link.addEventListener("click", () => this.editor.focus(preview.position));
                return link;
              })()
            : null;
        this.outputHead.replaceChildren(
          h(
            "div",
            { class: `ag-note ag-note-${preview.tone}`, role: preview.tone === "error" ? "alert" : null },
            preview.tone === "muted" ? null : icon(preview.tone === "error" ? "x" : "shield", "ag-note-icon"),
            where,
            h("span", {}, preview.text),
          ),
        );
        if (preview.tone === "error") {
          this.status.classList.add("ag-status-error");
          this.status.textContent = "Error";
        }
        return;
      }
      case "result":
        this.showResult(preview.stage, preview.open);
    }
  }

  dispose(): void {
    this.picker?.close();
    this.clearTrees();
    this.editor.destroy();
  }

  private showResult(stage: PreviewStage, open: string[][]): void {
    const count = stage.count ?? 0;
    const noun = count === 1 ? "document" : "documents";
    this.status.textContent = `${formatNumber(count)} · ${formatNumber(stage.elapsedMs)} ms`;
    this.status.title = `${formatNumber(count)} ${noun} out of this stage on the sample, in ${formatNumber(stage.elapsedMs)} ms`;
    const shown = Math.min(stage.documents.length, STRIP_DOCUMENTS);
    const joined = open.length > 0 ? h("span", { class: "ag-muted" }, ` · joined array "${open[0].join(".")}" opened`) : null;
    const parts = [
      h("span", { class: "ag-output-count" }, `Output: ${formatNumber(count)} ${noun}`),
      h("span", { class: "ag-muted" }, ` · ${formatNumber(stage.elapsedMs)} ms`),
      shown > 0 && count > shown ? h("span", { class: "ag-muted" }, ` · first ${shown} shown`) : null,
      joined,
    ];
    this.outputHead.replaceChildren(...parts.filter((part): part is HTMLSpanElement => part !== null));
    this.clearTrees();
    if (count === 0 || stage.documents.length === 0) {
      this.docs.replaceChildren(h("div", { class: "ag-empty" }, "No documents come out of this stage on the sample."));
      return;
    }
    const cards: HTMLElement[] = [];
    for (const document of stage.documents.slice(0, STRIP_DOCUMENTS)) {
      const tree = new DocTree({ numbered: false, limits: STRIP_LIMITS, label: "Output document" });
      this.trees.push(tree);
      cards.push(h("div", { class: "ag-doc" }, tree.element));
      tree.show([document as Doc], 0, open);
    }
    this.docs.classList.toggle("ag-docs-joined", open.length > 0);
    this.docs.replaceChildren(...cards);
  }

  private clearTrees(): void {
    for (const tree of this.trees) tree.dispose();
    this.trees = [];
    this.docs.replaceChildren();
  }

  /** The header follows the operator typed (or the one picked while the text has none). */
  private syncOperator(): void {
    const operator = operatorOfText(this.editor.text) ?? this.stage.operator;
    this.operatorLabel.textContent = operator;
    const spec = specOf(operator);
    this.description.textContent = this.stage.collapsed ? oneLineText(this.editor.text) : (spec?.description ?? "");
    this.description.title = this.stage.collapsed ? this.editor.text : (spec?.description ?? "");
    this.element.classList.toggle("writes", spec?.writes === true);
  }

  /** The operator list under the button: filtered by typing, Enter picks, Escape closes. */
  private openPicker(): void {
    if (this.picker) {
      this.picker.close();
      return;
    }
    const search = h("input", {
      class: "input ag-picker-search",
      type: "search",
      placeholder: "Stage operator",
      spellcheck: "false",
      "aria-label": "Search stage operators",
    });
    const list = h("div", { class: "ag-picker-list", role: "listbox", "aria-label": "Stage operators" });
    const element = h("div", { class: "ag-picker", role: "dialog", "aria-label": "Stage operator" }, search, list);
    let focused = 0;
    let shown: typeof STAGE_SPECS = STAGE_SPECS;
    const current = operatorOfText(this.editor.text) ?? this.stage.operator;
    const render = () => {
      const query = search.value.trim().toLowerCase().replace(/^\$/, "");
      shown = STAGE_SPECS.filter((spec) => !query || spec.operator.toLowerCase().includes(query) || spec.description.toLowerCase().includes(query));
      focused = Math.min(focused, Math.max(0, shown.length - 1));
      list.replaceChildren(
        ...shown.map((spec, index) => {
          const row = h(
            "div",
            {
              class: `ag-picker-row${index === focused ? " focused" : ""}${spec.operator === current ? " current" : ""}`,
              role: "option",
              "aria-selected": String(spec.operator === current),
            },
            h("span", { class: "ag-picker-op" }, spec.operator),
            h("span", { class: "ag-picker-desc" }, spec.description),
            spec.writes ? h("span", { class: "ag-picker-writes" }, "writes") : null,
          );
          row.addEventListener("click", () => pick(spec.operator));
          return row;
        }),
      );
      if (shown.length === 0) list.append(h("div", { class: "ag-empty" }, "No stage operator matches"));
      list.children[focused]?.scrollIntoView({ block: "nearest" });
    };
    const pick = (operator: string) => {
      close();
      if (operator !== current) this.options.pickOperator(operator);
      this.editor.focus();
    };
    const onPointerDown = (event: PointerEvent) => {
      const node = event.target as Node;
      if (!element.contains(node) && !this.operatorButton.contains(node)) close();
    };
    const close = () => {
      element.remove();
      document.removeEventListener("pointerdown", onPointerDown, true);
      this.picker = null;
    };
    search.addEventListener("input", () => {
      focused = 0;
      render();
    });
    element.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        focused = Math.max(0, Math.min(shown.length - 1, focused + (event.key === "ArrowDown" ? 1 : -1)));
        render();
      } else if (event.key === "Enter") {
        event.preventDefault();
        if (shown[focused]) pick(shown[focused].operator);
      } else if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        close();
        this.operatorButton.focus();
      }
    });
    document.addEventListener("pointerdown", onPointerDown, true);
    this.picker = { element, close };
    focused = Math.max(
      0,
      STAGE_SPECS.findIndex((spec) => spec.operator === current),
    );
    render();
    document.body.append(element);
    const rect = this.operatorButton.getBoundingClientRect();
    const placed = placeInViewport(rect.left, rect.bottom + 4, element.offsetWidth, element.offsetHeight, window.innerWidth, window.innerHeight);
    element.style.left = `${placed.left}px`;
    element.style.top = `${placed.top}px`;
    search.focus();
  }
}

function oneLineText(text: string): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > 140 ? `${flat.slice(0, 139)}…` : flat;
}
