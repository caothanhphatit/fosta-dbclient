/**
 * A tab brought back from the last session, before it is shown: nothing connects until then. Shown for the first time,
 * it connects (through the tree) and turns into its real view in place (`open`); until then it keeps what was saved.
 */
import { confirmDialog } from "../core/dialogs";
import { h } from "../core/dom";
import type { TabContent } from "../core/tabs";
import { hasUnsavedText } from "../features/query/queryRules";
import type { SavedTab } from "./tabState";

export class RestoredTab implements TabContent {
  readonly element: HTMLElement;
  private readonly status = h("div", { class: "restored-status" });
  private readonly retry = h("button", { class: "button", type: "button" }, "Try again");
  private opening = false;

  /** `open` connects and replaces this tab with its view; it resolves false when that failed (connecting, say). */
  constructor(
    readonly saved: SavedTab,
    private readonly open: () => Promise<boolean>,
  ) {
    this.retry.hidden = true;
    this.retry.addEventListener("click", () => void this.materialize());
    this.element = h("div", { class: "view restored-tab" }, h("div", { class: "restored-box" }, this.status, this.retry));
  }

  onActivate(): void {
    void this.materialize();
  }

  /** Query text that was not run yet (saved with the tab): closing asks first, like an open query tab. */
  hasUnsavedText(): boolean {
    return this.saved.kind === "query" && hasUnsavedText(this.saved.text, this.saved.saved);
  }

  canClose(): boolean | Promise<boolean> {
    if (!this.hasUnsavedText()) return true;
    return confirmDialog("This tab's query text was edited and not run since; closing the tab discards it. Close anyway?", "Close tab");
  }

  private async materialize(): Promise<void> {
    if (this.opening) return;
    this.opening = true;
    this.retry.hidden = true;
    this.status.textContent = this.saved.kind === "logs" ? "Opening…" : "Connecting…";
    try {
      // The tab manager is still activating this tab: replace it once that is done.
      await Promise.resolve();
      if (await this.open()) return;
      this.status.textContent = "This tab could not be opened. Connect in the tree, then try again.";
      this.retry.hidden = false;
    } finally {
      this.opening = false;
    }
  }
}
