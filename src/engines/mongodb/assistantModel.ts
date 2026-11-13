/**
 * The assistant in the collection workspace, without the DOM: the `mongo-filter` and `mongo-pipeline` fences the model
 * answers with (parsed with the mongosh parser before anything is applied, the stage texts kept as written), the line
 * diff shown before a suggestion replaces what the user has, the prompts of the inline "Ask" buttons, and the sampled
 * documents handed to the assistant (cut per field).
 */
import { lineColumn, parseMongoShell } from "../../lang/mongo/shell";
import type { Json } from "./documents";
import { parseStage } from "./pipelineModel";

export const FILTER_FENCE = "mongo-filter";
export const PIPELINE_FENCE = "mongo-pipeline";

export type FenceParse<T> = ({ ok: true } & T) | { ok: false; error: string };

/** Where a parse error is, as the user reads it: "Line 2, column 5: …". */
function located(text: string, position: number, error: string): string {
  const { line, column } = lineColumn(text, position);
  return `Line ${line}, column ${column}: ${error}`;
}

/** The body of a fence, without a trailing semicolon. */
function body(code: string): string {
  return code.trim().replace(/;\s*$/, "").trim();
}

/** A `mongo-filter` block: one filter document in mongosh syntax. */
export function parseFilterFence(code: string): FenceParse<{ text: string }> {
  const text = body(code);
  if (!text) return { ok: false, error: "The block is empty: it should hold one filter document, e.g. { status: 'paid' }" };
  const parsed = parseMongoShell(text, { expect: "document" });
  if (!parsed.ok) return { ok: false, error: located(text, parsed.position, parsed.error) };
  return { ok: true, text };
}

/**
 * A `mongo-pipeline` block: an array of stages, or one stage document. Each stage keeps its text as written (comments,
 * `ObjectId(…)`), with its common indentation removed; every stage must parse as a stage.
 */
export function parsePipelineFence(code: string): FenceParse<{ stages: string[] }> {
  const text = body(code);
  if (!text) return { ok: false, error: "The block is empty: it should hold a pipeline, e.g. [ { $match: {} } ]" };
  let stages: string[];
  if (text.startsWith("[")) {
    const parsed = parseMongoShell(text, { expect: "array" });
    if (!parsed.ok) return { ok: false, error: located(text, parsed.position, parsed.error) };
    const values = JSON.parse(parsed.json) as Json[];
    const pieces = splitArray(text);
    // The split keeps the text as written; should it ever disagree with the parser, the parsed stages are used.
    stages = pieces && pieces.length === values.length ? pieces.map(dedent) : values.map((value) => JSON.stringify(value));
  } else {
    stages = [text];
  }
  if (stages.length === 0) return { ok: false, error: "The pipeline has no stages" };
  for (const [index, stage] of stages.entries()) {
    const parsed = parseStage(stage);
    if (!parsed.ok) return { ok: false, error: `Stage ${index + 1}: ${located(stage, parsed.position, parsed.error)}` };
  }
  return { ok: true, stages };
}

/**
 * What needs a second look in a pipeline from the assistant before it is applied: a stage that writes (`$out` drops
 * and replaces its target, `$merge` changes it), at any depth, or a `{ db, coll }` naming another database than
 * `database` (`$lookup` / `$unionWith` sources, `$merge.into`, `$out`). Empty when there is nothing to warn about. The
 * assistant's own tool refuses both; a fence is applied by the user, so it is flagged and asks first.
 */
export function pipelineRisks(stages: readonly string[], database: string | null): string[] {
  const risks = new Set<string>();
  const walk = (value: unknown): void => {
    if (Array.isArray(value)) {
      value.forEach(walk);
      return;
    }
    if (typeof value !== "object" || value === null) return;
    const object = value as Record<string, unknown>;
    if ("$out" in object) risks.add("$out drops the target collection and replaces it with the result");
    if ("$merge" in object) risks.add("$merge writes the result into a collection");
    const db = object.db;
    if (typeof db === "string" && "coll" in object && db !== database) risks.add(`it names the database ${db}, outside ${database ?? "the current one"}`);
    Object.values(object).forEach(walk);
  };
  for (const stage of stages) {
    const parsed = parseStage(stage);
    if (parsed.ok) walk(parsed.value);
  }
  return [...risks];
}

