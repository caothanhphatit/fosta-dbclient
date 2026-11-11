/**
 * The dev mock's `mongo.explain`, like src-tauri/src/engines/mongo/explain.rs: a plausible classic plan for a find or
 * a pipeline from the collection's mock indexes (an index whose first key the filter or sort names, else a collection
 * scan; a SORT stage unless the index gives the order; the other candidate indexes as rejected plans) with counters
 * from the mock documents, returned both raw (explainVersion 1 shapes: `queryPlanner` / `executionStats`, `stages` with
 * a `$cursor` for pipelines) and normalised (`plan`). `$out` / `$merge` are refused.
 */
import { mockError } from "../errors";

type Json = unknown;
type Doc = Record<string, Json>;

interface Node {
  stage: string;
  indexName?: string;
  keyPattern?: Doc;
  direction?: string;
  filter?: Doc;
  details?: Json;
  nReturned?: number;
  executionTimeMillisEstimate?: number;
  totalKeysExamined?: number;
  totalDocsExamined?: number;
  children: Node[];
}

export interface ExplainHost {
  /** The collection's indexes (`mongo.indexes` entries). */
  indexes(args: Doc): { name: string; key: Doc; options: Doc }[];
  /** Documents in the collection, and those matching a filter. */
  total(args: Doc): number;
  matched(args: Doc, filter: Doc | undefined): number;
}

const VERBOSITIES = ["queryPlanner", "executionStats", "allPlansExecution"];
const isDoc = (value: Json): value is Doc => typeof value === "object" && value !== null && !Array.isArray(value);

function parsed(value: Json, what: string): Json {
  if (typeof value !== "string") return value ?? undefined;
  if (!value.trim()) return undefined;
  try {
    return JSON.parse(value) as Json;
  } catch {
    throw mockError("ENGINE_ERROR", `${what} must be a JSON document`);
  }
}

function fieldsOf(filter: Doc | undefined): string[] {
  if (!filter) return [];
  const fields: string[] = [];
  for (const [key, value] of Object.entries(filter)) {
    if (key === "$and" && Array.isArray(value)) fields.push(...value.flatMap((item) => fieldsOf(isDoc(item) ? item : undefined)));
    else if (!key.startsWith("$")) fields.push(key);
  }
  return [...new Set(fields)];
}

/** The plan of a find: an index scan (with a fetch) or a collection scan, a sort when the index does not give the order, a limit. */
function findPlan(host: ExplainHost, args: Doc, filter: Doc | undefined, sort: Doc | undefined, limit: number | null): { winning: Node; rejected: Node[] } {
  const fields = fieldsOf(filter);
  const sortFields = Object.keys(sort ?? {});
  const total = host.total(args);
  const matched = host.matched(args, filter);
  const candidates = host
    .indexes(args)
    .filter((index) => index.options.hidden !== true && !isDoc(index.options.partialFilterExpression))
    .map((index) => {
      const keys = Object.keys(index.key);
      let prefix = 0;
      while (prefix < keys.length && fields.includes(keys[prefix])) prefix++;
      const sorted = sortFields.length > 0 && sortFields.every((field, i) => keys[prefix + i] === field);
      return { index, keys, prefix, sorted };
    })
    .filter((candidate) => candidate.prefix > 0 || (candidate.sorted && candidate.index.name !== "_id_") || (fields.length === 0 && candidate.sorted))
    .sort((a, b) => b.prefix - a.prefix || Number(b.sorted) - Number(a.sorted));
  const returned = limit ? Math.min(matched, limit) : matched;
  const build = (candidate: (typeof candidates)[number] | undefined): Node => {
    let node: Node;
    let ordered = sortFields.length === 0;
    if (candidate) {
      const all = fields.every((field) => candidate.keys.slice(0, candidate.prefix).includes(field));
      const keys = candidate.prefix === 0 ? total : all ? matched : Math.min(total, Math.max(matched, Math.round(total / 3)));
      const rest = filter ? Object.fromEntries(Object.entries(filter).filter(([key]) => !candidate.keys.slice(0, candidate.prefix).includes(key))) : {};
      const scan: Node = {
        stage: "IXSCAN",
        indexName: candidate.index.name,
        keyPattern: candidate.index.key,
        direction: "forward",
        nReturned: keys,
        executionTimeMillisEstimate: Math.round(keys / 4000),
        totalKeysExamined: keys,
        children: [],
      };
      node = { stage: "FETCH", nReturned: matched, executionTimeMillisEstimate: Math.round(keys / 2000), totalDocsExamined: keys, children: [scan] };
      if (Object.keys(rest).length) node.filter = rest;
      ordered ||= candidate.sorted;
    } else {
      node = {
        stage: "COLLSCAN",
        direction: "forward",
        nReturned: matched,
        executionTimeMillisEstimate: Math.round(total / 1500),
        totalDocsExamined: total,
        children: [],
      };
      if (filter && Object.keys(filter).length) node.filter = filter;
    }
    if (!ordered)
      node = {
        stage: "SORT",
        details: { sortPattern: sort, ...(limit ? { limitAmount: limit } : {}) },
        nReturned: returned,
        executionTimeMillisEstimate: 1,
        children: [node],
      };
    else if (limit && matched > limit)
      node = { stage: "LIMIT", details: { limitAmount: limit }, nReturned: returned, executionTimeMillisEstimate: 0, children: [node] };
    return node;
  };
  const winning = build(candidates[0]);
  // The trial run of a rejected plan stops early: its counters are a fraction of a full run.
  const rejected = candidates.slice(1, 3).map((candidate) => trial(build(candidate)));
  return { winning, rejected };
}

