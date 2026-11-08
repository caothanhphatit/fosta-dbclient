// Cursor analysis for SQL completion: which statement the cursor is in, what is being typed, what kind of name fits
// there, and which tables are in scope (FROM/JOIN/UPDATE/INTO refs with aliases, subqueries with their output
// columns, CTEs), resolved against the completion schema.
import type { CompletionColumn, CompletionSchema, CompletionTable, LangDialect, ForeignRef } from "../types";
import { unquote } from "./dialect";
import { tokenize, isTrivia, type Token } from "./lexer";
import { splitStatements } from "./split";

export interface ScopeColumn {
  name: string;
  type?: string;
  pk?: boolean;
  fk?: ForeignRef;
}

export interface ScopeTable {
  kind: "table" | "cte" | "subquery";
  /** Table or CTE name; for a subquery, its alias. */
  name: string;
  schema?: string;
  alias?: string;
  columns: ScopeColumn[];
  /** The schema table the ref resolved to. */
  table?: CompletionTable;
  /** Nesting depth of the query the ref belongs to (0 = outermost). */
  depth: number;
  /** Offset of the ref in the document. */
  at: number;
}

export type ContextKind =
  /** Inside a string or comment: no completion. */
  | "none"
  /** After `x.`: members of x. */
  | "member"
  /** After FROM, INTO, UPDATE, TABLE...: a table name. */
  | "table"
  /** After JOIN: a table name, with FK-based join suggestions. */
  | "join"
  /** After ON (or AND/OR inside ON): a join condition. */
  | "joinCondition"
  /** An expression position: columns, functions, keywords. */
  | "column"
  /** Right after a table name or alias in FROM/JOIN: keywords (WHERE, JOIN, ON, AS...). */
  | "afterTable"
  /** Right after a value in an expression: keywords (FROM, AS, AND...). */
  | "afterValue"
  /** Nothing typed yet in the statement: statement keywords. */
  | "statementStart";

export interface SqlContext {
  kind: ContextKind;
  /** Replacement range for the completion. */
  from: number;
  to: number;
  /** The text typed so far (without an opening quote). */
  prefix: string;
  /** Opening quote when the cursor is inside a quoted name. */
  quote?: string;
  /** For `member`: the qualifier parts before the dot (`["s"]`, `["db", "schema"]`). */
  qualifier: string[];
  /** Tables visible at the cursor, innermost query first. */
  scope: ScopeTable[];
  /** CTEs visible at the cursor. */
  ctes: ScopeTable[];
  /** For `joinCondition`: the table just joined. */
  joinTarget?: ScopeTable;
  /** The clause keyword governing the cursor (upper case), if any. */
  clause?: string;
}

const CLAUSE_WORDS = new Set([
  "SELECT",
  "FROM",
  "WHERE",
  "JOIN",
  "ON",
  "USING",
  "BY",
  "HAVING",
  "SET",
  "VALUES",
  "INTO",
  "UPDATE",
  "TABLE",
  "LIMIT",
  "OFFSET",
  "RETURNING",
  "WITH",
  "UNION",
  "INTERSECT",
  "EXCEPT",
  "PREWHERE",
  "QUALIFY",
  "WINDOW",
  "APPLY",
  "STRAIGHT_JOIN",
]);

const TABLE_WORDS = new Set(["FROM", "INTO", "UPDATE", "TABLE", "TRUNCATE", "USING"]);
const JOIN_WORDS = new Set(["JOIN", "STRAIGHT_JOIN", "APPLY"]);

