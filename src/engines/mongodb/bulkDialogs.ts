/**
 * "Update matching…" and "Delete matching…" from the query bar: the bar's current filter, the documents it matches
 * (counted, the first few shown) and, for an update, a before/after preview (`mongo.updatePreview`, simulated on a
 * sample, operators it cannot simulate refused). The run button appears only for what was previewed, and the run goes
 * through the gateway, which asks per the workspace's rules (a delete and an update of every document are destructive:
 * the collection's name is typed in Guarded workspaces).
 */
import { openModal, toast } from "../../core/dialogs";
import { formatNumber, h } from "../../core/dom";
import { renderError } from "../../core/errorPanel";
import { readable } from "./bson";
import { diffDocuments } from "./docDiff";
import { diffList } from "./editDialogs";
import type { Doc } from "./documents";
import { deleteArgs, deleteHeadline, matchesEverything, parseUpdate, previewArgs, previewModel, PREVIEW_SAMPLE, runLabel, updateArgs } from "./bulkModel";
import type { MongoApi } from "./mongoApi";

export interface BulkContext {
  api: MongoApi;
  collection: string;
  /** The bar's filter as Extended JSON (null: none); `text` as typed, for the dialog. */
  filter: string | null;
  filterText: string;
  /** Runs the query again after a change. */
  done(): void;
}

function filterLine(ctx: BulkContext): HTMLElement {
  const everything = matchesEverything(ctx.filter);
  return h(
    "div",
    { class: `mb-filter${everything ? " mb-everything" : ""}` },
    h("span", { class: "mb-label" }, "Filter"),
    h("code", {}, ctx.filterText.trim() || "{}"),
    everything ? h("span", { class: "mb-warning" }, "No filter: every document in the collection") : null,
  );
}

export function openUpdateMatching(ctx: BulkContext): void {
  const area = h("textarea", {
    class: "input md-edit-area md-mono",
    spellcheck: "false",
    rows: 6,
    placeholder: "{ $set: { status: 'archived' } }",
  }) as HTMLTextAreaElement;
  const error = h("div", { class: "md-edit-error", role: "alert", hidden: true });
  const result = h("div", { class: "mb-result" });
  const previewButton = h("button", { class: "btn", type: "button" }, "Preview");
  const runButton = h("button", { class: "btn primary", type: "button", hidden: true }, runLabel("update", null));
  let previewed: { update: string; matched: number } | null = null;
  let close = () => undefined as void;

  const invalidate = () => {
    if (!previewed) return;
    previewed = null;
    runButton.hidden = true;
    result.classList.add("mb-stale");
  };
  area.addEventListener("input", invalidate);

  const preview = async () => {
    const parsed = parseUpdate(area.value);
    if (!parsed.ok) {
      error.textContent = parsed.error;
      error.hidden = false;
      area.focus();
      area.setSelectionRange(parsed.position, parsed.position);
      return;
    }
    error.hidden = true;
    previewButton.disabled = true;
    result.classList.remove("mb-stale");
    result.replaceChildren(h("div", { class: "mb-note" }, "Previewing…"));
    try {
      const answer = await ctx.api.updatePreview(previewArgs(ctx.api.target, ctx.filter, parsed.json));
      if (!answer) {
        result.replaceChildren(h("div", { class: "mb-note" }, "The preview was not run (the confirmation was declined)."));
        return;
      }
      const model = previewModel(answer);
      result.replaceChildren(
        h("div", { class: "mb-headline" }, model.headline),
        ...model.rows.map((row) =>
          h(
            "section",
            { class: "mb-sample" },
            h("div", { class: "mb-sample-id" }, `_id: ${readable(row.id).text}`),
            row.diff.length > 0 ? diffList(row.diff) : h("div", { class: "mb-note" }, "No change"),
          ),
        ),
        ...(model.matched > model.rows.length
          ? [h("div", { class: "mb-note" }, `The other ${formatNumber(model.matched - model.rows.length)} are not shown.`)]
          : []),
      );
      previewed = { update: parsed.json, matched: model.matched };
      runButton.textContent = runLabel("update", model.matched);
      runButton.hidden = model.matched === 0;
    } catch (failure) {
      result.replaceChildren(renderError(failure, { context: "Preview the update" }));
    } finally {
      previewButton.disabled = false;
    }
  };

  const run = async () => {
    if (!previewed) return;
    runButton.disabled = true;
    try {
      const answer = await ctx.api.updateMany(updateArgs(ctx.api.target, ctx.filter, previewed.update));
      if (!answer) return;
      toast(`Updated ${formatNumber(answer.modified)} of ${formatNumber(answer.matched)} matching documents in ${ctx.collection}`, "success");
      close();
      ctx.done();
    } catch (failure) {
      result.replaceChildren(renderError(failure, { context: `Update documents in ${ctx.collection}` }));
    } finally {
      runButton.disabled = false;
    }
  };
  previewButton.addEventListener("click", () => void preview());
  runButton.addEventListener("click", () => void run());
  area.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
      event.preventDefault();
      void preview();
    }
  });

  const body = h(
    "div",
    { class: "mb-dialog" },
    filterLine(ctx),
    h("label", { class: "mb-label" }, "Update (operators such as $set, $unset, $inc, or a pipeline)"),
    area,
    error,
    h("div", { class: "mb-actions" }, previewButton, runButton),
    result,
  );
  close = openModal(`Update matching documents · ${ctx.collection}`, body, [{ label: "Close", run: () => undefined }], { width: 760 });
  area.focus();
}

