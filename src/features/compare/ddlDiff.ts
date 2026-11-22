/**
 * The side-by-side DDL diff of one compare item (target on the left, source on the right, scroll-synced) and a small
 * SQL highlighter for the read-only DDL and script panes (keywords, strings, numbers, comments, WARNING lines).
 */
import { h } from "../../core/dom";
import { sideBySide, textLines, type SideRow } from "./lineDiff";

const KEYWORDS = new Set(
  (
    "add alter and as asc begin by cascade check column comment commit constraint create default delete desc drop exists " +
    "foreign from generated identity if in index is key materialized no not null on or primary references replace " +
    "restrict rollback schema select set table to transaction type unique update using view where with always stored " +
    "action include concurrently refresh data join left inner group order limit case when then else end distinct"
  ).split(" "),
);

const TOKENS = /(--[^\n]*)|('(?:[^']|'')*'?)|("(?:[^"]|"")*"?)|\b(\d+(?:\.\d+)?)\b|\b([A-Za-z_][A-Za-z0-9_]*)\b/g;

/** `text` as highlighted spans (no HTML parsing: every piece is a text node). */
export function highlightSql(text: string): DocumentFragment {
  const fragment = document.createDocumentFragment();
  let last = 0;
  for (const match of text.matchAll(TOKENS)) {
    const index = match.index ?? 0;
    const [token, comment, string, quoted, number, word] = match;
    let kind: string | null = null;
    if (comment) kind = /^--\s*WARNING/i.test(comment) ? "sql-warning" : "sql-comment";
    else if (string) kind = "sql-string";
    else if (quoted) kind = null;
    else if (number) kind = "sql-number";
    else if (word && KEYWORDS.has(word.toLowerCase())) kind = "sql-keyword";
    if (!kind) continue;
    if (index > last) fragment.append(text.slice(last, index));
    fragment.append(h("span", { class: kind }, token));
    last = index + token.length;
  }
  if (last < text.length) fragment.append(text.slice(last));
  return fragment;
}

export interface DdlDiffLabels {
  left: string;
  right: string;
}

/** Two `pre` panes; changed lines are marked on both sides, and a line missing on one side leaves a blank there. */
export class DdlDiff {
  readonly element: HTMLElement;
  private readonly leftTitle = h("div", { class: "ddl-diff-title" });
  private readonly rightTitle = h("div", { class: "ddl-diff-title" });
  private readonly left = h("pre", { class: "ddl ddl-diff-pane" });
  private readonly right = h("pre", { class: "ddl ddl-diff-pane" });
  private syncing = false;

  constructor() {
    this.element = h(
      "div",
      { class: "ddl-diff" },
      h("div", { class: "ddl-diff-side" }, this.leftTitle, this.left),
      h("div", { class: "ddl-diff-side" }, this.rightTitle, this.right),
    );
    this.syncScroll(this.left, this.right);
    this.syncScroll(this.right, this.left);
  }

  /** `left`: the target's DDL (what is there); `right`: the source's (what it should become). Null: the side has none. */
  show(left: string | null, right: string | null, labels: DdlDiffLabels): void {
    this.leftTitle.textContent = labels.left;
    this.rightTitle.textContent = labels.right;
    const rows = sideBySide(textLines(left), textLines(right));
    this.left.replaceChildren(...(left === null ? [absent("Not in the target")] : rows.map((row) => line(row, "left"))));
    this.right.replaceChildren(...(right === null ? [absent("Not in the source")] : rows.map((row) => line(row, "right"))));
    this.left.scrollTop = 0;
    this.right.scrollTop = 0;
  }

  clear(message: string): void {
    this.leftTitle.textContent = "";
    this.rightTitle.textContent = "";
    this.left.replaceChildren(absent(message));
    this.right.replaceChildren();
  }

  private syncScroll(from: HTMLElement, to: HTMLElement): void {
    from.addEventListener(
      "scroll",
      () => {
        if (this.syncing) return;
        this.syncing = true;
        to.scrollTop = from.scrollTop;
        to.scrollLeft = from.scrollLeft;
        requestAnimationFrame(() => (this.syncing = false));
      },
      { passive: true },
    );
  }
}

function absent(message: string): HTMLElement {
  return h("span", { class: "ddl-diff-absent" }, message);
}

/** One line of a pane: its number and text; `filler` where only the other side has a line. */
function line(row: SideRow, side: "left" | "right"): HTMLElement {
  const text = side === "left" ? row.left : row.right;
  const number = side === "left" ? row.leftNumber : row.rightNumber;
  const kind = text === null ? "filler" : row.kind === "equal" ? "same" : row.kind === "change" ? "changed" : side === "left" ? "removed" : "added";
  const body = h("span", { class: "ddl-line-text" });
  if (text !== null) body.append(highlightSql(text));
  return h("span", { class: `ddl-line ${kind}` }, h("span", { class: "ddl-line-no" }, number === null ? "" : String(number)), body);
}
