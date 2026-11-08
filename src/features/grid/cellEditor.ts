/**
 * Large editor for one cell, chosen by the column's kind: a textarea for text, a JSON editor (Format / Minify, inline
 * validation that jumps to the error) for JSON, documents and arrays, a checkbox for booleans, `0x…` hex for bytes, and
 * validated text for numbers, dates, times, timestamps, UUIDs and ObjectIds (`parseInput`; the server validates again).
 * NULL is a toggle, never an empty box. Vector values and cells of read-only grids open read-only with Copy. Saving hands
 * the typed value back to the grid as a pending edit.
 *
 * Keys stay inside the dialog: Esc closes without saving, Ctrl+Enter saves.
 */
import type { ColumnInfo, Value } from "../../core/contract";
import { showError } from "../../core/errorPanel";
import { registerModal, toast } from "../../core/dialogs";
import { copyText, h, icon } from "../../core/dom";
import { displayValue, parseInput } from "../../core/values";
import { checkJson, editText, formatJson, hexDump, isJsonKind, KIND_HINTS, looksLikeJson, minifyJson, textStats, vectorListing } from "./cellValue";

export interface CellEditorOptions {
  column: ColumnInfo;
  value: Value;
  /** Where the value comes from, e.g. "products · row 12". */
  context: string;
  /** Set when the value cannot be changed; the editor then only shows and copies it. */
  readOnlyReason: string | null;
  /** Loads every component of a vector value (the grid only holds a summary). */
  loadVector: (() => Promise<number[]>) | null;
  save: (value: Value) => void;
  onClose?: () => void;
}

const WRAP_STORAGE_KEY = "db-studio.cellEditor.wrap";
const numberFormat = new Intl.NumberFormat("en-US");

/** Kinds whose text needs no check (anything typed is a valid value). */
const FREE_TEXT = new Set(["text", "other", "interval"]);

