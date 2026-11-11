/**
 * The CodeMirror editors of the query bar: one document in mongosh syntax, a single line that grows into several
 * (Shift+Enter, or pasting text with line breaks), mongosh-like highlighting, completion from `queryCompletion.ts`, and
 * the parse error underlined at its position. Enter runs while the text is one line, Ctrl+Enter always; ArrowUp /
 * ArrowDown on a one-line text step through the recent filters (`onHistory`).
 *
 * Also the read-only JSON viewer of the documents' JSON mode.
 */
import {
  acceptCompletion,
  autocompletion,
  closeBrackets,
  closeBracketsKeymap,
  completionKeymap,
  completionStatus,
  type CompletionSource,
} from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, insertNewlineAndIndent } from "@codemirror/commands";
import { bracketMatching, HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorState, Prec, StateEffect, StateField, type Extension } from "@codemirror/state";
import { Decoration, drawSelection, EditorView, keymap, lineNumbers, placeholder, WidgetType, type DecorationSet } from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { jsonMode } from "../../core/jsonMode";
import { errorRange } from "./queryModel";

const highlightStyle = HighlightStyle.define([
  { tag: tags.keyword, color: "var(--syn-keyword)", fontWeight: "600" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--syn-string)" },
  { tag: [tags.number, tags.bool, tags.null], color: "var(--syn-number)" },
  { tag: [tags.comment, tags.lineComment, tags.blockComment], color: "var(--syn-comment)", fontStyle: "italic" },
  { tag: [tags.typeName, tags.standard(tags.name), tags.propertyName], color: "var(--syn-type)" },
  { tag: [tags.operator, tags.punctuation], color: "var(--fg-2)" },
]);

