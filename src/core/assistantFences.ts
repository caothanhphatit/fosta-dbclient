/**
 * Code fences an engine applies itself, besides Run / Insert / New tab: a MongoDB filter for a collection's query bar, a
 * pipeline for its Aggregations. The engine module supplies the buttons per fence tag (`EngineUiModule.assistantFences`,
 * found through `setFenceActionResolver`); the assistant panel shows them under a block with that tag, on a connection
 * of that engine. Also the request the assistant hands a workspace section (`AssistantSuggestion`): it is only
 * pre-filled, marked as suggested by the assistant, and never run.
 */
import type { ObjectRef } from "./contract";
import type { IconName } from "./dom";

/** Where a fence applies: the chat's connection and, when the active tab is an object's workspace, that object. */
export interface FenceTarget {
  connectionId: string;
  database: string | null;
  object: ObjectRef | null;
  /** Opens (or selects) the object's workspace on `section` and hands the section `request` (`SectionContent.receive`). */
  openWorkspace(object: ObjectRef, section: string, request: unknown): void;
}

export interface FenceAction {
  icon: IconName;
  label: string;
  title: string;
  /** Applies the block; resolves to a message to show under it instead (a parse error), or null. */
  run(): Promise<string | null> | string | null;
}

/** The buttons for one block of a fence tag. */
export type FenceActions = (code: string, target: FenceTarget) => FenceAction[];

type FenceResolver = (engineId: string, tag: string) => Promise<FenceActions | null>;

let resolver: FenceResolver = () => Promise.resolve(null);

export function setFenceActionResolver(next: FenceResolver): void {
  resolver = next;
}

/** The engine's own buttons for `tag`, or null when the engine has none for it. */
export async function fenceActionsOf(engineId: string | undefined, tag: string): Promise<FenceActions | null> {
  if (!engineId || !tag) return null;
  try {
    return await resolver(engineId, tag);
  } catch {
    return null;
  }
}

/** What the assistant suggests for a workspace section. */
export interface Suggestion {
  filter?: string | null;
  sort?: string | null;
  project?: string | null;
  /** Stage texts (mongosh syntax). */
  pipeline?: string[] | null;
  /** For a pipeline: replace the stages (asking first), insert after the active stage, or replace the active stage. */
  mode?: "replace" | "insert" | "replaceActive";
}

/** The request a section receives from the assistant (a fence's button, `open_collection_workspace`). */
export interface AssistantSuggestion {
  assistant: Suggestion;
}

/** The suggestion in a section request, or null when the request is something else. */
export function suggestionOf(request: unknown): Suggestion | null {
  if (typeof request !== "object" || request === null) return null;
  const suggestion = (request as Partial<AssistantSuggestion>).assistant;
  if (typeof suggestion !== "object" || suggestion === null) return null;
  const text = (value: unknown) => (typeof value === "string" ? value : null);
  const pipeline = Array.isArray(suggestion.pipeline) ? suggestion.pipeline.filter((stage): stage is string => typeof stage === "string") : null;
  const mode = suggestion.mode === "insert" || suggestion.mode === "replaceActive" ? suggestion.mode : "replace";
  return { filter: text(suggestion.filter), sort: text(suggestion.sort), project: text(suggestion.project), pipeline, mode };
}
