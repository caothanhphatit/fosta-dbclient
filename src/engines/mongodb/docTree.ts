/**
 * The List mode's document tree, virtualised: the documents of a page (or one document, in the side panel) as lines of
 * fields, nested documents and arrays folded until opened. Only the lines in view have elements, so a document with
 * thousands of fields or an array of a hundred thousand items scrolls as fast as a small one; opened containers show
 * their first items and a "Show N more" line. Values read as BSON (`bson.ts`); ObjectIds of inferred references
 * (`refs.ts`) get a link to the referenced document.
 */
import { showContextMenu, type MenuItem } from "../../core/contextMenu";
import { toast } from "../../core/dialogs";
import { copyText, h, icon, iconButton } from "../../core/dom";
import { readable, shellText } from "./bson";
import { documentJson, foldedText, isNested, type Doc, type Json } from "./documents";
import { refPath } from "./refs";
import { buildLines, DEFAULT_LIMITS, type TreeLimits, type TreeLine, type TreeState } from "./treeLines";

/** Items one "Show more" adds. */
const MORE_STEP = 1000;

/** Heights of the line kinds, in pixels (the CSS sizes the lines to match). */
const HEIGHTS: Record<TreeLine["type"], number> = { head: 6, field: 21, more: 22, foot: 14 };
const INDENT = 16;
/** Lines drawn beyond the viewport on each side. */
const OVERSCAN = 30;
const TITLE_LIMIT = 2000;

/** What the tree needs from the section: the inferred target of a reference field, and opening one. */
export interface TreeRefs {
  /** The collection the ObjectIds at `path` (dotted, no array indexes) point at; null when none is known. */
  targetOf(path: string): string | null;
  open(collection: string, id: Json, path: string): void;
}

export interface DocTreeOptions {
  limits?: TreeLimits;
  /** Each document's position among all matches is shown on its card (List mode); off in the side panel. */
  numbered?: boolean;
  refs?: TreeRefs;
  label?: string;
}

export class DocTree {
  readonly element: HTMLElement;
  private readonly sizer = h("div", { class: "dt-sizer" });
  private readonly limits: TreeLimits;
  private documents: readonly Doc[] = [];
  private offset = 0;
  private state: TreeState = { expanded: new Set(), limits: new Map() };
  private lines: TreeLine[] = [];
  /** Top of each line; one more entry: the total height. */
  private tops: number[] = [0];
  private rendered = new Map<number, HTMLElement>();
  private frame: number | null = null;
  private readonly resizeObserver: ResizeObserver | null = null;

  constructor(private readonly options: DocTreeOptions = {}) {
    this.limits = options.limits ?? DEFAULT_LIMITS;
    this.element = h("div", { class: "dt-tree", role: "tree", "aria-label": options.label ?? "Documents", tabIndex: 0 }, this.sizer);
    // Drawn at once (not on the next frame): a scroll event comes once per frame anyway, and lines never lag behind.
    this.element.addEventListener("scroll", () => this.draw(false), { passive: true });
    this.element.addEventListener("click", (event) => this.onClick(event));
    this.element.addEventListener("contextmenu", (event) => this.onContextMenu(event));
    if (typeof ResizeObserver !== "undefined") {
      this.resizeObserver = new ResizeObserver(() => this.schedule());
      this.resizeObserver.observe(this.element);
    }
  }

  /** Shows `documents` (the first at position `offset` among all matches), every container folded, at the top. */
  show(documents: readonly Doc[], offset = 0): void {
    this.documents = documents;
    this.offset = offset;
    this.state = { expanded: new Set(), limits: new Map() };
    this.rebuild();
    this.element.scrollTop = 0;
    this.draw(true);
  }

  /** Draws the lines in view again (a reference target became known). */
  refresh(): void {
    this.draw(true);
  }

  /** The index of the document at the top of the view. */
  topDocument(): number {
    const line = this.lines[this.lineAt(this.element.scrollTop)];
    return line ? line.doc : 0;
  }

  scrollToDocument(index: number): void {
    const at = this.lines.findIndex((line) => line.type === "head" && line.doc === index);
    this.element.scrollTop = at >= 0 ? this.tops[at] : 0;
    this.draw(false);
  }

  dispose(): void {
    this.resizeObserver?.disconnect();
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    this.frame = null;
  }

