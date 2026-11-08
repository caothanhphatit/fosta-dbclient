/**
 * The dialog for a save that failed with `CONFLICT` (optimistic concurrency): one section per conflicting row with its
 * key and a table of the involved columns — "When opened" (what the grid loaded), "Now" (the row in the database) and
 * "Yours" (what the save tried to write) — and, per row, **Overwrite** (send that row's change again with `force`,
 * through the safety rules), **Take theirs** (drop the local edit and reload the row) or, for all of them, **Keep
 * editing** (close; every pending edit stays).
 *
 * Nothing of the failed save was applied. Its changes without a conflict stay pending in the grid; Apply saves them
 * (and any row the user still has to decide on) later. Rows are resolved one at a time, so an Overwrite never carries
 * other edits along.
 */
import type { Value } from "../../core/contract";
import { openModal } from "../../core/dialogs";
import { h, icon } from "../../core/dom";
import { errorText } from "../../core/errors";
import { cellText, type ConflictPayload, type ConflictRow } from "./conflicts";
import "./conflictDialog.css";

export interface ConflictDialogOptions {
  /** The object's name, for the title. */
  objectLabel: string;
  payload: ConflictPayload;
  rows: ConflictRow[];
  /** Pending changes of the same save that had no conflict. */
  otherPending: number;
  /** Re-sends the row's change with `force`; resolves true when it was saved (false: cancelled at the confirmation). */
  overwrite(row: ConflictRow): Promise<boolean>;
  /** Drops the local edit and reloads the row from the server. */
  takeTheirs(row: ConflictRow): Promise<boolean>;
  onClose?(): void;
}

const OP_LABELS: Record<ConflictRow["op"], string> = { update: "Edited", delete: "Deleted", insert: "New row" };

export function openConflictDialog(options: ConflictDialogOptions): void {
  const { payload, rows } = options;
  let open = rows.length;
  let close: () => void = () => undefined;

  const resolved = () => {
    open -= 1;
    if (open === 0) close();
  };

  const sections = rows.map((row) => rowSection(row, options, resolved));
  const others =
    options.otherPending > 0
      ? `Your ${options.otherPending} other pending change(s) had no conflict and stay pending: press Apply again to save them.`
      : "No other change was waiting.";
  const body = h(
    "div",
    { class: "conflict-dialog" },
    h("p", { class: "conflict-intro" }, payload.message),
    payload.compensated === false
      ? h(
          "div",
          { class: "conflict-warning", role: "alert" },
          icon("shield"),
          h(
            "span",
            {},
            "Some writes of this save could not be undone (the engine has no transaction here). The object may hold part of the save: refresh and check before you retry.",
          ),
        )
      : null,
    h(
      "p",
      { class: "conflict-note" },
      `Per row: Overwrite saves your version of that row now; Take theirs drops your edit and shows the row as it is now. ${others}`,
    ),
    ...sections,
  );
  close = openModal(`Conflicts in ${options.objectLabel}`, body, [{ label: "Keep editing", primary: true, run: () => undefined }], {
    width: 820,
    onClose: () => options.onClose?.(),
  });
}

function rowSection(row: ConflictRow, options: ConflictDialogOptions, resolved: () => void): HTMLElement {
  const status = h("span", { class: "conflict-status" });
  const overwrite = h(
    "button",
    { class: "btn small danger", type: "button", title: "Save your version of this row anyway (goes through the safety rules)" },
    "Overwrite",
  );
  const theirs = h("button", { class: "btn small", type: "button", title: "Drop your edit of this row and load it as it is now" }, "Take theirs");
  // An insert has nothing to force (the key exists already), and a row that is gone cannot be updated or deleted.
  overwrite.hidden = row.op === "insert" || row.missing;
  const section = h(
    "section",
    { class: "conflict-row" },
    h(
      "div",
      { class: "conflict-row-head" },
      icon("key"),
      h("span", { class: "conflict-key" }, row.keyText || "(no key)"),
      h("span", { class: "conflict-op" }, OP_LABELS[row.op]),
      row.missing ? h("span", { class: "conflict-badge" }, "No longer exists") : null,
      h("span", { class: "spacer" }),
      status,
      overwrite,
      theirs,
    ),
    row.otherChange
      ? h("p", { class: "conflict-hint" }, "These columns look the same: another column or the row version changed since you loaded the row.")
      : null,
    cellsTable(row),
  );
  const act = async (run: () => Promise<boolean>, done: string) => {
    overwrite.disabled = true;
    theirs.disabled = true;
    status.className = "conflict-status";
    status.textContent = "Working…";
    try {
      if (!(await run())) {
        status.textContent = "";
        overwrite.disabled = false;
        theirs.disabled = false;
        return;
      }
      status.textContent = done;
      status.classList.add("ok");
      section.classList.add("resolved");
      overwrite.hidden = true;
      theirs.hidden = true;
      resolved();
    } catch (error) {
      status.textContent = errorText(error);
      status.classList.add("error");
      overwrite.disabled = false;
      theirs.disabled = false;
    }
  };
  overwrite.addEventListener("click", () => void act(() => options.overwrite(row), row.op === "delete" ? "Deleted" : "Overwritten"));
  theirs.addEventListener("click", () => void act(() => options.takeTheirs(row), row.missing ? "Removed from the grid" : "Reloaded"));
  return section;
}

function valueCell(value: Value | undefined, classes: string): HTMLTableCellElement {
  if (value === undefined) return h("td", { class: `${classes} conflict-same` }, "(not changed)");
  const text = cellText(value);
  return h(
    "td",
    { class: `${classes}${value?.t === "null" ? " null" : ""}`, title: text.length > 60 ? text : null },
    text.length > 200 ? `${text.slice(0, 200)}…` : text,
  );
}

function cellsTable(row: ConflictRow): HTMLElement {
  const span = Math.max(row.cells.length, 1);
  const body = row.cells.map((cell, index) => {
    const now = row.missing
      ? index === 0
        ? h("td", { class: "conflict-gone", rowspan: span }, "This row no longer exists")
        : null
      : valueCell(cell.current, cell.changedNow ? "changed" : "");
    const yours =
      row.op === "delete"
        ? index === 0
          ? h("td", { class: "conflict-delete", rowspan: span }, "Delete this row")
          : null
        : valueCell(cell.attempted, cell.changedByYou ? "mine" : "");
    return h("tr", {}, h("th", { scope: "row" }, cell.column), valueCell(cell.original, ""), now, yours);
  });
  return h(
    "table",
    { class: "conflict-table" },
    h("thead", {}, h("tr", {}, h("th", {}, "Column"), h("th", {}, "When opened"), h("th", {}, "Now"), h("th", {}, "Yours"))),
    h("tbody", {}, ...body),
  );
}
