/**
 * The query tab's CodeMirror editor. The language (highlighting, completion) is not fixed here: it comes from the
 * engine's `LanguageSupport.editorExtensions` and is swapped in with `setEditorLanguage` (when it has loaded, or when
 * the database or the language changes).
 */
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { Compartment, Prec, type Extension } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { basicSetup } from "codemirror";

const language = new Compartment();

const highlightStyle = HighlightStyle.define([
  { tag: tags.keyword, color: "var(--syn-keyword)", fontWeight: "600" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--syn-string)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--syn-number)" },
  { tag: [tags.comment, tags.lineComment, tags.blockComment], color: "var(--syn-comment)", fontStyle: "italic" },
  { tag: [tags.typeName, tags.standard(tags.name), tags.propertyName], color: "var(--syn-type)" },
  { tag: [tags.operator, tags.punctuation], color: "var(--fg-muted)" },
]);

const editorTheme = EditorView.theme({
  "&": { height: "100%", backgroundColor: "var(--editor-bg)", color: "var(--fg)", fontSize: "13px" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.55" },
  ".cm-content": { caretColor: "var(--fg)" },
  ".cm-gutters": { backgroundColor: "var(--bg-sidebar)", color: "var(--fg-muted)", borderRight: "1px solid var(--border)" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "var(--active-line)" },
  "&.cm-focused .cm-cursor": { borderLeftColor: "var(--fg)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground": {
    backgroundColor: "var(--selection) !important",
  },
  // Below modal overlays (z-index 40), so a completion list never stays drawn above a dialog.
  ".cm-tooltip": { zIndex: "35", backgroundColor: "var(--bg-panel)", color: "var(--fg)", border: "1px solid var(--border)" },
  ".cm-tooltip-autocomplete ul li[aria-selected]": { backgroundColor: "var(--accent)", color: "var(--accent-fg)" },
});

export interface EditorCommands {
  /** Ctrl+Enter / F9: the selection, or everything. */
  run(): void;
  /** Ctrl+Shift+Enter: the selection, or the statement under the cursor. */
  runStatement(): void;
  /** Shift+Alt+F. */
  format(): void;
  /** The text changed (typing, paste, format, an insert from the history). */
  changed?(): void;
}

export function createSqlEditor(parent: HTMLElement, doc: string, commands: EditorCommands): EditorView {
  const command = (action: () => void) => () => {
    action();
    return true;
  };
  return new EditorView({
    parent,
    doc,
    extensions: [
      Prec.highest(
        keymap.of([
          { key: "Mod-Shift-Enter", run: command(commands.runStatement) },
          { key: "Mod-Enter", run: command(commands.run) },
          { key: "F9", run: command(commands.run) },
          { key: "Shift-Alt-f", run: command(commands.format) },
        ]),
      ),
      basicSetup,
      language.of([]),
      syntaxHighlighting(highlightStyle),
      editorTheme,
      EditorView.updateListener.of((update) => {
        if (update.docChanged) commands.changed?.();
      }),
    ],
  });
}

/** Replaces the language extensions (from `LanguageSupport.editorExtensions`). */
export function setEditorLanguage(view: EditorView, extensions: Extension[]): void {
  view.dispatch({ effects: language.reconfigure(extensions) });
}

export function selectedOrAllText(view: EditorView): string {
  const { state } = view;
  const selected = state.selection.ranges
    .filter((range) => !range.empty)
    .map((range) => state.sliceDoc(range.from, range.to))
    .join("\n");
  return selected || state.doc.toString();
}