  private rebuild(): void {
    this.lines = buildLines(this.documents, this.state, this.limits);
    const tops = new Array<number>(this.lines.length + 1);
    let top = 0;
    this.lines.forEach((line, index) => {
      tops[index] = top;
      top += HEIGHTS[line.type];
    });
    tops[this.lines.length] = top;
    this.tops = tops;
    this.sizer.style.height = `${top}px`;
  }

  /** The line at `y` (binary search over the tops). */
  private lineAt(y: number): number {
    let low = 0;
    let high = Math.max(0, this.lines.length - 1);
    while (low < high) {
      const mid = (low + high + 1) >> 1;
      if (this.tops[mid] <= y) low = mid;
      else high = mid - 1;
    }
    return low;
  }

  private schedule(): void {
    if (this.frame !== null) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = null;
      this.draw(false);
    });
  }

  private draw(force: boolean): void {
    if (force) {
      for (const element of this.rendered.values()) element.remove();
      this.rendered.clear();
    }
    if (this.lines.length === 0) return;
    const viewport = this.element.clientHeight || 800;
    const first = Math.max(0, this.lineAt(this.element.scrollTop) - OVERSCAN);
    const last = Math.min(this.lines.length - 1, this.lineAt(this.element.scrollTop + viewport) + OVERSCAN);
    for (const [index, element] of this.rendered) {
      if (index >= first && index <= last) continue;
      element.remove();
      this.rendered.delete(index);
    }
    const fragment = document.createDocumentFragment();
    for (let index = first; index <= last; index++) {
      if (this.rendered.has(index)) continue;
      const element = this.renderLine(this.lines[index], index);
      element.style.top = `${this.tops[index]}px`;
      this.rendered.set(index, element);
      fragment.append(element);
    }
    this.sizer.append(fragment);
  }

  private renderLine(line: TreeLine, index: number): HTMLElement {
    const attrs = { "data-line": index };
    switch (line.type) {
      case "head": {
        const tools = h(
          "div",
          { class: "dt-tools" },
          this.options.numbered === false ? null : h("span", { class: "md-index" }, String(this.offset + line.doc + 1)),
          iconButton("copy", "Copy document (Extended JSON)", () => this.copyDocument(line.doc, false)),
        );
        return h("div", { ...attrs, class: "dt-line dt-head" }, tools);
      }
      case "foot":
        return h("div", { ...attrs, class: "dt-line dt-foot" }, h("div", { class: "dt-foot-box" }));
      case "more": {
        const step = Math.min(line.remaining, MORE_STEP);
        const noun = line.array ? (step === 1 ? "item" : "items") : step === 1 ? "field" : "fields";
        const text =
          step < line.remaining
            ? `Show ${step.toLocaleString()} more ${noun} (${line.remaining.toLocaleString()} not shown)`
            : `Show ${step.toLocaleString()} more ${noun}`;
        const more = h("button", { class: "link-button dt-more", type: "button", "data-more": line.id }, text);
        return h(
          "div",
          { ...attrs, class: "dt-line dt-field dt-more-line" },
          h("div", { class: "dt-row", style: `padding-left: ${line.depth * INDENT + 20}px` }, more),
        );
      }
      case "field":
        return this.renderField(line, attrs);
    }
  }

  private renderField(line: Extract<TreeLine, { type: "field" }>, attrs: Record<string, unknown>): HTMLElement {
    const key = h("span", { class: line.inArray ? "md-key md-index-key" : "md-key" }, line.key);
    const parts: (HTMLElement | null)[] = [];
    if (line.container) {
      const toggle = h(
        "button",
        { class: "md-toggle", type: "button", "data-toggle": line.id, "aria-label": `${line.expanded ? "Collapse" : "Expand"} ${line.key}` },
        icon("chevron"),
      );
      toggle.setAttribute("aria-expanded", String(line.expanded));
      key.classList.add("md-clickable");
      key.dataset.toggle = line.id;
      parts.push(toggle, key, h("span", { class: "md-colon" }, ":"), h("span", { class: "md-folded" }, foldedText(line.value as Doc | Json[])));
    } else if (isNested(line.value)) {
      parts.push(
        h("span", { class: "md-toggle-space" }),
        key,
        h("span", { class: "md-colon" }, ":"),
        h("span", { class: "md-folded" }, foldedText(line.value)),
      );
    } else {
      const value = readable(line.value);
      const text = value.text.length > TITLE_LIMIT ? `${value.text.slice(0, TITLE_LIMIT)}…` : value.text;
      const title = value.title ?? (value.text.length > 60 ? text : null);
      parts.push(
        h("span", { class: "md-toggle-space" }),
        key,
        h("span", { class: "md-colon" }, ":"),
        h("span", { class: `md-value md-${value.kind}`, title }, text),
      );
      if (value.type === "objectId") parts.push(this.refLink(line));
    }
    const row = h("div", { class: "dt-row", style: `padding-left: ${line.depth * INDENT}px` }, ...parts);
    const element = h("div", { ...attrs, class: "dt-line dt-field", role: "treeitem", "aria-level": line.depth + 1 }, row);
    if (line.container) element.setAttribute("aria-expanded", String(line.expanded));
    return element;
  }

  private refLink(line: Extract<TreeLine, { type: "field" }>): HTMLElement | null {
    const refs = this.options.refs;
    if (!refs) return null;
    const path = refPath(line.path);
    const target = refs.targetOf(path);
    if (!target) return null;
    const link = h(
      "button",
      {
        class: "dt-ref",
        type: "button",
        "data-ref": target,
        title: `Inferred reference: a document with this _id exists in ${target}. Open it`,
      },
      icon("chevron", "dt-ref-icon"),
      h("span", {}, target),
      h("span", { class: "dt-inferred" }, "inferred"),
    );
    return link;
  }

  private lineOf(target: EventTarget | null): TreeLine | null {
    const element = target instanceof Element ? target.closest<HTMLElement>("[data-line]") : null;
    return element ? (this.lines[Number(element.dataset.line)] ?? null) : null;
  }

  private onClick(event: MouseEvent): void {
    const target = event.target instanceof Element ? event.target : null;
    if (!target) return;
    const toggle = target.closest<HTMLElement>("[data-toggle]");
    if (toggle?.dataset.toggle) {
      this.toggle(toggle.dataset.toggle);
      return;
    }
    const more = target.closest<HTMLElement>("[data-more]");
    if (more?.dataset.more) {
      const id = more.dataset.more;
      const line = this.lineOf(more);
      const shown = this.state.limits.get(id) ?? (line?.type === "more" && line.depth === 0 ? this.limits.topFields : this.limits.childItems);
      this.state.limits.set(id, shown + MORE_STEP);
      this.rebuildKeepingScroll();
      return;
    }
    const ref = target.closest<HTMLElement>("[data-ref]");
    const line = this.lineOf(ref);
    if (ref?.dataset.ref && line?.type === "field") this.options.refs?.open(ref.dataset.ref, line.value, refPath(line.path));
  }

  private toggle(id: string): void {
    if (this.state.expanded.has(id)) this.state.expanded.delete(id);
    else this.state.expanded.add(id);
    this.rebuildKeepingScroll();
    const index = this.lines.findIndex((line) => line.type === "field" && line.id === id);
    this.rendered.get(index)?.querySelector<HTMLElement>(".md-toggle")?.focus({ preventScroll: true });
  }

  private rebuildKeepingScroll(): void {
    const top = this.element.scrollTop;
    this.rebuild();
    this.element.scrollTop = top;
    this.draw(true);
  }

  private onContextMenu(event: MouseEvent): void {
    const line = this.lineOf(event.target);
    if (!line || line.type === "more") return;
    const items: MenuItem[] = [];
    if (line.type === "field") {
      const path = line.path.join(".");
      items.push(
        {
          label: "Copy value",
          icon: "copy",
          detail: "mongosh syntax",
          run: () => void this.copy(shellText(line.value, isNested(line.value) ? 2 : 0), "Value copied"),
        },
        { label: "Copy value as JSON", run: () => void this.copy(documentJson(line.value), "Value copied") },
        { label: "Copy field path", run: () => void this.copy(refPath(line.path) || path, "Path copied") },
        "separator",
      );
    }
    items.push(
      { label: "Copy document", detail: "mongosh syntax", run: () => this.copyDocument(line.doc, true) },
      { label: "Copy document as JSON", run: () => this.copyDocument(line.doc, false) },
    );
    showContextMenu(event, items);
  }

  private copyDocument(doc: number, shell: boolean): void {
    const document = this.documents[doc];
    if (document) void this.copy(shell ? shellText(document, 2) : documentJson(document), "Document copied");
  }

  private async copy(text: string, done: string): Promise<void> {
    try {
      await copyText(text);
      toast(done, "success");
    } catch {
      toast("Could not write to the clipboard");
    }
  }
}
