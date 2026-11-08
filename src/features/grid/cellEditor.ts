/**
 * Large editor for one cell: a monospace textarea with line/char counts, word wrap and NULL toggles,
 * and Format / Minify with inline validation for JSON. Binary and vector values, and cells of read-only
 * grids, open read-only with Copy. Saving hands the value back to the grid as a normal pending edit.
 *
 * Keys stay inside the dialog: Esc closes without saving, Ctrl+Enter saves.
 */
import { registerModal, toast } from "../../core/dialogs";
import { copyText, h, icon } from "../../core/dom";
import type { Cell, ColumnMeta } from "../../core/types";
import { checkJson, formatJson, hexDump, looksLikeJson, minifyJson, textStats, vectorListing } from "./cellValue";

export interface CellEditorOptions {
  column: ColumnMeta;
  value: Cell;
  /** Where the value comes from, e.g. "products · row 12". */
  context: string;
  /** Set when the value cannot be changed; the editor then only shows and copies it. */
  readOnlyReason: string | null;
  /** Loads every component of a vector value (the grid only holds a summary). */
  loadVector: (() => Promise<number[]>) | null;
  save: (value: Cell) => void;
  onClose?: () => void;
}

const WRAP_STORAGE_KEY = "db-studio.cellEditor.wrap";
const numberFormat = new Intl.NumberFormat("en-US");

