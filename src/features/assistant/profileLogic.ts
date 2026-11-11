/**
 * Pure helpers of "Learn this database" and "What the assistant knows": the settings that allow learning and value
 * sampling (mirrors of `commands/profile.rs::learning_allowed` and `gateway/assistant.rs::reads_allowed`), step labels,
 * object signatures and search. No DOM here, so the rules are tested on their own.
 */
import type { ColumnProfile, DatabaseProfile, ObjectProfile, ProfileNote, ProfileStep, ValueHint } from "../../core/profileTypes";
import type { AppConfig, AssistantQueries, Workspace } from "../../core/types";
import { connectionSafety, PRODUCTION_ICON } from "../safety/safety";

function workspaceOf(config: AppConfig | null, connectionId: string): Workspace | null {
  return config?.workspaces.find((workspace) => workspace.connections.some((profile) => profile.id === connectionId)) ?? null;
}

function overrideOf(choice: AssistantQueries | undefined, global: boolean): boolean {
  if (choice === "allow") return true;
  if (choice === "deny") return false;
  return global;
}

/** Whether the assistant may learn databases on `connectionId`: the workspace's override wins, else the setting. */
export function learningAllowed(config: AppConfig | null, connectionId: string): boolean {
  return overrideOf(workspaceOf(config, connectionId)?.assistantLearn, config?.assistantLearn !== false);
}

/** Why learning is refused there, worded for a disabled button or a dialog. */
export function learningRefusal(config: AppConfig | null, connectionId: string): string {
  const workspace = workspaceOf(config, connectionId);
  return workspace?.assistantLearn === "deny"
    ? `Learning is turned off for the ${workspace.name} workspace (Edit workspace → Assistant may learn databases).`
    : "Learning is turned off (Settings → Assistant may learn databases).";
}

/**
 * Whether the assistant may read data on `connectionId` (value sampling): the workspace override wins; otherwise the
 * setting, which Read-only rules and Production workspaces turn off.
 */
export function readsAllowed(config: AppConfig | null, connectionId: string): boolean {
  const workspace = workspaceOf(config, connectionId);
  if (!workspace) return false;
  const choice = workspace.assistantQueries ?? "default";
  if (choice !== "default") return choice === "allow";
  const safety = connectionSafety(config?.workspaces ?? [], connectionId);
  return config?.assistantReadQueries !== false && safety?.preset !== "readOnly" && workspace.icon !== PRODUCTION_ICON;
}

/** The steps a build reports, in order, with what the progress list shows. */
export const LEARN_STEPS: { step: ProfileStep; label: string }[] = [
  { step: "listing", label: "Listing objects" },
  { step: "describing", label: "Reading columns, keys and indexes" },
  { step: "relations", label: "Finding relations" },
  { step: "values", label: "Sampling column values" },
  { step: "saving", label: "Saving" },
];

export const SUMMARY_STEP_LABEL = "Writing the summary and descriptions";

/** "1.2k", "34k", "980": a rounded token count. */
export function formatTokens(tokens: number): string {
  if (tokens < 1000) return String(Math.round(tokens));
  if (tokens < 10_000) return `${(tokens / 1000).toFixed(1).replace(/\.0$/, "")}k`;
  return `${Math.round(tokens / 1000)}k`;
}

/** "today 14:05", "12 Nov 2026 09:30". */
export function formatWhen(millis: number, now = Date.now()): string {
  const date = new Date(millis);
  const time = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const sameDay = new Date(now).toDateString() === date.toDateString();
  if (sameDay) return `today ${time}`;
  return `${date.toLocaleDateString([], { day: "numeric", month: "short", year: "numeric" })} ${time}`;
}

/** `schema.name`, or `name` without a schema. */
export function qualifiedName(object: Pick<ObjectProfile, "schema" | "name">): string {
  return object.schema ? `${object.schema}.${object.name}` : object.name;
}

/** "id int PK, email text?, status text": the columns in one line, nullable ones with `?`. */
export function columnSignature(columns: readonly ColumnProfile[], max = 12): string {
  const shown = columns.slice(0, max).map((column) => `${column.name} ${column.typeName}${column.primary ? " PK" : column.nullable ? "?" : ""}`);
  if (columns.length > max) shown.push(`+${columns.length - max} more`);
  return shown.join(", ");
}

/** "active | blocked | pending" or the format a column's values follow. */
export function hintText(hint: ValueHint): string {
  if (hint.values?.length) return hint.values.join(" | ");
  return hint.format ?? "";
}

/** The objects matching `query` (name, description, column names; every word must match), in profile order. */
export function searchObjects(profile: DatabaseProfile, query: string): ObjectProfile[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return profile.objects;
  return profile.objects.filter((object) => {
    const haystack = [qualifiedName(object), object.kind, object.description?.text ?? "", object.comment ?? "", ...object.columns.map((column) => column.name)]
      .join(" ")
      .toLowerCase();
    return words.every((word) => haystack.includes(word));
  });
}

/** The relations of one object (either end). */
export function relationsOf(profile: DatabaseProfile, object: ObjectProfile) {
  const name = qualifiedName(object);
  return profile.relations.filter((relation) => relation.from === name || relation.to === name);
}

/** A new note, or `note` with new text and scope (`object` null: about the whole database). */
export function editedNote(note: ProfileNote | null, text: string, object: string | null, now = Date.now()): ProfileNote {
  if (note) return { ...note, text, object, updatedAt: now };
  return { id: crypto.randomUUID(), text, object, createdAt: now, updatedAt: now };
}
