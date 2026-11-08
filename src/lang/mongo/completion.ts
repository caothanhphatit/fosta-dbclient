// MongoDB completion for both query styles the console accepts: shell syntax (`db.orders.find({ total: { $gt: 5 } })`,
// `db.getCollection("x").aggregate([...])`) and JSON database commands (`{ "find": "orders", "filter": {...} }`).
// It knows where the cursor is (collection, method, stage, query, update, expression, accumulator...) and offers
// collection names, methods, operators, stages and the collection's fields from the completion schema.
import type { Completion, CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import type { CompletionSchema, TextRange } from "../types";
import {
  ACCUMULATORS,
  COLLECTION_COMMANDS,
  COLLECTION_METHODS,
  COMMAND_OPTIONS,
  COMMANDS,
  CURSOR_METHODS,
  DB_METHODS,
  EXPRESSION_OPERATORS,
  LOGICAL_OPERATORS,
  QUERY_OPERATORS,
  STAGE_OPTIONS,
  STAGES,
  UPDATE_OPERATORS,
  type Term,
} from "./vocabulary";

/** Where a key or value is being typed. */
export type MongoRole =
  | "command"
  | "stage"
  | "query"
  | "fieldQuery"
  | "update"
  | "updateFields"
  | "fields"
  | "document"
  | "group"
  | "accumulator"
  | "expression"
  | "facet"
  | "updateSpec"
  | "deleteSpec"
  | `options:${string}`
  | "unknown";

export type MongoContext =
  | { kind: "none" }
  | { kind: "collections"; from: number; to: number; prefix: string; quote: string | null }
  | { kind: "dbMember"; from: number; to: number; prefix: string }
  | { kind: "collectionMember"; from: number; to: number; prefix: string }
  | { kind: "cursorMember"; from: number; to: number; prefix: string }
  | {
      kind: "key";
      role: MongoRole;
      from: number;
      to: number;
      prefix: string;
      quote: string | null;
      json: boolean;
      collection: string | null;
      firstKey: string | null;
    }
  | { kind: "fieldPath"; from: number; to: number; prefix: string; quote: string; collection: string | null };

interface Frame {
  open: "{" | "[" | "(";
  /** For a container that is the value of an object key: that key. */
  key: string | null;
  /** For "(": the called method; for others, inherited from an enclosing call argument. */
  callee: string | null;
  /** Argument / element index inside "(" and "[". */
  arg: number;
  expectKey: boolean;
  currentKey: string | null;
  firstKey: string | null;
  firstValue: string | null;
}

const IDENT = /[A-Za-z0-9_$]/;
const JS_IDENT = /^[A-Za-z_$][\w$]*$/;
const CURSOR_RETURNING = new Set(["find", "aggregate", "watch", ...CURSOR_METHODS.map((m) => m.name)]);

/** Splits a console script into statements: `;` or a line break at the top level ends one, unless the next line
 * continues it (starts with `.`, `)`, `]`, `}` or `,`). */
export function mongoStatements(text: string): TextRange[] {
  const out: TextRange[] = [];
  let depth = 0;
  let start = -1;
  let i = 0;
  const finish = (end: number) => {
    if (start >= 0) {
      let e = end;
      while (e > start && /\s/.test(text[e - 1])) e--;
      if (e > start) out.push({ from: start, to: e });
    }
    start = -1;
  };
  while (i < text.length) {
    const c = text[i];
    if (c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") i++;
      continue;
    }
    if (c === "/" && text[i + 1] === "*") {
      const end = text.indexOf("*/", i + 2);
      i = end < 0 ? text.length : end + 2;
      continue;
    }
    if (!/\s/.test(c) && start < 0) start = i;
    if (c === '"' || c === "'" || c === "`") {
      i = skipString(text, i);
      continue;
    }
    if ("{[(".includes(c)) depth++;
    else if ("}])".includes(c)) depth = Math.max(0, depth - 1);
    else if (depth === 0 && c === ";") {
      finish(i);
      i++;
      continue;
    } else if (depth === 0 && c === "\n" && start >= 0) {
      let j = i;
      while (j < text.length && /\s/.test(text[j])) j++;
      if (j >= text.length || !".)]},".includes(text[j])) {
        finish(i);
      }
    }
    i++;
  }
  finish(text.length);
  return out;
}

function skipString(text: string, i: number): number {
  const q = text[i];
  let j = i + 1;
  while (j < text.length && text[j] !== q) {
    if (text[j] === "\\") j++;
    else if (text[j] === "\n" && q !== "`") return j;
    j++;
  }
  return Math.min(j + 1, text.length);
}

/** `LanguageSupport.statementAt` for the Mongo console. */
export function mongoStatementAt(text: string, offset: number): TextRange {
  const ranges = mongoStatements(text);
  const inside = ranges.find((r) => r.from <= offset && offset <= r.to);
  if (inside) return inside;
  let prev: TextRange | null = null;
  for (const r of ranges) if (r.to <= offset) prev = r;
  return prev ?? ranges.find((r) => r.from > offset) ?? { from: offset, to: offset };
}

/** Analyzes the cursor position of a Mongo console document. */
export function analyzeMongo(doc: string, pos: number): MongoContext {
  const stmt = mongoStatements(doc).find((r) => r.from <= pos && pos <= r.to + 1) ?? { from: pos, to: pos };
  const base = stmt.from <= pos ? stmt.from : pos;
  const text = doc.slice(base, pos);
  const json = /^\s*\{/.test(doc.slice(base));
  const stack: Frame[] = [];
  let lastAtom: { text: string; string: boolean } | null = null;
  let inString: { quote: string; start: number } | null = null;
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === "/" && text[i + 1] === "/") {
      const nl = text.indexOf("\n", i);
      if (nl < 0) return { kind: "none" };
      i = nl;
      continue;
    }
    if (c === "/" && text[i + 1] === "*") {
      const end = text.indexOf("*/", i + 2);
      if (end < 0) return { kind: "none" };
      i = end + 2;
      continue;
    }
    if (c === '"' || c === "'" || c === "`") {
      const end = skipString(text, i);
      const closed = end <= text.length && text[end - 1] === c && end - 1 > i;
      if (!closed) {
        inString = { quote: c, start: i };
        break;
      }
      const value = text.slice(i + 1, end - 1);
      atomValue(stack, value, true);
      lastAtom = { text: value, string: true };
      i = end;
      continue;
    }
    if (IDENT.test(c)) {
      let j = i;
      while (j < text.length && IDENT.test(text[j])) j++;
      const word = text.slice(i, j);
      atomValue(stack, word, false);
      lastAtom = { text: word, string: false };
      i = j;
      continue;
    }
    const top = stack[stack.length - 1];
    if (c === "{" || c === "[" || c === "(") {
      const callee = c === "(" ? calleeBefore(text, i) : null;
      const key = top && top.open === "{" ? top.currentKey : null;
      stack.push({ open: c, key, callee, arg: 0, expectKey: c === "{", currentKey: null, firstKey: null, firstValue: null });
      lastAtom = null;
    } else if (c === "}" || c === "]" || c === ")") {
      stack.pop();
      lastAtom = null;
    } else if (c === ":" && top?.open === "{") {
      if (lastAtom) {
        top.currentKey = lastAtom.text;
        if (top.firstKey === null) top.firstKey = lastAtom.text;
      }
      top.expectKey = false;
      lastAtom = null;
    } else if (c === ",") {
      if (top?.open === "{") {
        top.expectKey = true;
        top.currentKey = null;
      } else if (top) top.arg++;
      lastAtom = null;
    }
    i++;
  }

  const collection = collectionOf(doc.slice(base, Math.max(stmt.to, pos)), stack);
  const top = stack[stack.length - 1];

  if (inString) {
    const from = base + inString.start + 1;
    const prefix = text.slice(inString.start + 1);
    const quote = inString.quote;
    // db.getCollection("|")
    if (top?.open === "(" && top.callee === "getCollection") return { kind: "collections", from, to: pos, prefix, quote };
    if (top?.open === "{" && top.expectKey) {
      return { kind: "key", role: roleOf(stack, stack.length - 1, json), from, to: pos, prefix, quote, json, collection, firstKey: top.firstKey };
    }
    if (top?.open === "{" && top.currentKey) {
      // { "find": "|" } and { $lookup: { from: "|" } }
      if (
        (stack.length === 1 && json && top.currentKey === top.firstKey && COLLECTION_COMMANDS.has(top.currentKey)) ||
        top.currentKey === "from" ||
        top.currentKey === "coll"
      ) {
        return { kind: "collections", from, to: pos, prefix, quote };
      }
    }
    if (prefix.startsWith("$")) return { kind: "fieldPath", from: from + 1, to: pos, prefix: prefix.slice(1), quote, collection };
    return { kind: "none" };
  }

  // The word being typed.
  let k = text.length;
  while (k > 0 && IDENT.test(text[k - 1])) k--;
  const prefix = text.slice(k);
  const from = base + k;
  const before = text.slice(0, k).replace(/\s+$/, "");
  if (before.endsWith(".")) {
    const target = memberTarget(before.slice(0, -1));
    if (target === "db") return { kind: "dbMember", from, to: pos, prefix };
    if (target === "collection") return { kind: "collectionMember", from, to: pos, prefix };
    if (target === "cursor") return { kind: "cursorMember", from, to: pos, prefix };
    return { kind: "none" };
  }
  if (top?.open === "{" && top.expectKey) {
    return { kind: "key", role: roleOf(stack, stack.length - 1, json), from, to: pos, prefix, quote: null, json, collection, firstKey: top.firstKey };
  }
  return { kind: "none" };
}

