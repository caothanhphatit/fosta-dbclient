/**
 * Editing in the Documents section (List and Table modes): the pending edits (`docEdits.ts`), the dialogs that make
 * them, the pending-changes bar (the grid's `pending-bar`: Review, Discard, Save) and the save.
 *
 * Save sends every pending change in one `apply_edits` call (`Caller::GridSave`), so the workspace's rules apply as for
 * a grid save: a Read-only workspace blocks it, confirmations follow the rules. Each update is matched by `_id` and the
 * loaded values of its edited paths; a `CONFLICT` opens the grid's conflict dialog (Overwrite re-sends that document's
 * change with `force`, Take theirs drops it), and nothing of the failed save was written. On a standalone server the
 * backend undoes what it wrote when a later change fails (see `tabular.rs`).
 */
import type { ObjectRef } from "../../core/contract";
import { toast } from "../../core/dialogs";
import { h, icon, spacer } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import { openConflictDialog } from "../../features/grid/conflictDialog";
import { conflictPayload, conflictRows, type ConflictPayload } from "../../features/grid/conflicts";
import { readable } from "./bson";
import { getAt, PendingEdits, type BuiltChange } from "./docEdits";
import type { TreeEditing } from "./docTree";
import type { Doc } from "./documents";
import { openDocumentDialog, openRenameDialog, openReviewDialog, openValueDialog, type ReviewItem } from "./editDialogs";
import type { MongoApi } from "./mongoApi";
import { duplicateText } from "./valueInput";

export interface DocumentEditingOptions {
  api: MongoApi;
  object: ObjectRef;
  /** The documents of the page shown, as loaded. */
  documents(): readonly Doc[];
  /** The pending edits changed: the views show the documents again. */
  changed(): void;
  /** Reads the page again (after a save). */
  reload(): Promise<void>;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;

export class DocumentEditing {
  readonly edits: PendingEdits;
  readonly bar: HTMLElement;
  readonly tree: TreeEditing;
  private readonly text = h("span", { class: "pending-text" });
  private readonly saveButton = h("button", { class: "btn primary", type: "button", title: "Save the pending changes" }, "Save");
  private readonly reviewButton = h("button", { class: "btn", type: "button", title: "Each changed document as a diff" }, "Review…");
  private saving = false;

  constructor(private readonly options: DocumentEditingOptions) {
    this.edits = new PendingEdits(() => {
      this.updateBar();
      options.changed();
    });
    const discard = h("button", { class: "btn", type: "button" }, "Discard");
    discard.addEventListener("click", () => this.edits.discardAll());
    this.reviewButton.addEventListener("click", () => this.review());
    this.saveButton.addEventListener("click", () => void this.save());
    this.bar = h("div", { class: "pending-bar mq-pending", role: "status" }, icon("edit"), this.text, spacer(), this.reviewButton, discard, this.saveButton);
    this.bar.hidden = true;
    const doc = (index: number) => options.documents()[index];
    this.tree = {
      pathState: (index, path) => {
        const document = doc(index);
        return document ? this.edits.pathState(document._id, path) : null;
      },
      docState: (index) => {
        const document = doc(index);
        if (!document) return null;
        if (this.edits.isDeleted(document._id)) return "deleted";
        return this.edits.opsOf(document._id).size > 0 ? "edited" : null;
      },
      editValue: (index, path) => void this.editValue(doc(index), path),
      addField: (index, parent) => void this.addField(doc(index), parent),
      renameField: (index, path) => void this.renameField(doc(index), path),
      removeField: (index, path) => this.attempt(() => this.edits.removeField(doc(index), path)),
      duplicate: (index) => void this.duplicate(doc(index)),
      deleteDocument: (index) => this.attempt(() => this.edits.deleteDocument(doc(index))),
      restoreDocument: (index) => {
        const document = doc(index);
        if (document) this.edits.discardDocument(document._id);
      },
    };
  }

  /** The page's documents with their pending edits. */
  shown(documents: readonly Doc[]): Doc[] {
    return documents.map((document) => this.edits.effective(document));
  }

  async insert(): Promise<void> {
    const document = await openDocumentDialog("Insert document", "{\n  \n}");
    if (document) this.attempt(() => this.edits.insertDocument(document));
  }

  /** "2 documents with unsaved edits" for closing the tab; null when nothing is pending. */
  warning(): string | null {
    return this.edits.isEmpty() ? null : `${plural(this.edits.count(), "document")} with unsaved changes in ${this.options.object.name}`;
  }

  private attempt(run: () => void): void {
    try {
      run();
    } catch (error) {
      toast(error instanceof Error ? error.message : String(error), "error");
    }
  }

