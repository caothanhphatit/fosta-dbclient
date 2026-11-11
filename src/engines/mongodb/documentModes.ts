/**
 * The ways the Documents section shows a page of documents. Each mode is a renderer the section creates on first use
 * and keeps while the tab is open; switching modes shows the same page at the same document (each mode tells the
 * index of its top document and scrolls to one; the section anchors it by `_id`, `anchor.ts`).
 *
 * - List: the virtualised document tree (`docTree.ts`), readable BSON, inferred references; values, fields and documents
 *   are edited from it (double-click, the card tools, the menu) when the section allows edits.
 * - JSON: the page as Extended JSON in a read-only editor.
 * - Table: the page flattened to dotted-path columns (`flatten.ts`) in the shared grid (no inline editing: a double-click
 *   or the menu opens the value editor, edits are marked per cell); arrays and deep
 *   documents are one cell that opens in the side panel.
 */
import type { ColumnInfo, Value, ValueKind as GridKind } from "../../core/contract";
import type { MenuItem } from "../../core/contextMenu";
import { h } from "../../core/dom";
import { DataGrid, gridSetup, type CellView } from "../../features/grid/grid";
import { documentAtLine, jsonDocumentLines } from "./anchor";
import { bsonType, readable, shellText, type BsonType } from "./bson";
import { DocTree, type TreeEditing, type TreeRefs } from "./docTree";
import { foldedText, isNested, type Doc, type Json } from "./documents";
import { columnTypeText, flatten, isOddCell, MAX_COLUMNS, type FlatTable } from "./flatten";
import { createJsonViewer } from "./queryEditor";
import type { DocumentsMode } from "./queryModel";
import { isRefName } from "./refs";

export interface DocumentsRenderer {
  readonly element: HTMLElement;
  /** Shows a page; `offset` is the position of its first document among all matches. */
  show(documents: Doc[], offset: number): void;
  /** The index (in the page) of the document at the top of the view. */
  topDocument(): number;
  scrollToDocument(index: number): void;
  /** Draws again what depends on the section (an inferred reference became known). */
  refresh?(): void;
  /** Shows new versions of the same documents (pending edits applied), keeping the position. */
  replace?(documents: Doc[]): void;
  dispose?(): void;
}

/** What a mode needs from the section. */
export interface RendererContext {
  refs: TreeRefs;
  /** Opens a value (an array, a document) in the side panel. */
  inspect(title: string, value: Json): void;
  /** Editing (a collection the workspace may write); null: read-only. Indexes are positions in the page. */
  edit: TreeEditing | null;
}

export interface ModeSpec {
  id: DocumentsMode;
  label: string;
  title: string;
  create(ctx: RendererContext): DocumentsRenderer;
}

export const MODES: ModeSpec[] = [
  { id: "list", label: "List", title: "Documents as cards with fields to expand", create: (ctx) => new ListRenderer(ctx) },
  { id: "json", label: "JSON", title: "The page as Extended JSON", create: () => new JsonRenderer() },
  { id: "table", label: "Table", title: "Fields as columns (nested fields as dotted paths)", create: (ctx) => new TableRenderer(ctx) },
];

class ListRenderer implements DocumentsRenderer {
  private readonly tree: DocTree;
  readonly element: HTMLElement;

  constructor(ctx: RendererContext) {
    this.tree = new DocTree({ refs: ctx.refs, numbered: true, edit: ctx.edit ?? undefined });
    this.element = h("div", { class: "md-list" }, this.tree.element);
  }

  show(documents: Doc[], offset: number): void {
    this.tree.show(documents, offset);
  }

  topDocument(): number {
    return this.tree.topDocument();
  }

  scrollToDocument(index: number): void {
    this.tree.scrollToDocument(index);
  }

  refresh(): void {
    this.tree.refresh();
  }

  replace(documents: Doc[]): void {
    this.tree.replace(documents);
  }

  dispose(): void {
    this.tree.dispose();
  }
}

class JsonRenderer implements DocumentsRenderer {
  readonly element = h("div", { class: "md-json" });
  private readonly viewer = createJsonViewer(this.element);
  private starts: number[] = [];

  show(documents: Doc[]): void {
    this.viewer.set(JSON.stringify(documents, null, 2));
    this.starts = jsonDocumentLines(documents);
    this.viewer.scrollToLine(0);
  }

  replace(documents: Doc[]): void {
    const top = this.viewer.topLine();
    this.viewer.set(JSON.stringify(documents, null, 2));
    this.starts = jsonDocumentLines(documents);
    this.viewer.scrollToLine(top);
  }

  topDocument(): number {
    return Math.max(0, documentAtLine(this.starts, this.viewer.topLine()));
  }