/** Records the value of a key (for JSON commands: the collection named by the command). */
function atomValue(stack: Frame[], value: string, isString: boolean): void {
  const top = stack[stack.length - 1];
  if (top?.open === "{" && !top.expectKey && top.currentKey === top.firstKey && top.firstValue === null && isString) top.firstValue = value;
}

/** The method name right before `(` at `i` (`db.orders.find(` -> `find`). */
function calleeBefore(text: string, i: number): string | null {
  let j = i;
  while (j > 0 && /\s/.test(text[j - 1])) j--;
  let k = j;
  while (k > 0 && IDENT.test(text[k - 1])) k--;
  return k < j ? text.slice(k, j) : null;
}

/** What `x` is in `x.`: the database, a collection, or a cursor. */
function memberTarget(expr: string): "db" | "collection" | "cursor" | null {
  const e = expr.replace(/\s+$/, "");
  if (/(^|[^\w$.])db$/.test(e)) return "db";
  if (e.endsWith(")")) {
    // Find the call's method name.
    let depth = 0;
    let j = e.length - 1;
    for (; j >= 0; j--) {
      if (e[j] === ")") depth++;
      else if (e[j] === "(") {
        depth--;
        if (depth === 0) break;
      }
    }
    const method = calleeBefore(e, j);
    if (method === "getCollection") return "collection";
    if (method && CURSOR_RETURNING.has(method)) return "cursor";
    return null;
  }
  if (/(^|[^\w$.])db\.[A-Za-z_$][\w$-]*$/.test(e)) {
    const name = e.slice(e.lastIndexOf(".") + 1);
    return DB_METHODS.some((m) => m.name === name) ? null : "collection";
  }
  return null;
}