function trial(node: Node): Node {
  const cut = (n: number | undefined) => (n === undefined ? undefined : Math.min(n, 101));
  return {
    ...node,
    nReturned: cut(node.nReturned),
    totalKeysExamined: cut(node.totalKeysExamined),
    totalDocsExamined: cut(node.totalDocsExamined),
    children: node.children.map(trial),
  };
}

function withoutCounters(node: Node): Node {
  const rest: Node = { ...node, children: node.children.map(withoutCounters) };
  for (const key of ["nReturned", "executionTimeMillisEstimate", "totalKeysExamined", "totalDocsExamined"] as const) delete rest[key];
  return rest;
}

/** A normalised node as the server would give it (classic: `inputStage` / `inputStages`, `keysExamined`, `docsExamined`). */
function raw(node: Node): Doc {
  const out: Doc = { stage: node.stage };
  if (node.indexName) Object.assign(out, { keyPattern: node.keyPattern, indexName: node.indexName, isMultiKey: false, direction: node.direction });
  else if (node.direction) out.direction = node.direction;
  if (node.filter) out.filter = node.filter;
  if (isDoc(node.details)) Object.assign(out, node.details);
  if (node.nReturned !== undefined) out.nReturned = node.nReturned;
  if (node.executionTimeMillisEstimate !== undefined) out.executionTimeMillisEstimate = node.executionTimeMillisEstimate;
  if (node.totalKeysExamined !== undefined) out.keysExamined = node.totalKeysExamined;
  if (node.totalDocsExamined !== undefined) out.docsExamined = node.totalDocsExamined;
  if (node.children.length === 1) out.inputStage = raw(node.children[0]);
  else if (node.children.length > 1) out.inputStages = node.children.map(raw);
  return out;
}

function sum(node: Node, key: "totalKeysExamined" | "totalDocsExamined"): number {
  return (node[key] ?? 0) + node.children.reduce((total, child) => total + sum(child, key), 0);
}

function summary(root: Node, counted: boolean, returned: number | null): Doc {
  const stages: Node[] = [];
  const walk = (node: Node) => {
    stages.push(node);
    node.children.forEach(walk);
  };
  walk(root);
  const docs = counted ? sum(root, "totalDocsExamined") : null;
  return {
    nReturned: counted ? returned : null,
    keysExamined: counted ? sum(root, "totalKeysExamined") : null,
    docsExamined: docs,
    executionTimeMillis: counted ? Math.max(1, root.executionTimeMillisEstimate ?? 1) : null,
    isCollScan: stages.some((node) => node.stage === "COLLSCAN"),
    usedIndexes: [...new Set(stages.flatMap((node) => (node.indexName ? [node.indexName] : [])))],
    sortInMemory: stages.some((node) => node.stage === "SORT" || node.stage === "$sort"),
    fetchRatio: counted && docs !== null && returned !== null ? Math.round((docs / Math.max(1, returned)) * 100) / 100 : null,
  };
}