  scrollToDocument(index: number): void {
    this.viewer.scrollToLine(index <= 0 ? 0 : (this.starts[index] ?? 0));
  }

  dispose(): void {
    this.viewer.destroy();
  }
}

const NULL: Value = { t: "null", v: null };

/** The grid kind of a column: numbers right-aligned, dates and ids with their badge, anything mixed as text. */
function gridKind(types: BsonType[]): GridKind {
  if (types.length !== 1) return { kind: types.length === 0 ? "text" : "other" };
  switch (types[0]) {
    case "number":
    case "long":
    case "decimal":
      return { kind: "float" };
    case "bool":
      return { kind: "bool" };
    case "date":
      return { kind: "timestampTz" };
    case "objectId":
      return { kind: "objectId" };
    case "array":
    case "object":
      return { kind: "json" };
    default:
      return { kind: "text" };
  }
}

/** The grid value of a cell: what Copy and the header sort see (mongosh syntax; plain numbers stay numbers). */
function gridValue(value: Json): Value {
  if (value === null || value === undefined) return NULL;
  if (typeof value === "number") return { t: "float", v: value };
  if (typeof value === "boolean") return { t: "bool", v: value };
  if (typeof value === "string") return { t: "text", v: value };
  return { t: "text", v: shellText(value) };
}

class TableRenderer implements DocumentsRenderer {
  readonly element: HTMLElement;
  private readonly grid = new DataGrid();
  private readonly note = h("div", { class: "mt-note", hidden: true });
  private documents: Doc[] = [];
  private table: FlatTable = { columns: [], rows: [], hiddenPaths: 0 };
  private offset = 0;

  constructor(private readonly ctx: RendererContext) {
    this.element = h("div", { class: "mt-table" }, this.grid.element, this.note);
  }

  show(documents: Doc[], offset: number): void {
    this.documents = documents;
    this.offset = offset;
    this.table = flatten(documents);
    this.load();
    this.grid.element.scrollTop = 0;
    this.grid.element.scrollLeft = 0;
    const hidden = this.table.hiddenPaths;
    this.note.hidden = hidden === 0;
    this.note.textContent = hidden
      ? `${hidden} more ${hidden === 1 ? "field is" : "fields are"} not shown as columns (at most ${MAX_COLUMNS}, the fields most documents have). List and JSON show every field.`
      : "";
  }

  topDocument(): number {
    return Math.max(0, this.grid.topRow());
  }

  scrollToDocument(index: number): void {
    this.grid.scrollToRow(index);
  }

  refresh(): void {
    const top = this.grid.element.scrollTop;
    const left = this.grid.element.scrollLeft;
    this.load();
    this.grid.element.scrollTop = top;
    this.grid.element.scrollLeft = left;
  }

  replace(documents: Doc[]): void {
    this.documents = documents;
    this.table = flatten(documents);
    this.refresh();
  }

  dispose(): void {
    this.grid.dispose();
  }

  private load(): void {
    const table = this.table;
    const columns: ColumnInfo[] = table.columns.map((column) => ({
      name: column.path,
      typeName: columnTypeText(column),
      kind: gridKind(column.types),
      nullable: true,
      default: null,
      comment: null,
      identity: null,
      generated: null,
      hints: { primary: column.path === "_id", readOnly: true },
    }));
    const rows = table.rows.map((row) => row.map((cell) => (cell.missing ? NULL : gridValue(cell.value))));
    const setup = gridSetup(columns, rows);
    setup.rowOffset = this.offset;
    setup.clientSort = true;
    setup.readOnlyReason = this.ctx.edit
      ? "Double-click a value (or use its menu) to edit it; edits stay pending until you save them."
      : "These documents are read-only here.";
    setup.cellView = (row, column) => this.cellView(row, column);
    setup.onCellOpen = (row, column) => this.open(row, column);
    setup.cellMenu = (row, column) => this.menu(row, column);
    this.grid.load(setup);
  }

  private cell(row: number, column: number): { path: string; value: Json } | null {
    const cell = this.table.rows[row]?.[column];
    const meta = this.table.columns[column];
    if (!cell || !meta || cell.missing) return null;
    return { path: meta.path, value: cell.value };
  }

  private refTarget(path: string, value: Json): string | null {
    return bsonType(value) === "objectId" && isRefName(path) ? this.ctx.refs.targetOf(path) : null;
  }

  /** The pending-edit mark of a cell (" mt-changed", " mt-added", " mt-deleted"), empty when none. */
  private mark(row: number, path: string): string {
    const edit = this.ctx.edit;
    if (!edit) return "";
    if (edit.docState(row) === "deleted") return " mt-deleted";
    const state = edit.pathState(row, path.split("."));
    return state === "set" ? " mt-changed" : state === "added" ? " mt-added" : state === "inside" ? " mt-changed" : "";
  }

