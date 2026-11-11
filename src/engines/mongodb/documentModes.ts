/**
 * The ways the Documents section shows a page of documents. Each mode is a renderer the section creates on first use
 * and keeps while the tab is open; switching modes shows the same page. List (expandable cards) and JSON (read-only
 * editor) are here; Table and the virtualised tree plug in as more `DocumentsRenderer`s (step M4).
 */
import { toast } from "../../core/dialogs";
import { copyText, h, icon, iconButton } from "../../core/dom";
import { documentJson, foldedText, isNested, scalarText, type Doc, type Json } from "./documents";
import { createJsonViewer } from "./queryEditor";
import type { DocumentsMode } from "./queryModel";

export interface DocumentsRenderer {
  readonly element: HTMLElement;
  /** Shows a page; `offset` is the position of its first document among all matches. */
  show(documents: Doc[], offset: number): void;
  dispose?(): void;
}

export interface ModeSpec {
  id: DocumentsMode;
  label: string;
  title: string;
  create(): DocumentsRenderer;
}

/** Top-level fields a card shows before "Show N more fields", and items an opened array shows at first. */
const CARD_FIELDS = 12;
const NESTED_ITEMS = 50;

export const MODES: ModeSpec[] = [
  { id: "list", label: "List", title: "Documents as cards with fields to expand", create: () => new ListRenderer() },
  { id: "json", label: "JSON", title: "The page as Extended JSON", create: () => new JsonRenderer() },
];

class ListRenderer implements DocumentsRenderer {
  readonly element = h("div", { class: "md-list", role: "list" });

  show(documents: Doc[], offset: number): void {
    this.element.replaceChildren(...documents.map((document, index) => card(document, offset + index)));
    this.element.scrollTop = 0;
  }
}

function card(document: Doc, position: number): HTMLElement {
  const copy = iconButton("copy", "Copy as JSON", () => {
    copyText(documentJson(document)).then(
      () => toast("Document copied", "success"),
      () => toast("Could not write to the clipboard"),
    );
  });
  const body = h("div", { class: "md-fields" });
  fillFields(body, Object.entries(document), CARD_FIELDS);
  return h(
    "article",
    { class: "md-card", role: "listitem", "aria-label": `Document ${position + 1}` },
    h("div", { class: "md-card-tools" }, h("span", { class: "md-index" }, String(position + 1)), copy),
    body,
  );
}

/** Appends `entries` as field rows, the first `limit` of them, then a button for the rest. */
function fillFields(host: HTMLElement, entries: [string, Json][], limit: number, array = false): void {
  const shown = entries.slice(0, limit);
  host.append(...shown.map(([key, value]) => fieldRow(key, value, array)));
  const rest = entries.length - shown.length;
  if (rest <= 0) return;
  const more = h(
    "button",
    { class: "link-button md-more", type: "button" },
    `Show ${rest} more ${array ? (rest === 1 ? "item" : "items") : rest === 1 ? "field" : "fields"}`,
  );
  more.addEventListener("click", () => {
    more.remove();
    fillFields(host, entries.slice(limit), Number.POSITIVE_INFINITY, array);
  });
  host.append(more);
}

function fieldRow(key: string, value: Json, inArray: boolean): HTMLElement {
  const keyElement = h("span", { class: inArray ? "md-key md-index-key" : "md-key" }, key);
  if (!isNested(value)) {
    const { text, kind } = scalarText(value);
    return h(
      "div",
      { class: "md-field" },
      h("span", { class: "md-toggle-space" }),
      keyElement,
      h("span", { class: "md-colon" }, ":"),
      h("span", { class: `md-value md-${kind}` }, text),
    );
  }
  const entries: [string, Json][] = Array.isArray(value) ? value.map((item, index) => [String(index), item]) : Object.entries(value);
  const folded = h("span", { class: "md-folded" }, foldedText(value));
  if (entries.length === 0) {
    return h("div", { class: "md-field" }, h("span", { class: "md-toggle-space" }), keyElement, h("span", { class: "md-colon" }, ":"), folded);
  }
  const toggle = h("button", { class: "md-toggle", type: "button", "aria-expanded": "false", "aria-label": `Expand ${key}` }, icon("chevron"));
  const head = h("div", { class: "md-field md-nested" }, toggle, keyElement, h("span", { class: "md-colon" }, ":"), folded);
  const row = h("div", { class: "md-node" }, head);
  let children: HTMLElement | null = null;
  const flip = () => {
    const open = toggle.getAttribute("aria-expanded") !== "true";
    toggle.setAttribute("aria-expanded", String(open));
    toggle.setAttribute("aria-label", `${open ? "Collapse" : "Expand"} ${key}`);
    if (!children) {
      children = h("div", { class: "md-children" });
      fillFields(children, entries, NESTED_ITEMS, Array.isArray(value));
      row.append(children);
    }
    children.hidden = !open;
  };
  toggle.addEventListener("click", flip);
  keyElement.addEventListener("click", flip);
  keyElement.classList.add("md-clickable");
  return row;
}

class JsonRenderer implements DocumentsRenderer {
  readonly element = h("div", { class: "md-json" });
  private readonly viewer = createJsonViewer(this.element);

  show(documents: Doc[]): void {
    this.viewer.set(JSON.stringify(documents, null, 2));
  }

  dispose(): void {
    this.viewer.destroy();
  }
}