/** Words that end a table reference (cannot be an implicit alias). */
const ALIAS_STOP = new Set([
  "WHERE",
  "JOIN",
  "ON",
  "USING",
  "GROUP",
  "ORDER",
  "LEFT",
  "RIGHT",
  "INNER",
  "OUTER",
  "CROSS",
  "FULL",
  "NATURAL",
  "LIMIT",
  "UNION",
  "INTERSECT",
  "EXCEPT",
  "MINUS",
  "SET",
  "VALUES",
  "HAVING",
  "WINDOW",
  "OFFSET",
  "FETCH",
  "FOR",
  "WITH",
  "PREWHERE",
  "FINAL",
  "SAMPLE",
  "ARRAY",
  "GLOBAL",
  "ANY",
  "ALL",
  "AS",
  "SELECT",
  "RETURNING",
  "INTO",
  "WHEN",
  "OUTPUT",
  "PIVOT",
  "UNPIVOT",
  "STRAIGHT_JOIN",
  "APPLY",
  "SEMI",
  "ANTI",
  "ASOF",
  "LATERAL",
  "QUALIFY",
  "FORMAT",
  "SETTINGS",
  "DEFAULT",
  "CONNECT",
  "START",
  "TABLESAMPLE",
  "LOCK",
  "PARTITION",
  "IGNORE",
  "FORCE",
  "USE",
]);

const OPERATOR_WORDS = new Set([
  "AND",
  "OR",
  "NOT",
  "IS",
  "IN",
  "LIKE",
  "ILIKE",
  "BETWEEN",
  "EXISTS",
  "CASE",
  "WHEN",
  "THEN",
  "ELSE",
  "DISTINCT",
  "ALL",
  "ANY",
  "SOME",
  "AS",
]);

const upper = (t: Token | undefined) => (t && t.kind === "word" ? t.text.toUpperCase() : "");
const isPunct = (t: Token | undefined, p: string) => !!t && t.kind === "punct" && t.text === p;
const isName = (t: Token | undefined) => !!t && (t.kind === "word" || t.kind === "quotedIdent");
const nameOf = (t: Token) => (t.kind === "quotedIdent" ? unquote(t.text) : t.text);
const eq = (a: string | undefined, b: string | undefined) => !!a && !!b && a.toLowerCase() === b.toLowerCase();

interface Group {
  open: number; // token index of "(" (-1 for the statement)
  close: number; // token index of ")" (tokens.length for the statement)
  parent: Group | null;
  depth: number;
  query: boolean;
  refs: ScopeTable[];
  ctes: ScopeTable[];
}

/** Statement bounds for the cursor: the statement containing it, or the one being typed after the last terminator. */
function statementBounds(dialect: LangDialect, doc: string, pos: number): { from: number; to: number } {
  const ranges = splitStatements(dialect, doc);
  const inside = ranges.find((r) => r.from <= pos && pos <= r.to);
  if (inside) return inside;
  let prev: { from: number; to: number } | null = null;
  for (const r of ranges) if (r.to < pos) prev = r;
  if (prev) {
    const between = tokenize(dialect, doc.slice(prev.to, pos));
    const ended = between.some((t) => (t.kind === "punct" && t.text === ";") || t.kind === "slashLine" || t.kind === "batchSeparator");
    if (!ended) return { from: prev.from, to: pos };
  }
  return { from: pos, to: pos };
}