/** The collection a statement works on. */
function collectionOf(stmt: string, stack: Frame[]): string | null {
  const byName = /db\.getCollection\(\s*(["'])((?:\\.|(?!\1).)*)\1\s*\)/.exec(stmt);
  if (byName) return byName[2];
  const dotted = /(?:^|[^\w$.])db\.([A-Za-z_$][\w$]*)\s*\./.exec(stmt);
  if (dotted && !DB_METHODS.some((m) => m.name === dotted[1])) return dotted[1];
  const root = stack[0];
  if (root?.open === "{" && root.firstKey && COLLECTION_COMMANDS.has(root.firstKey) && root.firstValue) return root.firstValue;
  const json = /^\s*\{\s*["']?(\w+)["']?\s*:\s*["']([^"']+)["']/.exec(stmt);
  if (json && COLLECTION_COMMANDS.has(json[1])) return json[2];
  return null;
}

const QUERY_METHODS = new Set([
  "find",
  "findOne",
  "countDocuments",
  "count",
  "deleteOne",
  "deleteMany",
  "findOneAndDelete",
  "updateOne",
  "updateMany",
  "replaceOne",
  "findOneAndUpdate",
  "findOneAndReplace",
]);

function argRole(callee: string | null, arg: number, json: boolean): MongoRole {
  if (!callee) return json ? "command" : "unknown";
  if (callee === "runCommand" || callee === "adminCommand") return "command";
  if (QUERY_METHODS.has(callee)) {
    if (arg === 0) return "query";
    if (callee === "find" || callee === "findOne") return arg === 1 ? "fields" : "unknown";
    if (callee.startsWith("update") || callee === "findOneAndUpdate") return arg === 1 ? "update" : "unknown";
    if (callee.startsWith("replace") || callee === "findOneAndReplace") return arg === 1 ? "document" : "unknown";
    return "unknown";
  }
  if (callee === "distinct") return arg === 1 ? "query" : "unknown";
  if (callee === "sort" || callee === "project" || callee === "projection" || callee === "createIndex" || callee === "hint") return "fields";
  if (callee === "insertOne") return "document";
  if (callee === "match") return "query";
  return "unknown";
}

/** The role of the object at `stack[i]` (which must be a "{"). */
export function roleOf(stack: Frame[], i: number, json: boolean): MongoRole {
  const frame = stack[i];
  const parent = stack[i - 1];
  if (!parent) return json ? "command" : "unknown";
  if (parent.open === "(") return argRole(parent.callee, parent.arg, json);
  if (parent.open === "[") return arrayElementRole(stack, i - 1, json);
  return valueRole(roleOf(stack, i - 1, json), frame.key ?? "", stack[i - 1]);
}

function arrayElementRole(stack: Frame[], j: number, json: boolean): MongoRole {
  const arr = stack[j];
  const parent = stack[j - 1];
  if (!parent) return "unknown";
  if (parent.open === "(") {
    if (parent.callee === "aggregate" && parent.arg === 0) return "stage";
    if (parent.callee === "insertMany") return "document";
    if (parent.callee === "watch") return "stage";
    return "unknown";
  }
  if (parent.open === "{") {
    const key = arr.key ?? "";
    const owner = roleOf(stack, j - 1, json);
    if (key === "pipeline" || owner === "facet") return "stage";
    if (["$and", "$or", "$nor"].includes(key)) return owner === "expression" ? "expression" : "query";
    if (key === "updates") return "updateSpec";
    if (key === "deletes") return "deleteSpec";
    if (key === "documents") return "document";
    if (key === "indexes") return `options:indexes`;
  }
  return "unknown";
}

function valueRole(owner: MongoRole, key: string, ownerFrame: Frame): MongoRole {
  switch (owner) {
    case "command":
      if (["filter", "query", "q"].includes(key)) return "query";
      if (["update", "u"].includes(key)) return "update";
      if (["sort", "projection", "fields", "hint"].includes(key)) return "fields";
      return "unknown";
    case "stage":
      if (key === "$match") return "query";
      if (["$project", "$addFields", "$set"].includes(key)) return "expression";
      if (key === "$group") return "group";
      if (key === "$sort") return "fields";
      if (key === "$facet") return "facet";
      if (STAGE_OPTIONS[key]) return `options:${key}`;
      return "expression";
    case "query":
      if (key === "$expr") return "expression";
      if (key.startsWith("$")) return key === "$not" ? "fieldQuery" : "unknown";
      return "fieldQuery";
    case "fieldQuery":
      if (key === "$elemMatch") return "query";
      if (key === "$not") return "fieldQuery";
      return "unknown";
    case "update":
      return key.startsWith("$") ? "updateFields" : "unknown";
    case "group":
      return key === "_id" ? "expression" : "accumulator";
    case "accumulator":
    case "expression":
      return "expression";
    case "updateSpec":
      return key === "q" ? "query" : key === "u" ? "update" : "unknown";
    case "deleteSpec":
      return key === "q" ? "query" : "unknown";
    default:
      if (owner.startsWith("options:")) {
        if (key === "pipeline") return "unknown"; // an array: handled by arrayElementRole
        if (key === "output") return ownerFrame.key === "$setWindowFields" ? "accumulator" : "group";
        if (["restrictSearchWithMatch", "query"].includes(key)) return "query";
        if (["newRoot", "groupBy", "startWith"].includes(key)) return "expression";
        if (key === "sortBy") return "fields";
      }
      return "unknown";
  }
}

const termOptions = (list: Term[], type: string, boost = 0): Completion[] => list.map((t) => ({ label: t.name, info: t.info, type, boost }));

function fieldsOf(schema: CompletionSchema, collection: string | null): string[] {
  if (!collection) return [];
  const table = schema.tables.find((t) => t.name === collection) ?? schema.tables.find((t) => t.name.toLowerCase() === collection.toLowerCase());
  return table ? table.columns.map((c) => c.name) : [];
}

function collectionNames(schema: CompletionSchema): string[] {
  return schema.tables.map((t) => t.name);
}

/** Completions for `pos` in `doc` (exported for tests). */
export function mongoCompletions(schema: CompletionSchema, doc: string, pos: number, explicit = false): CompletionResult | null {
  const ctx = analyzeMongo(doc, pos);
  if (ctx.kind === "none") return null;
  const nextChar = doc[pos] ?? "";
  switch (ctx.kind) {
    case "collections": {
      const close = ctx.quote && nextChar !== ctx.quote ? ctx.quote : "";
      const options = collectionNames(schema).map((n) => ({ label: n, apply: n + close, type: "class", detail: "collection" }));
      return options.length ? { from: ctx.from, to: ctx.to, options, validFor: /^[^"'`]*$/ } : null;
    }
    case "dbMember": {
      const options: Completion[] = [
        ...collectionNames(schema).map((n) => ({
          label: n,
          apply: JS_IDENT.test(n) ? n : `getCollection(${JSON.stringify(n)})`,
          type: "class",
          detail: "collection",
          boost: 1,
        })),
        ...termOptions(DB_METHODS, "method"),
      ];
      return { from: ctx.from, to: ctx.to, options, validFor: /^[\w$]*$/ };
    }
    case "collectionMember":
      return { from: ctx.from, to: ctx.to, options: termOptions(COLLECTION_METHODS, "method"), validFor: /^[\w$]*$/ };
    case "cursorMember":
      return { from: ctx.from, to: ctx.to, options: termOptions(CURSOR_METHODS, "method"), validFor: /^[\w$]*$/ };
    case "fieldPath": {
      const close = nextChar !== ctx.quote ? ctx.quote : "";
      const options = fieldsOf(schema, ctx.collection).map((f) => ({ label: f, apply: f + close, type: "property", detail: "field path" }));
      return options.length ? { from: ctx.from, to: ctx.to, options, validFor: /^[\w.$]*$/ } : null;
    }
    case "key": {
      if (!explicit && ctx.prefix === "" && !ctx.quote) return null;
      const keys = keyOptions(schema, ctx);
      if (!keys.length) return null;
      const options = keys.map((o) => ({ ...o, apply: keyText(o.label, ctx, nextChar) }));
      return { from: ctx.from, to: ctx.to, options, validFor: /^[\w.$]*$/ };
    }
  }
  return null;
}

function keyText(name: string, ctx: Extract<MongoContext, { kind: "key" }>, nextChar: string): string {
  if (ctx.quote) return name + (nextChar === ctx.quote ? "" : ctx.quote);
  if (ctx.json || !JS_IDENT.test(name)) return JSON.stringify(name);
  return name;
}

function keyOptions(schema: CompletionSchema, ctx: Extract<MongoContext, { kind: "key" }>): Completion[] {
  const fields = fieldsOf(schema, ctx.collection).map((f) => ({ label: f, type: "property", detail: "field", boost: 1 }));
  const role = ctx.role;
  switch (role) {
    case "command":
      if (!ctx.firstKey) return termOptions(COMMANDS, "keyword");
      return (COMMAND_OPTIONS[ctx.firstKey] ?? []).map((k) => ({ label: k, type: "property", detail: `${ctx.firstKey} option` }));
    case "stage":
      return termOptions(STAGES, "keyword");
    case "query":
      return [...fields, ...termOptions(LOGICAL_OPERATORS, "keyword")];
    case "fieldQuery":
      return termOptions(QUERY_OPERATORS, "keyword");
    case "update":
      return termOptions(UPDATE_OPERATORS, "keyword");
    case "updateFields":
    case "fields":
    case "document":
      return fields;
    case "group":
      return [{ label: "_id", type: "property", detail: "group key", boost: 2 }];
    case "accumulator":
      return termOptions(ACCUMULATORS, "function");
    case "expression":
      return [...termOptions(EXPRESSION_OPERATORS, "function"), ...fields];
    case "updateSpec":
      return ["q", "u", "upsert", "multi", "arrayFilters"].map((k) => ({ label: k, type: "property" }));
    case "deleteSpec":
      return ["q", "limit", "collation"].map((k) => ({ label: k, type: "property" }));
    default:
      if (role.startsWith("options:")) {
        const stage = role.slice("options:".length);
        if (stage === "indexes")
          return ["key", "name", "unique", "sparse", "expireAfterSeconds", "partialFilterExpression"].map((k) => ({ label: k, type: "property" }));
        return (STAGE_OPTIONS[stage] ?? []).map((k) => ({ label: k, type: "property", detail: `${stage} option` }));
      }
      return [];
  }
}

/** A CodeMirror completion source for the Mongo console over a schema provider (collections and their fields). */
export function mongoCompletionSource(schema: () => CompletionSchema): CompletionSource {
  return (context: CompletionContext) => mongoCompletions(schema(), context.state.doc.toString(), context.pos, context.explicit);
}