export function explainResult(host: ExplainHost, args: Doc, pipeline: Doc[]): Json {
  const verbosity = String(args.verbosity ?? "queryPlanner") || "queryPlanner";
  if (!VERBOSITIES.includes(verbosity))
    throw mockError("ENGINE_ERROR", `verbosity is "queryPlanner", "executionStats" or "allPlansExecution", not "${verbosity}"`);
  const counted = verbosity !== "queryPlanner";
  const kind = args.kind ?? "find";
  const collection = String(args.collection);
  const shell = `db.getCollection(${JSON.stringify(collection)}).explain(${JSON.stringify(verbosity)})`;
  if (kind === "find") {
    const filter = parsed(args.filter, "filter") as Doc | undefined;
    const sort = parsed(args.sort, "sort") as Doc | undefined;
    const limit = typeof args.limit === "number" && args.limit > 0 ? args.limit : null;
    const { winning, rejected } = findPlan(host, args, filter, sort && Object.keys(sort).length ? sort : undefined, limit);
    const shown = counted ? winning : withoutCounters(winning);
    const rejectedShown = verbosity === "allPlansExecution" ? rejected : rejected.map(withoutCounters);
    const explain: Doc = {
      explainVersion: "1",
      queryPlanner: {
        namespace: `${String(args.database)}.${collection}`,
        parsedQuery: filter ?? {},
        winningPlan: raw(withoutCounters(winning)),
        rejectedPlans: rejected.map((node) => raw(withoutCounters(node))),
      },
      command: { find: collection, filter: filter ?? {}, ...(sort ? { sort } : {}), ...(limit ? { limit } : {}), $db: args.database },
      serverInfo: { host: "mock", port: 27017, version: "8.0.4" },
      ok: 1,
    };
    if (counted)
      explain.executionStats = {
        executionSuccess: true,
        nReturned: winning.nReturned,
        executionTimeMillis: Math.max(1, winning.executionTimeMillisEstimate ?? 1),
        totalKeysExamined: sum(winning, "totalKeysExamined"),
        totalDocsExamined: sum(winning, "totalDocsExamined"),
        executionStages: raw(winning),
        ...(verbosity === "allPlansExecution" ? { allPlansExecution: [trial(winning), ...rejected].map((node) => ({ executionStages: raw(node) })) } : {}),
      };
    return {
      explain,
      plan: {
        shape: "find",
        explainVersion: "1",
        summary: summary(shown, counted, winning.nReturned ?? null),
        winningPlan: shown,
        rejectedPlans: rejectedShown.map((plan) => ({ shard: null, plan })),
      },
      verbosity,
      statement: `${shell}.find(${JSON.stringify(filter ?? {})}${sort ? `).sort(${JSON.stringify(sort)}` : ""})${limit ? `.limit(${limit})` : ""}`,
      elapsedMs: 4,
    };
  }
  const writes = pipeline.find((stage) => "$out" in stage || "$merge" in stage);
  if (writes) throw mockError("BLOCKED", `${Object.keys(writes)[0]} writes into a collection; explain only reads`);
  const matches: Doc[] = [];
  let sort: Doc | undefined;
  let rest = 0;
  for (const stage of pipeline) {
    if (isDoc(stage.$match) && !sort) matches.push(stage.$match);
    else if (isDoc(stage.$sort) && !sort) sort = stage.$sort;
    else break;
    rest++;
  }
  const filter = matches.length === 0 ? undefined : matches.length === 1 ? matches[0] : { $and: matches };
  const { winning, rejected } = findPlan(host, args, filter, sort, null);
  let root: Node = winning;
  let returned = winning.nReturned ?? 0;
  const stages: Doc[] = [];
  for (const stage of pipeline.slice(rest)) {
    const operator = Object.keys(stage)[0] ?? "?";
    if (operator === "$group") returned = Math.min(returned, 12);
    else if (operator === "$limit") returned = Math.min(returned, Number(stage.$limit) || returned);
    else if (operator === "$count") returned = Math.min(returned, 1);
    else if (operator === "$match") returned = Math.round(returned / 2);
    root = { stage: operator, details: stage[operator], nReturned: returned, executionTimeMillisEstimate: 1, children: [root] };
    stages.push({ ...stage, ...(counted ? { nReturned: returned, executionTimeMillisEstimate: 1 } : {}) });
  }
  const shown = counted ? root : withoutCounters(root);
  const cursor: Doc = {
    queryPlanner: {
      namespace: `${String(args.database)}.${collection}`,
      parsedQuery: filter ?? {},
      winningPlan: raw(withoutCounters(winning)),
      rejectedPlans: rejected.map((node) => raw(withoutCounters(node))),
    },
  };
  if (counted)
    cursor.executionStats = {
      executionSuccess: true,
      nReturned: winning.nReturned,
      executionTimeMillis: Math.max(1, winning.executionTimeMillisEstimate ?? 1),
      totalKeysExamined: sum(winning, "totalKeysExamined"),
      totalDocsExamined: sum(winning, "totalDocsExamined"),
      executionStages: raw(winning),
    };
  return {
    explain: { explainVersion: "1", stages: [{ $cursor: cursor }, ...stages], serverInfo: { host: "mock", port: 27017, version: "8.0.4" }, ok: 1 },
    plan: {
      shape: "aggregate",
      explainVersion: "1",
      summary: summary(shown, counted, returned),
      winningPlan: shown,
      rejectedPlans: (verbosity === "allPlansExecution" ? rejected : rejected.map(withoutCounters)).map((plan) => ({ shard: null, plan })),
    },
    verbosity,
    statement: `${shell}.aggregate(${JSON.stringify(pipeline)})`,
    elapsedMs: 5,
  };
}