export function openCellEditor(options: CellEditorOptions): void {
  const { column, value } = options;
  const binary = column.vectorDims == null && column.binary;
  const vector = column.vectorDims != null;
  const readOnly = options.readOnlyReason !== null;
  const jsonColumn = /json/i.test(column.typeName);
  let copyValue = value ?? "";

  const textarea = h("textarea", {
    class: "cell-editor-text",
    spellcheck: "false",
    autocomplete: "off",
    "aria-label": `Value of ${column.name}`,
  });
  textarea.value = binary && value !== null ? hexDump(value) : (value ?? "");
  textarea.readOnly = readOnly;
  let savedText = textarea.value;

  const wrap = h("input", { type: "checkbox", checked: readWrap() });
  const isNull = h("input", { type: "checkbox", checked: value === null, disabled: readOnly });
  const formatButton = toolbarButton("Format", readOnly ? "Pretty-print the JSON for reading" : "Pretty-print the JSON (2-space indent)");
  const minifyButton = toolbarButton("Minify", readOnly ? "Show the JSON on one line" : "Remove whitespace from the JSON");
  const copyButton = toolbarButton("Copy", binary ? "Copy the value as 0x… hex" : "Copy the value");
  const stats = h("span", { class: "cell-editor-stats" });
  const jsonStatus = h("button", { class: "cell-editor-json", type: "button" });
  const footer = h("div", { class: "modal-footer" });

  const tools = h(
    "div",
    { class: "cell-editor-tools" },
    h("label", { class: "checkbox" }, wrap, h("span", {}, "Wrap lines")),
    readOnly ? null : h("label", { class: "checkbox" }, isNull, h("span", {}, "NULL")),
    h("span", { class: "spacer" }),
    formatButton,
    minifyButton,
    copyButton,
  );
  const typeLabel = vector ? `vector(${column.vectorDims})` : column.typeName;
  const dialog = h(
    "div",
    { class: "modal cell-editor-modal", role: "dialog", "aria-modal": "true", "aria-label": `Edit ${column.name}` },
    h(
      "div",
      { class: "modal-title cell-editor-title" },
      h("span", {}, column.name),
      typeLabel ? h("span", { class: "cell-editor-type" }, typeLabel) : null,
      options.context ? h("span", { class: "cell-editor-context" }, options.context) : null,
    ),
    h(
      "div",
      { class: "modal-body cell-editor-body" },
      readOnly ? h("div", { class: "cell-editor-note" }, icon("view"), `${options.readOnlyReason}. You can read and copy the value.`) : null,
      tools,
      textarea,
      h("div", { class: "cell-editor-status" }, stats, jsonStatus),
    ),
    footer,
  );
  const overlay = h("div", { class: "modal-overlay", tabIndex: -1 }, dialog);
  // Clicks on the backdrop keep focus in the dialog instead of dropping it on <body>.
  overlay.addEventListener("mousedown", (event) => {
    if (event.target === overlay) {
      event.preventDefault();
      overlay.focus();
    }
  });

  const jsonMode = () => !binary && !vector && !isNull.checked && (jsonColumn || looksLikeJson(textarea.value));
  const refresh = () => {
    textarea.disabled = isNull.checked;
    textarea.placeholder = isNull.checked ? "NULL" : "";
    textarea.classList.toggle("nowrap", !wrap.checked);
    const { lines, chars } = textStats(isNull.checked ? "" : textarea.value);
    stats.textContent = isNull.checked ? "NULL" : `${numberFormat.format(lines)} ${lines === 1 ? "line" : "lines"} · ${numberFormat.format(chars)} chars`;
    const json = jsonMode();
    formatButton.hidden = !json;
    minifyButton.hidden = !json;
    jsonStatus.hidden = !json || textarea.value.trim() === "";
    jsonStatus.className = "cell-editor-json";
    jsonStatus.title = "";
    jsonStatus.onclick = null;
    if (jsonStatus.hidden) return;
    const check = checkJson(textarea.value);
    formatButton.disabled = !check.valid;
    minifyButton.disabled = !check.valid;
    if (check.valid) {
      jsonStatus.classList.add("ok");
      jsonStatus.textContent = "Valid JSON";
      return;
    }
    jsonStatus.classList.add("error");
    const where = check.line !== null ? ` (line ${check.line}, column ${check.column})` : "";
    jsonStatus.textContent = `Invalid JSON: ${check.message}${where}`;
    if (check.position !== null) {
      const position = Math.min(check.position, textarea.value.length);
      jsonStatus.title = "Go to the error";
      jsonStatus.onclick = () => {
        textarea.focus();
        textarea.setSelectionRange(position, Math.min(position + 1, textarea.value.length));
      };
    }
  };

  // Keys inside the dialog are handled below; Esc with focus elsewhere still closes it.
  const unregister = registerModal(overlay, (event, inside) => {
    if (inside || event.key !== "Escape") return false;
    close();
    return true;
  });
  const close = () => {
    if (!overlay.isConnected) return;
    unregister();
    overlay.remove();
    options.onClose?.();
  };
  const currentValue = (): Cell => (isNull.checked ? null : textarea.value);
  const save = () => {
    if (readOnly) return close();
    const next = currentValue();
    if (next !== value) options.save(next);
    close();
  };
  const replaceText = (text: string) => {
    // execCommand keeps the change on the textarea's undo stack; fall back to a plain assignment.
    textarea.focus();
    textarea.select();
    if (readOnly || !document.execCommand?.("insertText", false, text)) textarea.value = text;
    textarea.setSelectionRange(0, 0);
    refresh();
  };

  formatButton.addEventListener("click", () => replaceText(formatJson(textarea.value)));
  minifyButton.addEventListener("click", () => replaceText(minifyJson(textarea.value)));
  copyButton.addEventListener("click", () => {
    // What is shown, except for binary and vector values, which copy as 0x… hex and [a,b,…].
    const text = binary || vector ? copyValue : (currentValue() ?? "");
    void copyText(text).then(() => toast("Copied", "success"));
  });
  wrap.addEventListener("change", () => {
    writeWrap(wrap.checked);
    refresh();
  });
  isNull.addEventListener("change", () => {
    if (isNull.checked) savedText = textarea.value;
    else textarea.value = savedText;
    refresh();
    // The textarea is disabled while NULL is on; keep focus inside the dialog so Esc and Ctrl+Enter still work.
    (isNull.checked ? isNull : textarea).focus();
  });
  textarea.addEventListener("input", refresh);

  // Keys never leave the dialog: the grid, the tab and the app shortcuts must not see them.
  overlay.addEventListener("keydown", (event) => {
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      close();
    } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      save();
    }
  });
  overlay.addEventListener("keyup", (event) => event.stopPropagation());

  if (readOnly) {
    footer.append(actionButton("Close", true, close));
  } else {
    footer.append(
      h("span", { class: "cell-editor-hint" }, "Ctrl+Enter saves · Esc cancels · changes stay pending until Apply"),
      actionButton("Cancel", false, close),
      actionButton("Save", true, save),
    );
  }

  if (vector && options.loadVector && value !== null) {
    textarea.value = `${value}\n\nLoading all values…`;
    options
      .loadVector()
      .then((values) => {
        if (!overlay.isConnected) return;
        copyValue = `[${values.join(",")}]`;
        textarea.value = vectorListing(values);
        refresh();
      })
      .catch((error: unknown) => {
        textarea.value = value;
        toast(error instanceof Error ? error.message : String(error), "error");
        refresh();
      });
  }

  refresh();
  document.body.append(overlay);
  textarea.focus();
  if (!readOnly) textarea.setSelectionRange(0, 0);
}

function toolbarButton(label: string, title: string): HTMLButtonElement {
  return h("button", { class: "btn small", type: "button", title }, label);
}

function actionButton(label: string, primary: boolean, run: () => void): HTMLButtonElement {
  const button = h("button", { class: primary ? "btn primary" : "btn", type: "button" }, label);
  button.addEventListener("click", run);
  return button;
}

function readWrap(): boolean {
  try {
    return localStorage.getItem(WRAP_STORAGE_KEY) !== "off";
  } catch {
    return true;
  }
}

function writeWrap(on: boolean): void {
  try {
    localStorage.setItem(WRAP_STORAGE_KEY, on ? "on" : "off");
  } catch {
    // Storage can be unavailable; the toggle then lasts for this dialog only.
  }
}
