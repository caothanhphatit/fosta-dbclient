// Completion for one stage of the aggregation builder: the stage document in mongosh syntax. Keys and `"$field"` paths
// reuse the console's analysis (`mongoCompletions`) by placing the stage where it sits in a shell command
// (`db.getCollection("orders").aggregate([<stage>])`), so stage operators, query operators in `$match`, accumulators in
// `$group`, expression operators and fields are offered where they fit. The fields are the ones that reach this stage
// (sampled from the previous stage's preview output), not the collection's. On top of that: collection names for
// `$lookup.from`, `$graphLookup.from`, `$unionWith` (and its `coll`), `$out` and `$merge.into`; input fields for
// `localField` / `connectFromField`; the joined collection's fields for `foreignField` / `connectToField`.
import type { Completion, CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import type { CompletionSchema } from "../types";
import { mongoCompletions } from "./completion";

export interface StageCompletionOptions {
  collection: string;
  /** Collections and their sampled fields (`completion_schema`). */
  schema(): CompletionSchema;
  /** The fields reaching this stage (the previous stage's output); empty: the collection's own fields. */
  fields(): readonly string[];
}

/** A schema whose `collection` has exactly `fields` (the input of a stage), other collections as they are. */
export function stageSchema(schema: CompletionSchema, collection: string, fields: readonly string[]): CompletionSchema {
  if (fields.length === 0) return schema;
  const columns = fields.map((name) => ({ name, type: "", pk: name === "_id" }));
  return { ...schema, tables: [{ name: collection, kind: "collection", columns }, ...schema.tables.filter((table) => table.name !== collection)] };
}

const COLLECTION_VALUE = /(?:\$unionWith|\$out|\bcoll|\binto|\bfrom)["']?\s*:\s*(["'])([^"'\\]*)$/;
const INPUT_FIELD_VALUE = /\b(?:localField|connectFromField)["']?\s*:\s*(["'])([\w.]*)$/;
const FOREIGN_FIELD_VALUE = /\b(?:foreignField|connectToField)["']?\s*:\s*(["'])([\w.]*)$/;
const FROM_NAME = /\bfrom["']?\s*:\s*["']([^"']+)["']/;

function fieldsOf(schema: CompletionSchema, collection: string): string[] {
  const table = schema.tables.find((item) => item.name === collection);
  return table ? table.columns.map((column) => column.name) : [];
}

function quotedOptions(labels: readonly string[], detail: string, type: string, quote: string, nextChar: string): Completion[] {
  const close = nextChar === quote ? "" : quote;
  return labels.map((label) => ({ label, apply: label + close, type, detail }));
}

/** Completions at `pos` of a stage's `text`; synchronous, for tests and the source. */
export function stageCompletions(
  collection: string,
  schema: CompletionSchema,
  fields: readonly string[],
  text: string,
  pos: number,
  explicit: boolean,
): CompletionResult | null {
  const before = text.slice(0, pos);
  const nextChar = text[pos] ?? "";
  const names = (match: RegExpExecArray, labels: string[], detail: string, type: string): CompletionResult | null =>
    labels.length === 0
      ? null
      : { from: pos - match[2].length, to: pos, options: quotedOptions(labels, detail, type, match[1], nextChar), validFor: /^[^"'\\]*$/ };
  const collectionValue = COLLECTION_VALUE.exec(before);
  if (collectionValue)
    return names(
      collectionValue,
      schema.tables.map((table) => table.name),
      "collection",
      "class",
    );
  const inputField = INPUT_FIELD_VALUE.exec(before);
  if (inputField) return names(inputField, [...(fields.length > 0 ? fields : fieldsOf(schema, collection))], "field", "property");
  const foreignField = FOREIGN_FIELD_VALUE.exec(before);
  if (foreignField) {
    const from = FROM_NAME.exec(text)?.[1] ?? "";
    return names(foreignField, from ? fieldsOf(schema, from) : [], `field of ${from}`, "property");
  }
  const frame = `db.getCollection(${JSON.stringify(collection)}).aggregate([`;
  const doc = `${frame}${text}])`;
  const result = mongoCompletions(stageSchema(schema, collection, fields), doc, pos + frame.length, explicit);
  if (!result) return null;
  return { ...result, from: result.from - frame.length, to: result.to === undefined ? undefined : result.to - frame.length };
}

/** The CodeMirror completion source of one stage editor. */
export function stageCompletionSource(options: StageCompletionOptions): CompletionSource {
  return (context: CompletionContext) =>
    stageCompletions(options.collection, options.schema(), options.fields(), context.state.doc.toString(), context.pos, context.explicit);
}