export function openCellEditor(options: CellEditorOptions): void {
  const { column, value } = options;
  const kind = column.kind;
  const vector = column.hints.vectorDims != null;
  const readOnly = options.readOnlyReason !== null;
  const bytes = kind.kind === "bytes";
  const bool = kind.kind === "bool";
  const jsonKind = isJsonKind(kind);
  const startsNull = value.t === "null";
  const initialText = editText(value, true);
  let copyValue = displayValue(value) ?? "";

  const textarea = h("textarea", {
    class: "cell-editor-text",
    spellcheck: "false",
    autocomplete: "off",
    "aria-label": `Value of ${column.name}`,
    placeholder: KIND_HINTS[kind.kind] ?? "",
  });
  textarea.value = bytes && readOnly && !startsNull ? hexDump(initialText) : initialText;
  textarea.readOnly = readOnly;
  let savedText = textarea.value;

  const boolInput = h("input", { type: "checkbox", checked: value.t === "bool" && value.v, disabled: readOnly });
  const boolLabel = h("span", {}, String(value.t === "bool" && value.v));
  const boolControl = h("label", { class: "checkbox cell-editor-bool" }, boolInput, boolLabel);

  const wrap = h("input", { type: "checkbox", checked: readWrap() });
  const isNull = h("input", { type: "checkbox", checked: startsNull, disabled: readOnly });
  const formatButton = toolbarButton("Format", readOnly ? "Pretty-print the JSON for reading" : "Pretty-print the JSON (2-space indent)");
  const minifyButton = toolbarButton("Minify", readOnly ? "Show the JSON on one line" : "Remove whitespace from the JSON");
  const copyButton = toolbarButton("Copy", bytes ? "Copy the value as 0x… hex" : "Copy the value");
  const stats = h("span", { class: "cell-editor-stats" });
  const status = h("button", { class: "cell-editor-json", type: "button" });
  const footer = h("div", { class: "modal-footer" });
  const saveButton = actionButton("Save", true, () => save());

  const tools = h(
    "div",
    { class: "cell-editor-tools" },
    bool ? null : h("label", { class: "checkbox" }, wrap, h("span", {}, "Wrap lines")),
    readOnly ? null : h("label", { class: "checkbox" }, isNull, h("span", {}, "NULL")),
    h("span", { class: "spacer" }),
    formatButton,
    minifyButton,
    copyButton,
  );
  const typeLabel = vector ? `vector(${column.hints.vectorDims})` : column.typeName || kind.kind;
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
      bool ? boolControl : textarea,
      h("div", { class: "cell-editor-status" }, stats, status),
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

  /** Text columns that hold JSON still get Format / Minify and a (non-blocking) JSON check. */
  const jsonMode = () => !bytes && !vector && !bool && !isNull.checked && (jsonKind || (FREE_TEXT.has(kind.kind) && looksLikeJson(textarea.value)));

  /** The value as typed, or an `Error` that blocks saving. */
  const currentValue = (): Value | Error => {
    if (isNull.checked) return { t: "null", v: null };
    if (bool) return { t: "bool", v: boolInput.checked };
    if (kind.kind === "text") return { t: "text", v: textarea.value };
    return parseInput(kind, textarea.value);
  };

  const showStatus = (kindClass: "ok" | "error" | null, text: string, position: number | null = null) => {
    status.hidden = kindClass === null;
    status.className = `cell-editor-json${kindClass ? ` ${kindClass}` : ""}`;
    status.textContent = text;
    status.title = "";
    status.onclick = null;
    if (position === null) return;
    const at = Math.min(position, textarea.value.length);
    status.title = "Go to the error";
    status.onclick = () => {
      textarea.focus();
      textarea.setSelectionRange(at, Math.min(at + 1, textarea.value.length));
    };
  };

  const refresh = () => {
    textarea.disabled = isNull.checked;
    boolInput.disabled = readOnly || isNull.checked;
    boolLabel.textContent = isNull.checked ? "NULL" : String(boolInput.checked);
    textarea.placeholder = isNull.checked ? "NULL" : (KIND_HINTS[kind.kind] ?? "");
    textarea.classList.toggle("nowrap", !wrap.checked);
    const { lines, chars } = textStats(isNull.checked ? "" : textarea.value);
    stats.textContent = isNull.checked
      ? "NULL"
      : bool
        ? ""
        : bytes && !readOnly
          ? bytesStats(textarea.value)
          : `${numberFormat.format(lines)} ${lines === 1 ? "line" : "lines"} · ${numberFormat.format(chars)} chars`;
    const json = jsonMode();
    formatButton.hidden = !json;
    minifyButton.hidden = !json;
    saveButton.disabled = false;
    if (readOnly || isNull.checked || bool) return showStatus(null, "");
    if (json) {
      if (textarea.value.trim() === "") {
        saveButton.disabled = jsonKind;
        return jsonKind ? showStatus("error", "Empty: type JSON or tick NULL") : showStatus(null, "");
      }
      const check = checkJson(textarea.value);
      formatButton.disabled = !check.valid;
      minifyButton.disabled = !check.valid;
      if (check.valid) {
        const typed = jsonKind ? currentValue() : null;
        if (typed instanceof Error) {
          saveButton.disabled = true;
          return showStatus("error", typed.message);
        }
        return showStatus("ok", "Valid JSON");
      }
      // A text column may hold text that only looks like JSON; it still saves.
      saveButton.disabled = jsonKind;
      const where = check.line !== null ? ` (line ${check.line}, column ${check.column})` : "";
      return showStatus("error", `Invalid JSON: ${check.message}${where}`, check.position);
    }
    if (FREE_TEXT.has(kind.kind)) return showStatus(null, "");
    const typed = currentValue();
    if (typed instanceof Error) {
      saveButton.disabled = true;
      return showStatus("error", typed.message);
    }
    showStatus("ok", `Valid ${KIND_HINTS[kind.kind] ? kind.kind : "value"}: ${displayValue(typed) ?? "NULL"}`);
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
  const unchanged = () =>
    isNull.checked === startsNull && (isNull.checked || (bool ? value.t === "bool" && boolInput.checked === value.v : textarea.value === initialText));
  const save = () => {
    if (readOnly) return close();
    if (unchanged()) return close();
    const next = currentValue();
    if (next instanceof Error) {
      toast(next.message, "error");
      return;
    }
    options.save(next);
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
    const typed = currentValue();
    const text = bytes || vector ? copyValue : typed instanceof Error ? textarea.value : (displayValue(typed) ?? "");
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
    (isNull.checked ? isNull : bool ? boolInput : textarea).focus();
  });
  textarea.addEventListener("input", refresh);
  boolInput.addEventListener("change", refresh);

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
      saveButton,
    );
  }

  if (vector && options.loadVector && value.t !== "null") {
    const summary = displayValue(value) ?? "";
    textarea.value = `${summary}\n\nLoading all values…`;
    options
      .loadVector()
      .then((values) => {
        if (!overlay.isConnected) return;
        copyValue = `[${values.join(",")}]`;
        textarea.value = vectorListing(values);
        refresh();
      })
      .catch((error: unknown) => {
        textarea.value = summary;
        showError(error, "Load the vector");
        refresh();
      });
  }

  refresh();
  document.body.append(overlay);
  (bool ? boolInput : textarea).focus();
  if (!readOnly && !bool) textarea.setSelectionRange(0, 0);
}

function bytesStats(text: string): string {
  const hex = /^0x([0-9a-f]*)$/i.exec(text.trim())?.[1];
  if (hex !== undefined && hex.length % 2 === 0) return `${numberFormat.format(hex.length / 2)} bytes`;
  return `${numberFormat.format(new TextEncoder().encode(text).length)} bytes as UTF-8 text`;
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
