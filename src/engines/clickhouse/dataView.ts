/**
 * The table grid for ClickHouse: the shared `TableView`, plus what is specific here.
 *
 * - Edits are asynchronous mutations (`ALTER TABLE … UPDATE`, `DELETE FROM`) on MergeTree tables only, so the view says so in
 *   a notice above the grid.
 * - Every other table engine is read-only: the view is built without `editRows` (no cell editing, no Insert or Delete, no
 *   Import) and the notice says why. The tree marks those tables with a "read-only" badge; a table opened without a tree
 *   node is asked with `describe` and switches to read-only when the answer comes.
 */
import { h } from "../../core/dom";
import type { TabContent } from "../../core/tabs";
import type { ViewFactory } from "../../core/views";
import { TableView } from "../../features/table/tableView";
import { isReadOnlyNode, MUTATION_NOTICE, readOnlyReason } from "./model";

const NO_MUTATIONS =
  "This table's engine has no mutations. ClickHouse can change rows only in MergeTree tables; add rows with an INSERT statement in a query tab.";

function notice(reason: string | null): HTMLElement {
  if (reason) return h("div", { class: "ch-notice ch-readonly", role: "note" }, h("strong", {}, "Read-only. "), reason);
  return h(
    "details",
    { class: "ch-notice", role: "note" },
    h("summary", {}, "Edits are asynchronous mutations, not transactions."),
    h("p", {}, MUTATION_NOTICE),
  );
}

export const dataViewFactory: ViewFactory = (ctx): TabContent => {
  const readOnlyKnown = isReadOnlyNode(ctx.node);
  const capabilities = ctx.info.capabilities.filter((name) => !readOnlyKnown || name !== "editRows");
  // A copy of the connection's info: the grid reads it when it asks whether edits are on.
  const info = { ...ctx.info, capabilities };
  const view = new TableView({ ...ctx, info });
  if (ctx.object?.kind !== "table") return view;
  const holder = h("div", { class: "ch-notice-slot" });
  view.element.prepend(holder);
  const show = (reason: string | null): void => holder.replaceChildren(notice(reason));
  if (readOnlyKnown) {
    show(NO_MUTATIONS);
    return view;
  }
  show(null);
  if (!ctx.node) {
    // Opened without a tree node (from a query result, say): ask the table.
    void ctx.connection
      .describe(ctx.object)
      .then((description) => {
        const reason = readOnlyReason(description);
        if (!reason) return;
        const index = capabilities.indexOf("editRows");
        if (index >= 0) capabilities.splice(index, 1);
        show(reason);
      })
      .catch(() => undefined);
  }
  return view;
};
