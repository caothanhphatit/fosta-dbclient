import { MariaSQL, sql } from "@codemirror/lang-sql";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { Compartment, Prec } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { basicSetup } from "codemirror";

const language = new Compartment();

const highlightStyle = HighlightStyle.define([
  { tag: tags.keyword, color: "var(--syn-keyword)", fontWeight: "600" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--syn-string)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--syn-number)" },
  { tag: [tags.comment, tags.lineComment, tags.blockComment], color: "var(--syn-comment)", fontStyle: "italic" },
  { tag: [tags.typeName, tags.standard(tags.name)], color: "var(--syn-type)" },
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
  ".cm-tooltip": { backgroundColor: "var(--bg-panel)", color: "var(--fg)", border: "1px solid var(--border)" },
  ".cm-tooltip-autocomplete ul li[aria-selected]": { backgroundColor: "var(--accent)", color: "var(--accent-fg)" },
});

export function createSqlEditor(parent: HTMLElement, doc: string, onRun: () => void): EditorView {
  const run = () => {
    onRun();
    return true;
  };
  return new EditorView({
    parent,
    doc,
    extensions: [
      Prec.highest(
        keymap.of([
          { key: "Mod-Enter", run },
          { key: "F9", run },
        ]),
      ),
      basicSetup,
      language.of(sqlLanguage([])),
      syntaxHighlighting(highlightStyle),
      editorTheme,
    ],
  });
}

export function setSchemaTables(view: EditorView, tables: string[]): void {
  view.dispatch({ effects: language.reconfigure(sqlLanguage(tables)) });
}

export function selectedOrAllText(view: EditorView): string {
  const { state } = view;
  const selected = state.selection.ranges
    .filter((range) => !range.empty)
    .map((range) => state.sliceDoc(range.from, range.to))
    .join("\n");
  return selected || state.doc.toString();
}

function sqlLanguage(tables: string[]) {
  return sql({
    dialect: MariaSQL,
    upperCaseKeywords: true,
    schema: Object.fromEntries(tables.map((table) => [table, []])),
  });
}