/**
 * The top-level elements of the array literal `text` as written, or null when `text` is not one array. Strings,
 * comments and regex literals are skipped over, so brackets and commas inside them do not count.
 */
export function splitArray(text: string): string[] | null {
  const source = text.trim();
  if (!source.startsWith("[")) return null;
  const elements: string[] = [];
  let depth = 0;
  let start = 1;
  let previous = "[";
  for (let index = 0; index < source.length; index += 1) {
    const char = source[index];
    const next = source[index + 1];
    if (char === '"' || char === "'" || char === "`") {
      index = skipString(source, index);
      previous = char;
      continue;
    }
    if (char === "/" && next === "/") {
      const end = source.indexOf("\n", index);
      index = end < 0 ? source.length : end;
      continue;
    }
    if (char === "/" && next === "*") {
      const end = source.indexOf("*/", index + 2);
      index = end < 0 ? source.length : end + 1;
      continue;
    }
    if (char === "/" && /[:,[({]/.test(previous)) {
      index = skipRegex(source, index);
      previous = "/";
      continue;
    }
    if (char === "[" || char === "{" || char === "(") depth += 1;
    else if (char === "]" || char === "}" || char === ")") {
      depth -= 1;
      if (depth === 0) {
        if (index !== source.length - 1) return null;
        const last = source.slice(start, index).trim();
        if (last) elements.push(last);
        return elements;
      }
    } else if (char === "," && depth === 1) {
      // A comment after the comma on the same line belongs to the element before it.
      const comment = /^[ \t]*\/\/[^\n]*/.exec(source.slice(index + 1))?.[0] ?? "";
      elements.push(`${source.slice(start, index).trim()}${comment ? ` ${comment.trim()}` : ""}`);
      start = index + 1 + comment.length;
      index += comment.length;
    }
    if (!/\s/.test(char)) previous = char;
  }
  return null;
}

function skipString(text: string, start: number): number {
  const quote = text[start];
  for (let index = start + 1; index < text.length; index += 1) {
    if (text[index] === "\\") index += 1;
    else if (text[index] === quote) return index;
  }
  return text.length;
}

function skipRegex(text: string, start: number): number {
  let inClass = false;
  for (let index = start + 1; index < text.length; index += 1) {
    const char = text[index];
    if (char === "\\") index += 1;
    else if (char === "[") inClass = true;
    else if (char === "]") inClass = false;
    else if (char === "/" && !inClass) {
      let end = index;
      while (/[a-z]/i.test(text[end + 1] ?? "")) end += 1;
      return end;
    }
  }
  return text.length;
}

/** Removes the indentation the lines after the first share (a stage cut out of an indented array). */
export function dedent(text: string): string {
  const lines = text.split("\n");
  if (lines.length < 2) return text.trim();
  const rest = lines.slice(1).filter((line) => line.trim());
  const indent = Math.min(...rest.map((line) => /^\s*/.exec(line)![0].length));
  return [lines[0].trim(), ...lines.slice(1).map((line) => line.slice(Math.min(indent, /^\s*/.exec(line)![0].length)))].join("\n").trimEnd();
}

// --- Diff ------------------------------------------------------------------------------------------------------------

export interface DiffLine {
  kind: "same" | "added" | "removed";
  text: string;
}

/** A line diff of `before` and `after` (longest common subsequence), removed lines before added ones. */
export function lineDiff(before: string, after: string): DiffLine[] {
  const a = before === "" ? [] : before.split("\n");
  const b = after === "" ? [] : after.split("\n");
  const lengths = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      lengths[i][j] = a[i] === b[j] ? lengths[i + 1][j + 1] + 1 : Math.max(lengths[i + 1][j], lengths[i][j + 1]);
    }
  }
  const lines: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) {
      lines.push({ kind: "same", text: a[i] });
      i += 1;
      j += 1;
    } else if (i < a.length && (j >= b.length || lengths[i + 1][j] >= lengths[i][j + 1])) {
      lines.push({ kind: "removed", text: a[i] });
      i += 1;
    } else {
      lines.push({ kind: "added", text: b[j] });
      j += 1;
    }
  }
  return lines;
}