  private async editValue(document: Doc | undefined, path: string[]): Promise<void> {
    if (!document) return;
    const current = getAt(this.edits.effective(document), path);
    const result = await openValueDialog({
      title: current.found ? "Edit value" : "Add field",
      path: path.join("."),
      value: current.found ? current.value : undefined,
    });
    if (result) this.attempt(() => this.edits.setValue(document, path, result.value, result.keepType));
  }

  private async addField(document: Doc | undefined, parent: string[]): Promise<void> {
    if (!document) return;
    const container = getAt(this.edits.effective(document), parent).value;
    const array = Array.isArray(container);
    const where = parent.length > 0 ? parent.join(".") : `document _id: ${readable(document._id).text}`;
    const result = await openValueDialog({ title: array ? "Add item" : "Add field", path: array ? `${where} (new last item)` : where, askName: !array });
    if (result) this.attempt(() => this.edits.addField(document, parent, result.name, result.value, result.keepType));
  }

  private async renameField(document: Doc | undefined, path: string[]): Promise<void> {
    if (!document) return;
    const name = await openRenameDialog(path.join("."), path[path.length - 1]);
    if (name !== null) this.attempt(() => this.edits.renameField(document, path, name));
  }

  private async duplicate(document: Doc | undefined): Promise<void> {
    if (!document) return;
    const copy = await openDocumentDialog("Duplicate document", duplicateText(this.edits.effective(document)));
    if (copy) this.attempt(() => this.edits.insertDocument(copy));
  }

  private updateBar(): void {
    const summary = this.edits.summary();
    const parts: string[] = [];
    if (summary.updated > 0) parts.push(`${plural(summary.updated, "document")} edited (${plural(summary.paths, "field")})`);
    if (summary.deleted > 0) parts.push(`${plural(summary.deleted, "document")} to delete`);
    if (summary.inserted > 0) parts.push(`${plural(summary.inserted, "new document")}`);
    this.text.textContent = `Pending: ${parts.join(", ")}`;
    this.bar.hidden = this.edits.isEmpty();
    this.saveButton.disabled = this.saving;
  }

  private reviewItems(): ReviewItem[] {
    const items: ReviewItem[] = this.edits.insertsList().map((insert) => ({
      title: insert.document._id === undefined ? "New document" : `New document, _id: ${readable(insert.document._id).text}`,
      kind: "new" as const,
      before: null,
      after: insert.document,
      discard: () => this.edits.removeInsert(insert.id),
    }));
    for (const { id, original, deleted } of this.edits.editedDocuments()) {
      items.push({
        title: `_id: ${readable(id).text}`,
        kind: deleted ? "deleted" : "edited",
        before: original,
        after: deleted ? null : this.edits.effective(original),
        discard: () => this.edits.discardDocument(id),
      });
    }
    return items;
  }

  private review(): void {
    if (this.edits.isEmpty()) return;
    openReviewDialog(
      () => this.reviewItems(),
      () => void this.save(),
      () => this.edits.discardAll(),
    );
  }

  async save(): Promise<void> {
    const built = this.edits.changes();
    if (built.length === 0 || this.saving) return;
    const object = this.options.object;
    this.saving = true;
    this.updateBar();
    try {
      const result = await this.options.api.applyEdits({ object, changes: built.map((entry) => entry.change) });
      if (!result) return;
      const done = [
        result.updated ? `${plural(result.updated, "document")} updated` : "",
        result.inserted ? `${plural(result.inserted, "document")} inserted` : "",
        result.deleted ? `${plural(result.deleted, "document")} deleted` : "",
      ].filter(Boolean);
      toast(`Saved: ${done.join(", ") || "nothing changed"}`, "success");
      this.edits.discardAll();
      await this.options.reload();
    } catch (error) {
      const conflict = conflictPayload(error);
      if (conflict) this.showConflicts(conflict, built);
      else showError(error, `Save changes to ${object.name}`);
    } finally {
      this.saving = false;
      this.updateBar();
    }
  }

  private drop(entry: BuiltChange | undefined): void {
    if (!entry) return;
    if (entry.insert) this.edits.removeInsert(entry.insert);
    else if (entry.id !== undefined) this.edits.discardDocument(entry.id);
  }

  /** The grid's conflict dialog; nothing of the save was written, so every edit is still pending. */
  private showConflicts(payload: ConflictPayload, built: BuiltChange[]): void {
    const object = this.options.object;
    openConflictDialog({
      objectLabel: object.name,
      payload,
      rows: conflictRows(payload.conflicts),
      otherPending: Math.max(0, built.length - payload.conflicts.length),
      overwrite: async (row) => {
        const entry = built[row.index];
        if (!entry || entry.change.op === "insert") return false;
        const result = await this.options.api.applyEdits({ object, changes: [{ ...entry.change, force: true }] });
        if (result === null) return false;
        this.drop(entry);
        return true;
      },
      takeTheirs: async (row) => {
        this.drop(built[row.index]);
        return true;
      },
      onClose: () => void this.options.reload(),
    });
  }
}