/** Analyzes the cursor position `pos` of `doc`. */
export function analyzeSql(dialect: LangDialect, schema: CompletionSchema, doc: string, pos: number): SqlContext {
  // A comment outside any statement belongs to no statement range, so check it on the whole document first.
  const around = tokenize(dialect, doc).find((t) => t.start < pos && pos <= t.end);
  if (around?.kind === "lineComment" || (around?.kind === "blockComment" && (pos < around.end || !around.text.endsWith("*/")))) {
    return { kind: "none", from: pos, to: pos, prefix: "", qualifier: [], scope: [], ctes: [] };
  }
  const bounds = statementBounds(dialect, doc, pos);
  const stmtFrom = bounds.from;
  const stmtTo = Math.max(bounds.to, pos);
  const all = tokenize(dialect, doc.slice(stmtFrom, stmtTo)).map((t) => ({ ...t, start: t.start + stmtFrom, end: t.end + stmtFrom }));
  const empty: SqlContext = { kind: "none", from: pos, to: pos, prefix: "", qualifier: [], scope: [], ctes: [] };

  // The token under the cursor.
  const cur = all.find((t) => t.start < pos && pos <= t.end);
  let from = pos;
  let prefix = "";
  let quote: string | undefined;
  let prefixToken: Token | undefined;
  if (cur) {
    const closed = (open: string) => cur.text.length > 1 && cur.text.endsWith(open === "[" ? "]" : open);
    switch (cur.kind) {
      case "lineComment":
        return empty;
      case "blockComment":
        if (pos < cur.end || !cur.text.endsWith("*/")) return empty;
        break;
      case "string":
        if (pos < cur.end || !closed(cur.text.slice(-1) === "'" ? "'" : cur.text.slice(-1)) || cur.text.length === 1) return empty;
        break;
      case "quotedIdent":
        if (pos < cur.end || !closed(cur.text[0])) {
          quote = cur.text[0];
          from = cur.start + 1;
          prefix = doc.slice(from, pos);
          prefixToken = cur;
        }
        break;
      case "word":
      case "variable":
        from = cur.start;
        prefix = doc.slice(cur.start, pos);
        prefixToken = cur;
        break;
    }
  }

  const tokens = all.filter((t) => !isTrivia(t));
  const groups = buildGroups(tokens);
  for (const g of groups) collectRefsOnce(dialect, schema, tokens, g, groups);

  // Innermost group around the cursor, then its query ancestors.
  const containing = groups.filter((g) => (g.open < 0 || tokens[g.open].start < pos) && (g.close >= tokens.length || pos <= tokens[g.close].start));
  containing.sort((a, b) => b.depth - a.depth);
  const scope: ScopeTable[] = [];
  const ctes: ScopeTable[] = [];
  for (const g of containing) {
    if (g.query) scope.push(...g.refs);
    ctes.push(...g.ctes);
  }

  const before = tokens.filter((t) => t.end <= from && t !== prefixToken);
  const ctx: SqlContext = { kind: "column", from, to: pos, prefix, quote, qualifier: [], scope, ctes };
  const last = before[before.length - 1];
  if (!last) return { ...ctx, kind: "statementStart" };

  // Member access: a.b.|
  if (isPunct(last, ".")) {
    const qualifier: string[] = [];
    let k = before.length - 2;
    while (k >= 0 && isName(before[k])) {
      qualifier.unshift(nameOf(before[k]));
      if (k >= 2 && isPunct(before[k - 1], ".")) k -= 2;
      else break;
    }
    return { ...ctx, kind: qualifier.length ? "member" : "column", qualifier };
  }

  const clause = governingClause(before);
  ctx.clause = clause.word;
  const lastWord = upper(last);
  if (JOIN_WORDS.has(lastWord)) return { ...ctx, kind: "join" };
  if (TABLE_WORDS.has(lastWord)) {
    // `JOIN t USING (cols)` lists columns; `DELETE FROM t USING u` names tables.
    const usingAfterJoin = lastWord === "USING" && JOIN_WORDS.has(governingClause(before.slice(0, -1)).word ?? "");
    if (!usingAfterJoin) return { ...ctx, kind: "table" };
  }
  if (lastWord === "ON" || ((lastWord === "AND" || lastWord === "OR") && clause.word === "ON")) {
    const joinTarget = lastJoinedRef(scope, last.start);
    return { ...ctx, kind: "joinCondition", joinTarget };
  }
  if (isPunct(last, ",") && clause.word === "FROM" && clause.depth === 0) return { ...ctx, kind: "table" };
  if ((clause.word === "FROM" || JOIN_WORDS.has(clause.word ?? "")) && (isName(last) || isPunct(last, ")"))) {
    return { ...ctx, kind: "afterTable" };
  }
  if (isPunct(last, "(") && ["FROM", "JOIN", "EXISTS", "IN", "AS"].includes(upper(before[before.length - 2]))) {
    return { ...ctx, kind: upper(before[before.length - 2]) === "IN" ? "column" : "statementStart" };
  }
  if (isPunct(last, ";")) return { ...ctx, kind: "statementStart" };
  // After a value (a name, number, string or closing paren) an operator or keyword comes next.
  const valueEnd =
    last.kind === "number" ||
    last.kind === "string" ||
    last.kind === "quotedIdent" ||
    last.kind === "param" ||
    isPunct(last, ")") ||
    (last.kind === "word" && !OPERATOR_WORDS.has(lastWord) && !CLAUSE_WORDS.has(lastWord) && !isKeywordish(lastWord));
  if (valueEnd) return { ...ctx, kind: "afterValue" };
  return ctx;
}