  private cellView(row: number, column: number): CellView {
    const view = this.plainCellView(row, column);
    const meta = this.table.columns[column];
    const mark = meta ? this.mark(row, meta.path) : "";
    return mark ? { ...view, className: `${view.className ?? ""}${mark}` } : view;
  }

  private plainCellView(row: number, column: number): CellView {
    const meta = this.table.columns[column];
    const cell = this.table.rows[row]?.[column];
    if (!meta || !cell || cell.missing) {
      const title = this.ctx.edit ? "No such field in this document: double-click to add it" : "No such field in this document";
      return { text: "", title, className: "mt-missing" };
    }
    const odd = isOddCell(meta, cell) ? " mt-odd" : "";
    const value = cell.value;
    if (isNested(value)) {
      const what = Array.isArray(value) ? "the items" : "the document";
      return { text: foldedText(value), title: `Double-click to open ${what} in the side panel`, className: `mt-nested${odd}` };
    }
    const shown = readable(value);
    const target = this.refTarget(meta.path, value);
    const oddTitle = odd ? ` · ${shown.type} in a column of mostly ${meta.types[0]}` : "";
    if (target) {
      return {
        text: shown.text,
        title: `${shown.title ?? ""}\nInferred reference to ${target}: double-click to open it${oddTitle}`.trim(),
        className: `mt-value md-${shown.kind} mt-ref${odd}`,
      };
    }
    const numeric = shown.kind === "number" ? " numeric" : "";
    const title = shown.title ? `${shown.title}${oddTitle}` : odd ? oddTitle.slice(3) : null;
    return { text: shown.kind === "null" ? "null" : shown.text, title, className: `mt-value md-${shown.kind}${numeric}${odd}` };
  }

  private open(row: number, column: number): boolean {
    const cell = this.cell(row, column);
    const edit = this.ctx.edit;
    const meta = this.table.columns[column];
    if (!cell) {
      // A field this document does not have: add it at that path.
      if (!edit || !meta || edit.docState(row) === "deleted") return false;
      edit.editValue(row, meta.path.split("."));
      return true;
    }
    if (isNested(cell.value)) {
      this.ctx.inspect(`${cell.path} · document ${this.offset + row + 1}`, cell.value);
      return true;
    }
    const target = this.refTarget(cell.path, cell.value);
    if (target) {
      this.ctx.refs.open(target, cell.value, cell.path);
      return true;
    }
    if (edit && edit.docState(row) !== "deleted" && cell.path !== "_id") {
      edit.editValue(row, cell.path.split("."));
      return true;
    }
    return false;
  }

  private editItems(row: number, column: number): MenuItem[] {
    const edit = this.ctx.edit;
    const meta = this.table.columns[column];
    if (!edit || !this.documents[row]) return [];
    if (edit.docState(row) === "deleted") return [{ label: "Restore document", icon: "history", run: () => edit.restoreDocument(row) }, "separator"];
    const items: MenuItem[] = [];
    if (meta && meta.path !== "_id") {
      const path = meta.path.split(".");
      const present = this.cell(row, column) !== null;
      items.push({ label: present ? "Edit value…" : `Add ${meta.path}…`, icon: "edit", run: () => edit.editValue(row, path) });
      if (present && path.length > 0) {
        items.push({ label: "Rename field…", run: () => edit.renameField(row, path) });
        items.push({ label: "Remove field", icon: "trash", run: () => edit.removeField(row, path) });
      }
    }
    items.push(
      { label: "Add field to document…", icon: "plus", run: () => edit.addField(row, []) },
      { label: "Duplicate document…", icon: "layers", run: () => edit.duplicate(row) },
      { label: "Delete document", icon: "trash", run: () => edit.deleteDocument(row) },
      "separator",
    );
    return items;
  }

  private menu(row: number, column: number): MenuItem[] {
    const items: MenuItem[] = this.editItems(row, column);
    const cell = this.cell(row, column);
    if (cell && isNested(cell.value)) {
      items.push({ label: Array.isArray(cell.value) ? "Open items" : "Open document", icon: "expand", run: () => void this.open(row, column) });
    }
    const target = cell ? this.refTarget(cell.path, cell.value) : null;
    if (cell && target)
      items.push({ label: `Open referenced document in ${target}`, icon: "expand", run: () => this.ctx.refs.open(target, cell.value, cell.path) });
    const document = this.documents[row];
    if (document) {
      items.push({ label: "Open whole document", run: () => this.ctx.inspect(`Document ${this.offset + row + 1}`, document) });
    }
    return items;
  }
}
