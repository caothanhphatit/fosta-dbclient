/** The dialog shown before an assistant's suggestion replaces what the user has: a line diff of the old and new text. */
import { openModal } from "../../core/dialogs";
import { h } from "../../core/dom";
import { lineDiff } from "./assistantModel";

const MARKS = { same: "  ", removed: "- ", added: "+ " } as const;

/** Resolves true when the user accepts the change. */
export function reviewChange(title: string, before: string, after: string, confirmLabel = "Replace"): Promise<boolean> {
  return new Promise((resolve) => {
    let accepted = false;
    const lines = lineDiff(before, after).map((line) => h("div", { class: `mq-diff-line mq-diff-${line.kind}` }, `${MARKS[line.kind]}${line.text}`));
    const body = h(
      "div",
      { class: "mq-review" },
      h("p", { class: "mq-review-note" }, "Suggested by the assistant. Nothing runs when you accept: you run it yourself."),
      h("pre", { class: "mq-diff", "aria-label": "Changes: removed lines start with -, added lines with +" }, ...lines),
    );
    openModal(
      title,
      body,
      [
        { label: "Cancel", run: () => undefined },
        { label: confirmLabel, primary: true, run: () => void (accepted = true) },
      ],
      { width: 620, onClose: () => resolve(accepted) },
    );
  });
}