/** Common keywords that are followed by an expression (so the cursor after them wants a column). */
function isKeywordish(word: string): boolean {
  return ["SELECT", "WHERE", "HAVING", "BY", "SET", "RETURNING", "TOP", "LIMIT", "OFFSET", "VALUES", "ON", "OVER", "PARTITION", "DISTINCT"].includes(word);
}

/** The nearest clause keyword at the cursor's paren level, walking backwards. */
function governingClause(before: Token[]): { word?: string; depth: number } {
  let depth = 0;
  for (let k = before.length - 1; k >= 0; k--) {
    const t = before[k];
    if (isPunct(t, ")")) depth++;
    else if (isPunct(t, "(")) {
      if (depth === 0) return { word: "(", depth: 0 };
      depth--;
    } else if (depth === 0 && t.kind === "word") {
      const w = t.text.toUpperCase();
      if (CLAUSE_WORDS.has(w)) return { word: w, depth: 0 };
    }
  }
  return { depth: 0 };
}

function lastJoinedRef(scope: ScopeTable[], before: number): ScopeTable | undefined {
  let best: ScopeTable | undefined;
  for (const r of scope) if (r.at < before && (!best || r.at > best.at)) best = r;
  return best;
}

function buildGroups(tokens: Token[]): Group[] {
  const root: Group = { open: -1, close: tokens.length, parent: null, depth: 0, query: true, refs: [], ctes: [] };
  const groups: Group[] = [root];
  const stack: Group[] = [root];
  tokens.forEach((t, i) => {
    if (isPunct(t, "(")) {
      const parent = stack[stack.length - 1];
      const g: Group = { open: i, close: tokens.length, parent, depth: parent.depth + 1, query: false, refs: [], ctes: [] };
      const first = upper(tokens[i + 1]);
      g.query = first === "SELECT" || first === "WITH" || first === "VALUES";
      groups.push(g);
      stack.push(g);
    } else if (isPunct(t, ")") && stack.length > 1) {
      const g = stack.pop();
      if (g) g.close = i;
    }
  });
  return groups;
}

/** Index of the matching `)` for the `(` at `i` (or the end). */
function matchingClose(tokens: Token[], i: number, end: number): number {
  let depth = 0;
  for (let k = i; k < end; k++) {
    if (isPunct(tokens[k], "(")) depth++;
    else if (isPunct(tokens[k], ")")) {
      depth--;
      if (depth === 0) return k;
    }
  }
  return end;
}

function groupAt(groups: Group[], open: number): Group | undefined {
  return groups.find((g) => g.open === open);
}

