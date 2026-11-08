// Editor helpers for the query view's "Run current statement" and "Format" actions. No language package is imported
// here, so the query view can use them without loading lang-sql.
import type { EditorState } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import type { LanguageSupport, TextRange } from "./types";

export interface CurrentStatement extends TextRange {
  text: string;
}

/**
 * What "Run current statement" runs: the selection when there is one, else the statement under the cursor (null
 * when the editor holds no statement).
 */
export function currentStatement(state: EditorState, statementAtFn: (text: string, offset: number) => TextRange | null): CurrentStatement | null {
  const sel = state.selection.main;
  if (!sel.empty) return { from: sel.from, to: sel.to, text: state.sliceDoc(sel.from, sel.to) };
  const doc = state.doc.toString();
  const range = statementAtFn(doc, sel.head);
  if (!range || range.from === range.to) return null;
  return { ...range, text: doc.slice(range.from, range.to) };
}

/**
 * Formats the selection (or the whole document) with `support.format` and replaces it in one undoable change; a
 * formatted selection stays selected. Returns false when the language has no formatter, the text did not change, or
 * the document changed while the formatter ran.
 */
export async function formatInEditor(view: EditorView, support: Pick<LanguageSupport, "format">): Promise<boolean> {
  if (!support.format) return false;
  const sel = view.state.selection.main;
  const from = sel.empty ? 0 : sel.from;
  const to = sel.empty ? view.state.doc.length : sel.to;
  const before = view.state.sliceDoc(from, to);
  const after = await support.format(before);
  // The document may have changed while the formatter ran.
  if (after === before || view.state.sliceDoc(from, to) !== before) return false;
  view.dispatch({
    changes: { from, to, insert: after },
    selection: sel.empty ? undefined : { anchor: from, head: from + after.length },
    userEvent: "input.format",
  });
  return true;
}
