/** What the value editors share: the host the value view gives them and a few DOM builders. */
import { h } from "../../core/dom";
import type { Blob, Expect, ReadOptions, RedisEdit, ValueRead } from "./model";
import { blobDisplay, isBinary } from "./model";
import type { RedisApi } from "./redisApi";

export interface EditorHost {
  readonly key: Blob;
  readonly api: RedisApi;
  /** The workspace's rules block every write: editors show their data without edit controls. */
  readonly readonly: boolean;
  /**
   * Applies an edit guarded by `expect`. Resolves true when it was applied (the view then reloads the key); false when it was
   * declined, failed (the error is shown) or conflicted (the view shows the conflict with "Reload" and "Overwrite").
   */
  apply(edit: RedisEdit, expect: Expect | undefined, label: string): Promise<boolean>;
  /** Re-reads the key with other paging options and redraws the editor. */
  page(options: ReadOptions): Promise<void>;
}

export type EditorRenderer = (host: EditorHost, read: ValueRead) => HTMLElement;

export function button(label: string, onClick: () => void, className = "tool-button", title = label): HTMLButtonElement {
  const element = h("button", { class: className, type: "button", title }, label);
  element.addEventListener("click", onClick);
  return element;
}

export function textInput(value: string, placeholder = "", className = "input"): HTMLInputElement {
  return h("input", { class: className, type: "text", value, placeholder, spellcheck: "false", autocomplete: "off" });
}

export function emptyHint(text: string): HTMLElement {
  return h("div", { class: "empty-hint" }, text);
}

export function dataTable(headers: string[], rows: HTMLElement[]): HTMLElement {
  return h("table", { class: "rd-table" }, h("thead", {}, h("tr", {}, ...headers.map((header) => h("th", {}, header)))), h("tbody", {}, ...rows));
}

export function cell(...children: (HTMLElement | string | null)[]): HTMLElement {
  return h("td", {}, ...children);
}

/**
 * An input that shows `blob` and calls `commit(text)` when the user changes it and leaves the field or presses Enter.
 * Binary data is shown as hex and cannot be edited here; `Escape` reverts.
 */
export function editableCell(blob: Blob, editable: boolean, commit: (text: string) => void): HTMLInputElement {
  const shown = blobDisplay(blob);
  const input = textInput(shown, "", "input rd-cell-input");
  if (!editable || isBinary(blob)) {
    input.readOnly = true;
    input.classList.add("readonly");
    if (isBinary(blob)) input.title = "Binary value: edit it from the console";
    return input;
  }
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") input.blur();
    else if (event.key === "Escape") {
      input.value = shown;
      input.blur();
    }
  });
  input.addEventListener("change", () => {
    if (input.value !== shown) commit(input.value);
  });
  return input;
}

/** A small row of controls above a table. */
export function bar(...children: (HTMLElement | null)[]): HTMLElement {
  return h("div", { class: "rd-bar" }, ...children);
}

export function labelled(label: string, control: HTMLElement): HTMLElement {
  return h("label", { class: "inline-field" }, h("span", {}, label), control);
}

export function trashButton(onClick: () => void, title: string, disabled = false): HTMLButtonElement {
  const element = h("button", { class: "icon-button rd-remove", type: "button", title, "aria-label": title }, "×");
  element.disabled = disabled;
  element.addEventListener("click", onClick);
  return element;
}