/** Collects the CTEs and table refs of one query group. */
function collectRefs(dialect: LangDialect, schema: CompletionSchema, tokens: Token[], g: Group, groups: Group[]): void {
  if (!g.query) return;
  const start = g.open + 1;
  const end = g.close;
  let k = start;
  // CTEs: WITH [RECURSIVE] name [(cols)] AS [NOT] [MATERIALIZED] ( ... ) [, ...]
  if (upper(tokens[k]) === "WITH") {
    k++;
    if (upper(tokens[k]) === "RECURSIVE") k++;
    while (k < end && isName(tokens[k])) {
      const name = nameOf(tokens[k]);
      const at = tokens[k].start;
      k++;
      let explicit: string[] | null = null;
      if (isPunct(tokens[k], "(")) {
        const close = matchingClose(tokens, k, end);
        explicit = tokens
          .slice(k + 1, close)
          .filter(isName)
          .map(nameOf);
        k = close + 1;
      }
      if (upper(tokens[k]) !== "AS") break;
      k++;
      while (["NOT", "MATERIALIZED"].includes(upper(tokens[k]))) k++;
      if (!isPunct(tokens[k], "(")) break;
      const close = matchingClose(tokens, k, end);
      const body = groupAt(groups, k);
      const cte: ScopeTable = { kind: "cte", name, columns: [], depth: g.depth, at };
      g.ctes.push(cte);
      // Columns are filled lazily below, once every CTE name is known (a CTE may select from an earlier one).
      cteBodies.set(cte, { body, explicit });
      k = close + 1;
      if (isPunct(tokens[k], ",")) k++;
      else break;
    }
  }
  for (const cte of g.ctes) {
    const info = cteBodies.get(cte);
    if (!info) continue;
    cte.columns = info.explicit ? info.explicit.map((name) => ({ name })) : info.body ? deriveColumns(dialect, schema, tokens, info.body, groups) : [];
  }

  let mode: "none" | "from" = "none";
  for (let i = k; i < end; i++) {
    const t = tokens[i];
    if (isPunct(t, "(")) {
      i = matchingClose(tokens, i, end);
      continue;
    }
    const w = upper(t);
    if (w === "FROM" || w === "USING" || w === "UPDATE" || (w === "INTO" && upper(tokens[i - 1]) !== "SELECT") || w === "TABLE" || JOIN_WORDS.has(w)) {
      if (w === "USING" && isPunct(tokens[i + 1], "(")) continue; // JOIN ... USING (cols)
      const next = parseRef(dialect, schema, tokens, i + 1, end, g, groups);
      if (next > i + 1) i = next - 1;
      // A FROM list goes on through joins (`FROM a JOIN b ON ..., c`).
      if (w === "FROM") mode = "from";
      else if (!JOIN_WORDS.has(w)) mode = "none";
      continue;
    }
    if (mode === "from" && isPunct(t, ",")) {
      const next = parseRef(dialect, schema, tokens, i + 1, end, g, groups);
      if (next > i + 1) i = next - 1;
      continue;
    }
    if (w && CLAUSE_WORDS.has(w) && !["FROM", "ON", "USING"].includes(w) && !JOIN_WORDS.has(w)) mode = "none";
  }
}

const cteBodies = new WeakMap<ScopeTable, { body: Group | undefined; explicit: string[] | null }>();

/** Parses one table reference at `i`; returns the index after it. */
function parseRef(dialect: LangDialect, schema: CompletionSchema, tokens: Token[], i: number, end: number, g: Group, groups: Group[]): number {
  let k = i;
  if (upper(tokens[k]) === "LATERAL" || upper(tokens[k]) === "ONLY") k++;
  let ref: ScopeTable | null = null;
  if (isPunct(tokens[k], "(")) {
    const close = matchingClose(tokens, k, end);
    const sub = groupAt(groups, k);
    if (sub && sub.query) {
      ref = { kind: "subquery", name: "", columns: deriveColumns(dialect, schema, tokens, sub, groups), depth: g.depth, at: tokens[k].start };
    }
    k = close + 1;
  } else if (isName(tokens[k]) && !ALIAS_STOP.has(upper(tokens[k]))) {
    const parts = [nameOf(tokens[k])];
    const at = tokens[k].start;
    k++;
    while (isPunct(tokens[k], ".") && isName(tokens[k + 1])) {
      parts.push(nameOf(tokens[k + 1]));
      k += 2;
    }
    // `schema.` still being typed: not a table yet.
    if (isPunct(tokens[k], ".")) return k + 1;
    if (isPunct(tokens[k], "(")) {
      // A table function: no columns known.
      k = matchingClose(tokens, k, end) + 1;
    }
    const name = parts[parts.length - 1];
    const schemaName = parts.length > 1 ? parts[parts.length - 2] : undefined;
    ref = resolveRef(schema, g, name, schemaName, at);
  } else {
    return k;
  }
  // [AS] alias [(col aliases)]
  if (upper(tokens[k]) === "AS") k++;
  if (isName(tokens[k]) && !ALIAS_STOP.has(upper(tokens[k]))) {
    if (ref) ref.alias = nameOf(tokens[k]);
    k++;
    if (isPunct(tokens[k], "(") && ref) {
      const close = matchingClose(tokens, k, end);
      const names = tokens
        .slice(k + 1, close)
        .filter(isName)
        .map(nameOf);
      if (names.length) ref.columns = names.map((name, n) => ({ ...ref.columns[n], name }));
      k = close + 1;
    }
  }
  if (ref) {
    if (ref.kind === "subquery") ref.name = ref.alias ?? "";
    g.refs.push(ref);
  }
  return k;
}

