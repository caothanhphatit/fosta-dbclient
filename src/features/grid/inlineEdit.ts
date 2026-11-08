/** Inline cell editors: a text box (validated per column kind) and a checkbox for booleans. */
import { editText, KIND_HINTS } from "./cellValue";
import type { GridRow, Position } from "./gridModel";
import { stepVisibleColumn } from "./columnVisibility";
import type { ColumnInfo, Value } from "../../core/contract";
import { toast } from "../../core/dialogs";
import { h } from "../../core/dom";
import { parseInput, valuesEqual } from "../../core/values";

/** What an inline editor needs from the grid that hosts it. */
export interface InlineEditHost {
  /** Indexes of the visible columns, in order (Tab steps over them). */
  displayed(): number[];
  /** An editor opened: Esc / outside commits go through `commit`. */
  begin(row: number, commit: () => void): void;
  /** The editor closed. */
  end(): void;
  refreshRow(index: number): void;
  focusGrid(): void;
  select(position: Position): void;
  /** Row index of the selection, if any. */
  selectedRow(): number | undefined;
  rowSelected(values: Value[]): void;
  changed(): void;
  openLargeEditor(position: Position): void;
}

export function beginTextEdit(
  host: InlineEditHost,
  position: Position,
  row: GridRow,
  cell: HTMLElement,
  column: ColumnInfo,
  value: Value,
  initial?: string,
): void {
  const startText = editText(value);
  const input = h("input", {
    class: "cell-editor",
    value: initial ?? startText,
    spellcheck: "false",
    placeholder: value.t === "null" && initial === undefined ? "NULL" : (KIND_HINTS[column.kind.kind] ?? ""),
  });
  let finished = false;
  /** The typed value, or an Error that keeps the editor open. */
  const parsed = (): Value | Error => (column.kind.kind === "text" ? { t: "text", v: input.value } : parseInput(column.kind, input.value));
  const markInvalid = (error: Error | null) => {
    input.classList.toggle("invalid", error !== null);
    input.title = error?.message ?? "";
  };
  const finish = (commit: boolean, next: Position | null, refocus: boolean) => {
    if (finished) return;
    const untouched = initial === undefined && input.value === startText;
    // An empty box on a NULL cell that was only opened and left stays NULL.
    const leftNull = value.t === "null" && initial === undefined && input.value === "";
    if (commit && !untouched && !leftNull) {
      const result = parsed();
      if (result instanceof Error) {
        // Enter / Tab keep the editor open on invalid input; leaving it (blur, scrolling away) drops the edit.
        if (refocus) {
          markInvalid(result);
          toast(result.message, "error");
          return;
        }
        toast(`Not changed: ${result.message}`, "error");
        commit = false;
      } else if (!valuesEqual(result, value)) {
        row.current[position.column] = result;
        row.touched.add(position.column);
        row.edited = true;
      }
    }
    finished = true;
    host.end();
    host.refreshRow(position.row);
    if (refocus) host.focusGrid();
    if (next) host.select(next);
    if (commit && host.selectedRow() === position.row) host.rowSelected(row.current);
    host.changed();
  };
  input.addEventListener("input", () => {
    if (input.classList.contains("invalid")) {
      const result = parsed();
      markInvalid(result instanceof Error ? result : null);
    }
  });
  input.addEventListener("keydown", (event) => {
    event.stopPropagation();
    if (event.key === "Enter" && event.shiftKey) {
      // Continue in the large editor with what was typed so far.
      event.preventDefault();
      finish(true, null, true);
      if (finished) host.openLargeEditor(position);
    } else if (event.key === "Enter") finish(true, { row: position.row + 1, column: position.column }, true);
    else if (event.key === "Escape") finish(false, null, true);
    else if (event.key === "Tab") {
      event.preventDefault();
      finish(true, { row: position.row, column: stepVisibleColumn(host.displayed(), position.column, event.shiftKey ? -1 : 1) }, true);
    }
  });
  input.addEventListener("blur", () => finish(true, null, false));
  host.begin(position.row, () => finish(true, null, false));
  cell.classList.add("editing");
  cell.replaceChildren(input);
  input.focus();
  if (initial === undefined) input.select();
}

/** Booleans edit as a checkbox in the cell: Space toggles, Enter / Tab keep, Esc cancels; t/f/1/0/y/n set it. */
export function beginBoolEdit(host: InlineEditHost, position: Position, row: GridRow, cell: HTMLElement, value: Value, initial?: string): void {
  const start = value.t === "bool" ? value.v : false;
  const box = h("input", { type: "checkbox", class: "cell-editor-check", checked: start });
  const typed = initial?.toLowerCase();
  if (typed && "t1y".includes(typed)) box.checked = true;
  else if (typed && "f0n".includes(typed)) box.checked = false;
  else if (typed === " ") box.checked = !start;
  let finished = false;
  const finish = (commit: boolean, next: Position | null, refocus: boolean) => {
    if (finished) return;
    finished = true;
    host.end();
    const changed = value.t !== "bool" || value.v !== box.checked;
    if (commit && changed) {
      row.current[position.column] = { t: "bool", v: box.checked };
      row.touched.add(position.column);
      row.edited = true;
    }
    host.refreshRow(position.row);
    if (refocus) host.focusGrid();
    if (next) host.select(next);
    if (commit && changed && host.selectedRow() === position.row) host.rowSelected(row.current);
    host.changed();
  };
  box.addEventListener("keydown", (event) => {
    event.stopPropagation();
    const key = event.key.toLowerCase();
    if (event.key === "Enter") finish(true, { row: position.row + 1, column: position.column }, true);
    else if (event.key === "Escape") finish(false, null, true);
    else if (event.key === "Tab") {
      event.preventDefault();
      finish(true, { row: position.row, column: stepVisibleColumn(host.displayed(), position.column, event.shiftKey ? -1 : 1) }, true);
    } else if (key.length === 1 && "t1y".includes(key)) box.checked = true;
    else if (key.length === 1 && "f0n".includes(key)) box.checked = false;
  });
  box.addEventListener("blur", () => finish(true, null, false));
  host.begin(position.row, () => finish(true, null, false));
  cell.classList.add("editing");
  cell.replaceChildren(h("label", { class: "cell-check" }, box, h("span", {}, "true")));
  box.focus();
}
