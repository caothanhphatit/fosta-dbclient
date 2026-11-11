/**
 * The "Update matching…" and "Delete matching…" actions of the query bar, without DOM: the update typed in mongosh
 * syntax (update operators or a pipeline) as Extended JSON, the arguments of `mongo.updatePreview`,
 * `mongo.updateMany` and `mongo.deleteMany` for the bar's current filter, and what the preview says.
 *
 * The confirmation is the gateway's (Write, or Destructive with an empty filter and for every delete); the dialog only
 * lets the user run the exact update it previewed.
 */
import { parseMongoShell } from "../../lang/mongo/shell";
import { diffDocuments, type DiffEntry } from "./docDiff";
import type { Doc, Json } from "./documents";
import type { Target } from "./queryModel";

export type ParsedUpdate = { ok: true; json: string } | { ok: false; error: string; position: number };

/** An update in mongosh syntax: `{ $set: { … } }` or a pipeline `[ { $set: … } ]`. Braces are needed. */
export function parseUpdate(text: string): ParsedUpdate {
  const trimmed = text.trim();
  if (!trimmed) return { ok: false, error: "Write an update, e.g. { $set: { status: 'archived' } }", position: 0 };
  const parsed = parseMongoShell(trimmed);
  if (!parsed.ok) return parsed;
  const value = JSON.parse(parsed.json) as Json;
  const offset = text.indexOf(trimmed);
  if (Array.isArray(value)) {
    if (value.length === 0) return { ok: false, error: "The pipeline is empty.", position: offset };
    if (!value.every((stage) => typeof stage === "object" && stage !== null && !Array.isArray(stage) && Object.keys(stage).length === 1)) {
      return { ok: false, error: "Each pipeline stage is one document, e.g. { $set: { … } }.", position: offset };
    }
    return { ok: true, json: parsed.json };
  }
  if (typeof value !== "object" || value === null) {
    return { ok: false, error: "An update is a document of operators, e.g. { $set: { status: 'archived' } }.", position: offset };
  }
  const keys = Object.keys(value as Doc);
  if (keys.length === 0) return { ok: false, error: "The update is empty.", position: offset };
  const plain = keys.find((key) => !key.startsWith("$"));
  if (plain !== undefined) {
    return {
      ok: false,
      error: `Use update operators: { $set: { ${plain}: … } }. A document without operators would replace whole documents.`,
      position: offset,
    };
  }
  return { ok: true, json: parsed.json };
}

/** Whether a filter (Extended JSON text, or null for none) matches every document. */
export function matchesEverything(filter: string | null): boolean {
  if (filter === null || !filter.trim()) return true;
  try {
    const value = JSON.parse(filter) as Json;
    return typeof value === "object" && value !== null && !Array.isArray(value) && Object.keys(value).length === 0;
  } catch {
    return true;
  }
}

export const PREVIEW_SAMPLE = 5;

export function previewArgs(target: Target, filter: string | null, update: string) {
  return { ...target, filter: filter ?? "", update, sample: PREVIEW_SAMPLE };
}

export function updateArgs(target: Target, filter: string | null, update: string) {
  return { ...target, filter: filter ?? "", update };
}

export function deleteArgs(target: Target, filter: string | null) {
  return { ...target, filter: filter ?? "" };
}

export interface PreviewResult {
  matched: number;
  sample: { before: Doc; after: Doc; changed: boolean }[];
  elapsedMs: number;
  statement: string;
}

export interface PreviewModel {
  matched: number;
  /** "12 documents match. The 5 shown first (by _id): 4 change." */
  headline: string;
  rows: { id: Json; diff: DiffEntry[] }[];
}

const plural = (n: number, one: string, many: string) => `${n.toLocaleString("en-US")} ${n === 1 ? one : many}`;

export function previewModel(result: PreviewResult): PreviewModel {
  const rows = result.sample.map((pair) => ({ id: pair.before._id, diff: diffDocuments(pair.before, pair.after) }));
  const changing = rows.filter((row) => row.diff.length > 0).length;
  const matchText = result.matched === 0 ? "No documents match the filter." : `${plural(result.matched, "document matches", "documents match")}.`;
  const sampleText =
    rows.length === 0
      ? ""
      : rows.length === 1
        ? ` The first one: ${changing === 1 ? "it changes" : "it does not change"}.`
        : ` The first ${rows.length} (by _id): ${changing === rows.length ? "all change" : `${changing} change`}.`;
  return { matched: result.matched, headline: `${matchText}${sampleText}`, rows };
}

/** The label of the button that runs the update or delete. */
export function runLabel(kind: "update" | "delete", matched: number | null): string {
  const verb = kind === "update" ? "Update" : "Delete";
  return matched === null ? `${verb} matching documents` : `${verb} ${plural(matched, "document", "documents")}`;
}