function visibleCtes(g: Group | null): ScopeTable[] {
  const out: ScopeTable[] = [];
  for (let p = g; p; p = p.parent) out.push(...p.ctes);
  return out;
}

/** Finds a schema table by name (and schema), preferring the default schema. */
export function findTable(schema: CompletionSchema, name: string, schemaName?: string): CompletionTable | undefined {
  const matches = schema.tables.filter((t) => eq(t.name, name) && (!schemaName || eq(t.schema, schemaName)));
  if (matches.length <= 1) return matches[0];
  return (
    matches.find((t) => t.name === name && (!schemaName || t.schema === schemaName)) ??
    matches.find((t) => eq(t.schema, schema.defaultSchema)) ??
    matches.find((t) => !t.schema) ??
    matches[0]
  );
}

function resolveRef(schema: CompletionSchema, g: Group, name: string, schemaName: string | undefined, at: number): ScopeTable {
  if (!schemaName) {
    const cte = visibleCtes(g).find((c) => eq(c.name, name));
    if (cte) return { kind: "cte", name: cte.name, columns: cte.columns, depth: g.depth, at };
  }
  const table = findTable(schema, name, schemaName);
  return {
    kind: "table",
    name: table?.name ?? name,
    schema: table?.schema ?? schemaName,
    columns: table ? table.columns.map(toScopeColumn) : [],
    table,
    depth: g.depth,
    at,
  };
}

const toScopeColumn = (c: CompletionColumn): ScopeColumn => ({ name: c.name, type: c.type, pk: c.pk, fk: c.fk });

const deriving = new Set<Group>();

/** Output columns of a subquery: aliases, plain column names, and `*` / `t.*` expanded from its refs. */
function deriveColumns(dialect: LangDialect, schema: CompletionSchema, tokens: Token[], sub: Group, groups: Group[]): ScopeColumn[] {
  if (deriving.has(sub)) return [];
  deriving.add(sub);
  try {
    collectRefsOnce(dialect, schema, tokens, sub, groups);
    const start = sub.open + 1;
    const end = sub.close;
    // Skip a leading WITH ... part: find the SELECT at this level.
    let k = start;
    let depth = 0;
    for (; k < end; k++) {
      if (isPunct(tokens[k], "(")) depth++;
      else if (isPunct(tokens[k], ")")) depth--;
      else if (depth === 0 && upper(tokens[k]) === "SELECT") break;
    }
    if (k >= end) return [];
    k++;
    while (["DISTINCT", "ALL"].includes(upper(tokens[k]))) k++;
    if (upper(tokens[k]) === "TOP") k += 2;
    const items: Token[][] = [];
    let item: Token[] = [];
    depth = 0;
    for (; k < end; k++) {
      const t = tokens[k];
      if (depth === 0 && ["FROM", "INTO", "WHERE", "UNION", "GROUP", "ORDER", "LIMIT", "HAVING"].includes(upper(t))) break;
      if (isPunct(t, "(")) depth++;
      if (isPunct(t, ")")) depth--;
      if (depth === 0 && isPunct(t, ",")) {
        items.push(item);
        item = [];
      } else item.push(t);
    }
    if (item.length) items.push(item);
    const out: ScopeColumn[] = [];
    for (const it of items) {
      const lastTok = it[it.length - 1];
      const prev = it[it.length - 2];
      if (!lastTok) continue;
      if (lastTok.kind === "operator" && lastTok.text === "*") {
        const qual = it.length >= 3 && isPunct(prev, ".") ? nameOf(it[it.length - 3]) : null;
        for (const r of sub.refs) {
          if (!qual || eq(r.alias, qual) || eq(r.name, qual)) out.push(...r.columns);
        }
        continue;
      }
      if (!isName(lastTok)) continue;
      if (
        it.length === 1 ||
        upper(prev) === "AS" ||
        isPunct(prev, ".") ||
        (prev && (isName(prev) || isPunct(prev, ")") || prev.kind === "number" || prev.kind === "string"))
      ) {
        out.push({ name: nameOf(lastTok), ...typeOf(sub, it) });
      }
    }
    return out;
  } finally {
    deriving.delete(sub);
  }
}