export function openDeleteMatching(ctx: BulkContext): void {
  const result = h("div", { class: "mb-result" }, h("div", { class: "mb-note" }, "Counting…"));
  const runButton = h("button", { class: "btn danger", type: "button", hidden: true }, runLabel("delete", null));
  let close = () => undefined as void;
  const body = h("div", { class: "mb-dialog" }, filterLine(ctx), result, h("div", { class: "mb-actions" }, runButton));
  close = openModal(`Delete matching documents · ${ctx.collection}`, body, [{ label: "Close", run: () => undefined }], { width: 720 });

  void (async () => {
    try {
      const target = ctx.api.target;
      const [counted, sample] = await Promise.all([
        ctx.api.count({ ...target, filter: ctx.filter ?? undefined, estimated: false }),
        ctx.api.find({ ...target, filter: ctx.filter ?? undefined, limit: PREVIEW_SAMPLE }),
      ]);
      if (!counted || !sample) {
        result.replaceChildren(h("div", { class: "mb-note" }, "Not read (the confirmation was declined)."));
        return;
      }
      const documents = sample.documents as Doc[];
      result.replaceChildren(
        h("div", { class: "mb-headline" }, deleteHeadline(counted.count)),
        ...documents.map((document) =>
          h(
            "section",
            { class: "mb-sample" },
            h("div", { class: "mb-sample-id" }, `_id: ${readable(document._id).text}`),
            diffList(diffDocuments(document, {}).slice(0, 8)),
          ),
        ),
        ...(counted.count > documents.length
          ? [h("div", { class: "mb-note" }, `The other ${formatNumber(counted.count - documents.length)} are not shown.`)]
          : []),
      );
      runButton.textContent = runLabel("delete", counted.count);
      runButton.hidden = counted.count === 0;
    } catch (failure) {
      result.replaceChildren(renderError(failure, { context: `Count documents in ${ctx.collection}` }));
    }
  })();

  runButton.addEventListener("click", () => {
    runButton.disabled = true;
    void (async () => {
      try {
        const answer = await ctx.api.deleteMany(deleteArgs(ctx.api.target, ctx.filter));
        if (!answer) return;
        toast(`Deleted ${formatNumber(answer.deleted)} documents from ${ctx.collection}`, "success");
        close();
        ctx.done();
      } catch (failure) {
        result.replaceChildren(renderError(failure, { context: `Delete documents in ${ctx.collection}` }));
      } finally {
        runButton.disabled = false;
      }
    })();
  });
}