const sharedTheme = EditorView.theme({
  "&": { backgroundColor: "var(--editor-bg)", color: "var(--fg)", fontSize: "13px" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.5" },
  ".cm-content": { caretColor: "var(--fg)" },
  "&.cm-focused .cm-cursor": { borderLeftColor: "var(--fg)" },
  ".cm-selectionBackground": { backgroundColor: "color-mix(in srgb, var(--accent) 22%, transparent) !important" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground": {
    backgroundColor: "color-mix(in srgb, var(--accent) 38%, transparent) !important",
  },
  ".cm-tooltip": { zIndex: "35", backgroundColor: "var(--surface)", color: "var(--fg)", border: "1px solid var(--border)" },
  ".cm-tooltip-autocomplete ul li[aria-selected]": { backgroundColor: "var(--accent)", color: "var(--accent-fg)" },
  ".cm-gutters": { backgroundColor: "var(--surface-2)", color: "var(--fg-3)", borderRight: "1px solid var(--border)" },
});

const fieldTheme = EditorView.theme({
  "&": { border: "1px solid var(--border)", borderRadius: "var(--radius)", minHeight: "28px" },
  "&.cm-focused": { outline: "none", borderColor: "var(--accent)", boxShadow: "0 0 0 1px var(--accent)" },
  ".cm-scroller": { maxHeight: "168px", overflow: "auto" },
  ".cm-content": { padding: "4px 0" },
  ".cm-line": { padding: "0 8px" },
  ".cm-placeholder": { color: "var(--fg-3)" },
  ".mq-error-mark": { textDecoration: "underline wavy var(--danger)", textUnderlineOffset: "3px", textDecorationSkipInk: "none" },
  // An error at the end of the text (an unclosed brace) has nothing to underline: a marker stands in.
  ".mq-error-end": { borderBottom: "2px solid var(--danger)", paddingLeft: "6px" },
});

const setError = StateEffect.define<{ from: number; to: number } | null>();

const errorField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, transaction) {
    let next = value.map(transaction.changes);
    // Typing clears the underline: the error is about text that is no longer there.
    if (transaction.docChanged) next = Decoration.none;
    for (const effect of transaction.effects) {
      if (!effect.is(setError)) continue;
      if (!effect.value) next = Decoration.none;
      else {
        const { from, to } = effect.value;
        next =
          from < to
            ? Decoration.set([Decoration.mark({ class: "mq-error-mark" }).range(from, to)])
            : Decoration.set([Decoration.widget({ widget: new EndMarker(), side: 1 }).range(from)]);
      }
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

class EndMarker extends WidgetType {
  toDOM(): HTMLElement {
    const mark = document.createElement("span");
    mark.className = "mq-error-end";
    mark.setAttribute("aria-hidden", "true");
    return mark;
  }

  eq(): boolean {
    return true;
  }
}

export interface FieldEditorOptions {
  doc: string;
  placeholder: string;
  label: string;
  completion: CompletionSource;
  run(): void;
  changed?(): void;
  /** ArrowUp (-1) / ArrowDown (1) on a one-line text with no completion open; true when it replaced the text. */
  onHistory?(direction: -1 | 1): boolean;
}

export class FieldEditor {
  readonly view: EditorView;

  constructor(parent: HTMLElement, options: FieldEditorOptions) {
    const run = () => {
      options.run();
      return true;
    };
    const oneLine = (view: EditorView) => !view.state.doc.toString().includes("\n");
    const historyStep = (direction: -1 | 1) => (view: EditorView) => {
      if (!options.onHistory || !oneLine(view) || completionStatus(view.state) !== null) return false;
      return options.onHistory(direction);
    };
    this.view = new EditorView({
      parent,
      doc: options.doc,
      extensions: [
        Prec.highest(keymap.of([{ key: "Mod-Enter", run }])),
        autocompletion({ override: [options.completion], activateOnTyping: true, icons: false }),
        // Below the completion keymap, so Enter picks the highlighted completion first.
        Prec.high(
          keymap.of([
            { key: "Tab", run: acceptCompletion },
            { key: "Enter", run: (view) => (oneLine(view) ? run() : insertNewlineAndIndent(view)) },
            { key: "Shift-Enter", run: insertNewlineAndIndent },
            { key: "ArrowUp", run: historyStep(-1) },
            { key: "ArrowDown", run: historyStep(1) },
          ]),
        ),
        keymap.of([...closeBracketsKeymap, ...completionKeymap, ...defaultKeymap, ...historyKeymap]),
        history(),
        drawSelection(),
        closeBrackets(),
        bracketMatching(),
        jsonMode(),
        syntaxHighlighting(highlightStyle),
        placeholder(options.placeholder),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({ "aria-label": options.label, spellcheck: "false", autocapitalize: "off", autocorrect: "off" }),
        errorField,
        sharedTheme,
        fieldTheme,
        EditorView.updateListener.of((update) => {
          if (update.docChanged) options.changed?.();
        }),
      ],
    });
  }

  get text(): string {
    return this.view.state.doc.toString();
  }

  set text(value: string) {
    if (value === this.text) return;
    this.view.dispatch({ changes: { from: 0, to: this.view.state.doc.length, insert: value }, selection: { anchor: value.length } });
  }

  /** Underlines the token at `position` (an offset into the text), or clears the underline. */
  showError(position: number | null): void {
    this.view.dispatch({ effects: setError.of(position === null ? null : errorRange(this.text, position)) });
  }

  focus(position?: number): void {
    if (position !== undefined) this.view.dispatch({ selection: { anchor: Math.min(position, this.view.state.doc.length) }, scrollIntoView: true });
    this.view.focus();
  }

  destroy(): void {
    this.view.destroy();
  }
}

/** A read-only JSON viewer with line numbers (the documents' JSON mode). */
export function createJsonViewer(parent: HTMLElement): { set(text: string): void; destroy(): void } {
  const extensions: Extension[] = [
    EditorState.readOnly.of(true),
    EditorView.editable.of(true),
    lineNumbers(),
    drawSelection(),
    bracketMatching(),
    jsonMode(),
    syntaxHighlighting(highlightStyle),
    keymap.of(defaultKeymap),
    sharedTheme,
    EditorView.theme({ "&": { height: "100%" }, ".cm-scroller": { overflow: "auto" } }),
    EditorView.contentAttributes.of({ "aria-label": "Documents as JSON" }),
  ];
  const view = new EditorView({ parent, doc: "", extensions });
  return {
    set(text: string) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
    },
    destroy: () => view.destroy(),
  };
}