/** The type of a plain `col` / `t.col` select item, from the subquery's refs. */
function typeOf(sub: Group, item: Token[]): { type?: string } {
  const names = item.filter(isName).map(nameOf);
  if (item.some((t) => t.kind === "operator" || isPunct(t, "("))) return {};
  const col = names[names.length - 1];
  const qual = names.length >= 2 && item.length >= 3 && isPunct(item[1], ".") ? names[0] : null;
  for (const r of sub.refs) {
    if (qual && !eq(r.alias, qual) && !eq(r.name, qual)) continue;
    const c = r.columns.find((x) => eq(x.name, col));
    if (c?.type) return { type: c.type };
  }
  return {};
}

const collected = new WeakSet<Group>();

function collectRefsOnce(dialect: LangDialect, schema: CompletionSchema, tokens: Token[], g: Group, groups: Group[]): void {
  if (collected.has(g)) return;
  collected.add(g);
  g.refs = [];
  g.ctes = [];
  collectRefs(dialect, schema, tokens, g, groups);
}

/** Resolves `x.` in scope: columns of a table/alias/CTE, tables of a schema, or schemas of a database. */
export type MemberTarget =
  { kind: "columns"; table: ScopeTable } | { kind: "tables"; schema: string } | { kind: "schemas"; database: string } | { kind: "unknown" };

export function resolveMember(schema: CompletionSchema, ctx: SqlContext): MemberTarget {
  const q = ctx.qualifier;
  if (q.length === 1) {
    const name = q[0];
    const byAlias = ctx.scope.find((r) => eq(r.alias, name)) ?? ctx.scope.find((r) => !r.alias && eq(r.name, name)) ?? ctx.scope.find((r) => eq(r.name, name));
    if (byAlias) return { kind: "columns", table: byAlias };
    const cte = ctx.ctes.find((c) => eq(c.name, name));
    if (cte) return { kind: "columns", table: cte };
    if (schema.schemas.some((s) => eq(s, name))) return { kind: "tables", schema: name };
    if (schema.tables.some((t) => eq(t.schema, name))) return { kind: "tables", schema: name };
    if (schema.databases.some((d) => eq(d, name))) return { kind: "schemas", database: name };
    const table = findTable(schema, name);
    if (table)
      return {
        kind: "columns",
        table: { kind: "table", name: table.name, schema: table.schema, columns: table.columns.map(toScopeColumn), table, depth: 0, at: 0 },
      };
    return { kind: "unknown" };
  }
  if (q.length >= 2) {
    const [a, b] = q.slice(-2);
    const table = findTable(schema, b, a);
    if (table)
      return {
        kind: "columns",
        table: { kind: "table", name: table.name, schema: table.schema, columns: table.columns.map(toScopeColumn), table, depth: 0, at: 0 },
      };
    if (schema.tables.some((t) => eq(t.schema, b))) return { kind: "tables", schema: b };
  }
  return { kind: "unknown" };
}
