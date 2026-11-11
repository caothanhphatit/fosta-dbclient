/**
 * The buttons under the assistant's `mongo-filter` and `mongo-pipeline` blocks. Each parses the block first (a parse
 * error shows under the block, nothing is applied) and then hands the collection workspace a suggestion
 * (`AssistantSuggestion`): the Documents section fills its query bar without running it; Aggregations replaces its
 * stages (asking first, with a diff), inserts them after the active stage, or replaces the active stage (with a diff).
 */
import type { AssistantSuggestion, FenceAction, FenceTarget, Suggestion } from "../../core/assistantFences";
import type { ObjectRef } from "../../core/contract";
import { parseFilterFence, parsePipelineFence } from "./assistantModel";

const NO_COLLECTION = "Open the collection first (double-click it in the tree): the block applies to the collection you are looking at.";

/** The collection (or view) of the active workspace tab, or null. */
function collectionOf(target: FenceTarget): ObjectRef | null {
  const object = target.object;
  return object && (object.kind === "collection" || object.kind === "view") ? object : null;
}

function deliver(target: FenceTarget, section: string, suggestion: Suggestion): string | null {
  const object = collectionOf(target);
  if (!object) return NO_COLLECTION;
  target.openWorkspace(object, section, { assistant: suggestion } satisfies AssistantSuggestion);
  return null;
}

export function filterFenceActions(code: string, target: FenceTarget): FenceAction[] {
  return [
    {
      icon: "filter",
      label: "Apply to query bar",
      title: "Put this filter in the collection's query bar; it runs when you press Find",
      run: () => {
        const parsed = parseFilterFence(code);
        return parsed.ok ? deliver(target, "documents", { filter: parsed.text }) : parsed.error;
      },
    },
  ];
}

export function pipelineFenceActions(code: string, target: FenceTarget): FenceAction[] {
  const apply = (mode: NonNullable<Suggestion["mode"]>) => () => {
    const parsed = parsePipelineFence(code);
    if (!parsed.ok) return parsed.error;
    if (mode === "replaceActive" && parsed.stages.length !== 1) return "Replace the active stage takes a block of one stage; this one has several.";
    return deliver(target, "aggregations", { pipeline: parsed.stages, mode });
  };
  return [
    { icon: "layers", label: "Open in Aggregations", title: "Open as the collection's pipeline (asks before replacing its stages)", run: apply("replace") },
    { icon: "plus", label: "Insert as stage(s)", title: "Insert after the active stage of the collection's pipeline", run: apply("insert") },
    { icon: "edit", label: "Replace stage", title: "Replace the active stage with this one (shows the change first)", run: apply("replaceActive") },
  ];
}