/** Whether replacing `before` with `after` needs the user to look first: there is something of theirs that changes. */
export function needsReview(before: string, after: string): boolean {
  return before.trim() !== "" && before.trim() !== "{}" && before.trim() !== after.trim();
}

/** A pipeline as one text for the diff: one stage per block, separated by a blank line. */
export function pipelineText(stages: readonly string[]): string {
  return stages.map((stage, index) => `// ${index + 1}\n${stage.trim()}`).join("\n\n");
}

// --- Ask prompts -----------------------------------------------------------------------------------------------------

export type StageQuestion = "explain" | "next" | "fix";

/** The message of the query bar's "Ask": plain language to a filter for `collection`. */
export function filterQuestion(collection: string, request: string): string {
  return `Write a MongoDB filter for the collection ${collection}: ${request.trim()}\n\nAnswer with one \`${FILTER_FENCE}\` code block (mongosh syntax) and at most one sentence.`;
}

/** What the chat shows for the query bar's "Ask" (the instructions to the model stay in `filterQuestion`). */
export function filterQuestionShown(collection: string, request: string): string {
  return `Filter ${collection}: ${request.trim()}`;
}

/** The message of a stage card's "Ask" (`index` 0-based). */
export function stageQuestion(question: StageQuestion, index: number, operator: string, collection: string): string {
  const stage = `stage ${index + 1} (${operator}) of the aggregation pipeline on ${collection}`;
  switch (question) {
    case "explain":
      return `Explain what ${stage} does, briefly, with what goes in and what comes out.`;
    case "next":
      return `Suggest the next stage after ${stage}. Answer with one \`${PIPELINE_FENCE}\` code block holding just the new stage, and one sentence.`;
    case "fix":
      return `${stage[0].toUpperCase()}${stage.slice(1)} fails with the error in the context. Fix it: answer with one \`${PIPELINE_FENCE}\` code block holding just the fixed stage, and the cause in one sentence.`;
  }
}

/** What the chat shows for a stage card's "Ask" (the full question goes to the model). */
export function stageQuestionShown(question: StageQuestion, index: number, operator: string): string {
  const stage = `stage ${index + 1} (${operator})`;
  switch (question) {
    case "explain":
      return `Explain ${stage}`;
    case "next":
      return `Suggest the stage after ${stage}`;
    case "fix":
      return `Fix ${stage}`;
  }
}

// --- Sampled documents -----------------------------------------------------------------------------------------------

/** Documents, characters per value, array items and nesting kept for the assistant. */
export const SAMPLE_DOCUMENTS = 3;
const SAMPLE_VALUE_CHARS = 200;
const SAMPLE_ARRAY_ITEMS = 10;
const SAMPLE_DEPTH = 6;
const SAMPLE_CHARS = 4000;

function cutValue(value: Json, depth: number): Json {
  if (typeof value === "string") return value.length > SAMPLE_VALUE_CHARS ? `${value.slice(0, SAMPLE_VALUE_CHARS)}…` : value;
  if (Array.isArray(value)) {
    if (depth >= SAMPLE_DEPTH) return `[… ${value.length} item(s)]`;
    const kept = value.slice(0, SAMPLE_ARRAY_ITEMS).map((item) => cutValue(item, depth + 1));
    if (value.length > SAMPLE_ARRAY_ITEMS) kept.push(`… ${value.length - SAMPLE_ARRAY_ITEMS} more item(s)`);
    return kept;
  }
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value as Record<string, Json>);
    if (depth >= SAMPLE_DEPTH) return `{… ${entries.length} field(s)}`;
    return Object.fromEntries(entries.map(([key, inner]) => [key, cutValue(inner, depth + 1)]));
  }
  return value;
}

/** A few documents (each cut per field) as one line each, within a bound; null when there are none. */
export function sampledDocumentsText(documents: readonly Json[], label: string): string | null {
  if (documents.length === 0) return null;
  let text = `${label}:`;
  for (const document of documents.slice(0, SAMPLE_DOCUMENTS)) {
    const line = JSON.stringify(cutValue(document, 0));
    if (text.length + line.length + 1 > SAMPLE_CHARS) break;
    text += `\n${line}`;
  }
  return text;
}
