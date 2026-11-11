/**
 * The dialogs of document editing: a value (with its type: string, the numeric types, boolean, date, ObjectId, null,
 * or an object / array in mongosh syntax), a field name (add, rename), a new document (insert, duplicate) and the
 * review of the pending changes as a per-document diff. Nothing here writes: the dialogs hand their result to the
 * pending edits (`docEdits.ts`); Save in the section sends them.
 */
import { field, openModal, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import { shellText } from "./bson";
import { diffDocuments, diffSummary, entryText, type DiffEntry } from "./docDiff";
import { fieldNameProblem } from "./docEdits";
import type { Doc, Json } from "./documents";
import { defaultType, EDIT_TYPES, initialText, parseDocumentInput, parseInput, type EditType } from "./valueInput";

export interface ValueResult {
  name: string;
  value: Json;
  keepType: boolean;
}

export interface ValueDialogOptions {
  title: string;
  /** The field's path, shown under the title. */
  path: string;
  /** The value now (undefined for a new field). */
  value?: Json;
  /** Ask for a field name (adding a field to a document). */
  askName?: boolean;
}

const HINTS: Partial<Record<EditType, string>> = {
  number: "Kept in the field's stored numeric type (an Int32 stays an Int32, a Double a Double).",
  date: "ISO 8601, e.g. 2026-01-02 or 2026-01-02T08:30:00Z; a time without a zone is UTC.",
  json: "mongosh syntax: { city: 'Oslo' }, [1, 2], ObjectId('…'), ISODate('…'), NumberLong('…'), /regex/i.",
  long: "A whole number; exact past 2^53.",
  decimal: "Exact decimal, e.g. 19.99.",
};

/** Asks for a value (and a name); resolves null when cancelled. */
export function openValueDialog(options: ValueDialogOptions): Promise<ValueResult | null> {
  return new Promise((resolve) => {
    let result: ValueResult | null = null;
    const original = options.value;
    let type: EditType = original === undefined ? "string" : defaultType(original);
    const name = textInput("", { placeholder: "field name", spellcheck: false, class: "input md-edit-name" });
    const select = h("select", { class: "input md-edit-type", "aria-label": "Type" }) as HTMLSelectElement;
    for (const item of EDIT_TYPES) select.append(h("option", { value: item.id }, item.label));
    select.value = type;
    const line = textInput("", { spellcheck: false, class: "input md-edit-input" });
    const area = h("textarea", { class: "input md-edit-area", spellcheck: "false", rows: 8 }) as HTMLTextAreaElement;
    const bool = h(
      "select",
      { class: "input md-edit-input", "aria-label": "Value" },
      h("option", { value: "true" }, "true"),
      h("option", { value: "false" }, "false"),
    ) as HTMLSelectElement;
    const nullNote = h("div", { class: "md-edit-note" }, "The value is null.");
    const hint = h("div", { class: "md-edit-hint" });
    const error = h("div", { class: "md-edit-error", role: "alert", hidden: true });
    const valueHost = h("div", { class: "md-edit-value" });
    let shownText = "";

    const control = (): HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement | null =>
      type === "bool" ? bool : type === "null" ? null : type === "json" || type === "string" ? area : line;
    const text = () => control()?.value ?? "";
    const show = (keepText: boolean) => {
      const previous = text();
      const next = keepText ? previous : original === undefined ? "" : initialText(original, type);
      valueHost.replaceChildren(control() ?? nullNote);
      area.rows = type === "json" ? 8 : 3;
      area.classList.toggle("md-mono", type === "json");
      const target = control();
      if (target) target.value = type === "bool" ? (next === "false" ? "false" : "true") : next;
      shownText = text();
      hint.textContent = HINTS[type] ?? "";
      hint.hidden = !HINTS[type];
      error.hidden = true;
    };
    select.addEventListener("change", () => {
      // Text the user typed is kept when the type changes; untouched text is shown in the new type.
      const typed = text() !== shownText;
      type = select.value as EditType;
      show(typed && type !== "bool" && type !== "null");
    });
    show(false);

    const body = h(
      "div",
      { class: "md-edit" },
      h("div", { class: "md-edit-path" }, options.path),
      options.askName ? field("Name", name) : null,
      field("Type", select),
      valueHost,
      hint,
      error,
    );
    openModal(
      options.title,
      body,
      [
        { label: "Cancel", run: () => undefined },
        {
          label: "Apply",
          primary: true,
          run: () => {
            const fieldName = name.value.trim();
            if (options.askName) {
              const problem = fieldNameProblem(fieldName);
              if (problem) {
                error.textContent = problem;
                error.hidden = false;
                name.focus();
                return false;
              }
            }
            const parsed = parseInput(type, text());
            if (!parsed.ok) {
              error.textContent = parsed.error;
              error.hidden = false;
              control()?.focus();
              return false;
            }
            result = { name: fieldName, value: parsed.value, keepType: parsed.keepType };
            return true;
          },
        },
      ],
      { width: 520, onClose: () => resolve(result) },
    );
    (options.askName ? name : (control() ?? select)).focus();
  });
}

/** Asks for a field's new name; resolves null when cancelled. */
export function openRenameDialog(path: string, current: string): Promise<string | null> {
  return new Promise((resolve) => {
    let result: string | null = null;
    const input = textInput(current, { spellcheck: false });
    const error = h("div", { class: "md-edit-error", role: "alert", hidden: true });
    openModal(
      "Rename field",
      h("div", { class: "md-edit" }, h("div", { class: "md-edit-path" }, path), field("New name", input), error),
      [
        { label: "Cancel", run: () => undefined },
        {
          label: "Rename",
          primary: true,
          run: () => {
            const name = input.value.trim();
            const problem = fieldNameProblem(name);
            if (problem) {
              error.textContent = problem;
              error.hidden = false;
              return false;
            }
            result = name;
            return true;
          },
        },
      ],
      { width: 420, onClose: () => resolve(result) },
    );
    input.select();
  });
}

/** Asks for a new document in mongosh syntax (insert, duplicate); resolves the document or null. */
export function openDocumentDialog(title: string, text: string): Promise<Doc | null> {
  return new Promise((resolve) => {
    let result: Doc | null = null;
    const area = h("textarea", { class: "input md-edit-area md-mono", spellcheck: "false", rows: 16 }) as HTMLTextAreaElement;
    area.value = text;
    const error = h("div", { class: "md-edit-error", role: "alert", hidden: true });
    const hint = h(
      "div",
      { class: "md-edit-hint" },
      "mongosh syntax. Without an _id the server gives the document a new ObjectId. Nothing is written until you save the pending changes.",
    );
    openModal(
      title,
      h("div", { class: "md-edit" }, area, hint, error),
      [
        { label: "Cancel", run: () => undefined },
        {
          label: "Add to pending changes",
          primary: true,
          run: () => {
            const parsed = parseDocumentInput(area.value);
            if (!parsed.ok) {
              error.textContent = parsed.error;
              error.hidden = false;
              area.focus();
              area.setSelectionRange(parsed.position, parsed.position);
              return false;
            }
            result = parsed.document;
            return true;
          },
        },
      ],
      { width: 620, onClose: () => resolve(result) },
    );
    area.focus();
  });
}

/** The lines of a diff, each marked by kind. */
export function diffList(entries: readonly DiffEntry[]): HTMLElement {
  return h(
    "ul",
    { class: "md-diff" },
    ...entries.map((entry) => h("li", { class: `md-diff-line md-diff-${entry.kind}`, title: entryText(entry) }, entryText(entry))),
  );
}

export interface ReviewItem {
  /** "_id: ObjectId('…')" or "New document". */
  title: string;
  kind: "edited" | "deleted" | "new";
  before: Doc | null;
  after: Doc | null;
  discard(): void;
}

const KIND_LABELS: Record<ReviewItem["kind"], string> = { edited: "Edited", deleted: "Deleted", new: "New" };

/** Lists the pending changes as a diff per document; Save runs `save` (the dialog closes first). */
export function openReviewDialog(items: () => ReviewItem[], save: () => void, discardAll: () => void): void {
  const list = h("div", { class: "md-review" });
  let close = () => undefined as void;
  const draw = () => {
    const current = items();
    if (current.length === 0) {
      close();
      return;
    }
    list.replaceChildren(
      ...current.map((item) => {
        const discard = h("button", { class: "link-button", type: "button" }, "Discard");
        discard.addEventListener("click", () => {
          item.discard();
          draw();
        });
        const diff = item.kind === "edited" && item.before && item.after ? diffDocuments(item.before, item.after) : [];
        const detail =
          item.kind === "edited" ? diffList(diff) : h("pre", { class: "md-review-doc" }, shellText(item.kind === "new" ? item.after : item.before, 2));
        return h(
          "section",
          { class: `md-review-item md-review-${item.kind}` },
          h(
            "div",
            { class: "md-review-head" },
            h("span", { class: "md-review-badge" }, KIND_LABELS[item.kind]),
            h("span", { class: "md-review-title" }, item.title),
            item.kind === "edited" ? h("span", { class: "md-review-summary" }, diffSummary(diff)) : null,
            h("span", { class: "spacer" }),
            discard,
          ),
          detail,
        );
      }),
    );
  };
  draw();
  close = openModal(
    "Pending changes",
    list,
    [
      { label: "Discard all", alignLeft: true, danger: true, run: () => discardAll() },
      { label: "Close", run: () => undefined },
      { label: "Save", primary: true, run: () => save() },
    ],
    { width: 720 },
  );
}
