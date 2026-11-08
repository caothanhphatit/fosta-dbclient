// The CodeMirror completion source for SQL: keywords and functions per dialect, tables, columns in scope (with alias
// resolution), `alias.` members, FK-based JOIN ... ON suggestions, identifiers quoted per dialect when needed.
import type { Completion, CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import type { CompletionSchema, CompletionTable, LangDialect } from "../types";
import { analyzeSql, resolveMember, type ScopeTable, type SqlContext } from "./analyze";
import { closingQuote, quoteIfNeeded } from "./dialect";
import { dialectFunctions, dialectKeywords, statementStarters } from "./keywords";

export interface SqlCompletionOptions {
  dialect: LangDialect;
  /** The current schema snapshot; called on every completion so the cache can change under the editor. */
  schema: () => CompletionSchema;
}

const IDENT_CHARS = /^[\w$#@]*$/;

/** Words offered first after a table name or alias in FROM/JOIN. */
const AFTER_TABLE = ["AS", "WHERE", "JOIN", "LEFT JOIN", "INNER JOIN", "ON", "USING", "GROUP BY", "ORDER BY", "LIMIT", "UNION"];
/** Words offered first after a value. */
const AFTER_VALUE = [
  "FROM",
  "AS",
  "AND",
  "OR",
  "WHERE",
  "GROUP BY",
  "ORDER BY",
  "HAVING",
  "LIMIT",
  "IS NULL",
  "IS NOT NULL",
  "IN",
  "LIKE",
  "BETWEEN",
  "ASC",
  "DESC",
  "THEN",
  "ELSE",
  "END",
];

/** Builds the completion result for `pos` in `doc` (exported for tests and non-CodeMirror callers). */
export function sqlCompletions(dialect: LangDialect, schema: CompletionSchema, doc: string, pos: number, explicit = false): CompletionResult | null {
  const ctx = analyzeSql(dialect, schema, doc, pos);
  if (ctx.kind === "none") return null;
  // Typing in the open (no prefix) only completes after a dot; elsewhere it waits for a letter or Ctrl-Space.
  if (!explicit && ctx.prefix === "" && !ctx.quote && ctx.kind !== "member") return null;
  const options = buildOptions(dialect, schema, ctx);
  if (!options.length) return null;
  return { from: ctx.from, to: ctx.to, options, validFor: IDENT_CHARS };
}

/** A CodeMirror `CompletionSource` over a schema provider. */
export function sqlCompletionSource(options: SqlCompletionOptions): CompletionSource {
  return (context: CompletionContext) => sqlCompletions(options.dialect, options.schema(), context.state.doc.toString(), context.pos, context.explicit);
}

function buildOptions(dialect: LangDialect, schema: CompletionSchema, ctx: SqlContext): Completion[] {
  const name = (n: string) => identifier(n, dialect, ctx);
  const kwCase = keywordCaser(ctx.prefix);
  const keywords = (boost: number, first: string[] = []) => {
    const out: Completion[] = first.map((k) => ({ label: kwCase(k), type: "keyword", boost: boost + 1 }));
    const seen = new Set(first);
    for (const k of dialectKeywords(dialect)) if (!seen.has(k)) out.push({ label: kwCase(k), type: "keyword", boost });
    return out;
  };
  const functions = (boost: number): Completion[] =>
    dialectFunctions(dialect).map((f) => ({ label: dialect === "clickHouse" ? f : kwCase(f), type: "function", detail: "function", boost }));

  switch (ctx.kind) {
    case "member": {
      const target = resolveMember(schema, ctx);
      if (target.kind === "columns") return columnOptions(target.table, name, 2);
      if (target.kind === "tables")
        return tableOptions(
          schema.tables.filter((t) => eqi(t.schema, target.schema)),
          name,
          false,
        );
      if (target.kind === "schemas") return schema.schemas.map((s) => ({ label: s, apply: name(s), type: "namespace", detail: "schema" }));
      return [];
    }
    case "table":
    case "join": {
      const out: Completion[] = [];
      if (ctx.kind === "join") out.push(...joinSuggestions(dialect, schema, ctx));
      out.push(...ctx.ctes.map((c) => ({ label: c.name, apply: name(c.name), type: "class", detail: "CTE", boost: 1 })));
      out.push(...tableOptions(schema.tables, name, true, schema.defaultSchema));
      out.push(
        ...schema.schemas
          .filter((s) => !eqi(s, schema.defaultSchema))
          .map((s) => ({ label: s, apply: name(s), type: "namespace", detail: "schema", boost: -1 })),
      );
      if (ctx.kind === "table") out.push(...keywords(-3, ["SELECT"]).filter((k) => /^(SELECT|LATERAL|ONLY)$/i.test(k.label)));
      return out;
    }
    case "joinCondition":
      return [...joinConditions(dialect, ctx), ...scopeColumnOptions(ctx, name), ...refOptions(ctx, name), ...keywords(-2)];
    case "column":
      return [...scopeColumnOptions(ctx, name), ...refOptions(ctx, name), ...functions(-1), ...keywords(-2)];
    case "afterTable":
      return keywords(0, AFTER_TABLE);
    case "afterValue":
      return keywords(0, AFTER_VALUE);
    case "statementStart":
      return keywords(0, statementStarters(dialect));
    default:
      return [];
  }
}

const eqi = (a: string | undefined, b: string | undefined) => !!a && !!b && a.toLowerCase() === b.toLowerCase();

/** Keywords follow the case of what was typed: `sel` gives `select`, anything else upper case. */
function keywordCaser(prefix: string): (k: string) => string {
  const lower = prefix !== "" && prefix === prefix.toLowerCase() && /[a-z]/.test(prefix);
  return (k) => (lower ? k.toLowerCase() : k);
}

/** The text to insert for an identifier: quoted when needed, or closed when the user opened a quote. */
function identifier(n: string, dialect: LangDialect, ctx: SqlContext): string {
  if (ctx.quote) {
    const close = closingQuote(ctx.quote);
    return n.split(close).join(close + close) + close;
  }
  return quoteIfNeeded(n, dialect);
}

function tableOptions(tables: CompletionTable[], name: (n: string) => string, showSchema: boolean, defaultSchema?: string): Completion[] {
  const multiSchema = new Set(tables.map((t) => t.schema ?? "")).size > 1;
  return tables.map((t) => {
    const qualify = showSchema && multiSchema && t.schema && !eqi(t.schema, defaultSchema);
    return {
      label: qualify ? `${t.schema}.${t.name}` : t.name,
      apply: qualify ? `${name(t.schema ?? "")}.${name(t.name)}` : name(t.name),
      type: t.kind === "view" || t.kind === "materializedView" ? "interface" : "class",
      detail: t.schema ? `${t.kind} · ${t.schema}` : t.kind,
      boost: qualify ? -1 : 0,
    };
  });
}

function columnOptions(table: ScopeTable, name: (n: string) => string, boost: number): Completion[] {
  return table.columns.map((c) => ({
    label: c.name,
    apply: name(c.name),
    type: "property",
    detail: [c.type, c.pk ? "PK" : "", c.fk ? `→ ${c.fk.table}.${c.fk.column}` : ""].filter(Boolean).join(" "),
    boost: boost + (c.pk ? 0.5 : 0),
  }));
}

const refName = (r: ScopeTable) => r.alias ?? r.name;

/** Columns of every table in scope; a name found in several tables is offered qualified. */
function scopeColumnOptions(ctx: SqlContext, name: (n: string) => string): Completion[] {
  const innermost = ctx.scope.length ? Math.max(...ctx.scope.map((r) => r.depth)) : 0;
  const counts = new Map<string, number>();
  for (const r of ctx.scope) for (const c of r.columns) counts.set(c.name.toLowerCase(), (counts.get(c.name.toLowerCase()) ?? 0) + 1);
  const out: Completion[] = [];
  const seen = new Set<string>();
  for (const r of ctx.scope) {
    const own = r.depth === innermost ? 1 : 0;
    for (const c of r.columns) {
      const ambiguous = (counts.get(c.name.toLowerCase()) ?? 0) > 1 && refName(r);
      const label = ambiguous ? `${refName(r)}.${c.name}` : c.name;
      if (seen.has(label)) continue;
      seen.add(label);
      out.push({
        label,
        apply: ambiguous ? `${name(refName(r))}.${name(c.name)}` : name(c.name),
        type: "property",
        detail: [refName(r), c.type].filter(Boolean).join(" · "),
        boost: own + (c.pk ? 0.2 : 0),
      });
    }
  }
  return out;
}

/** The aliases and table names in scope, so `alias.` can be typed from the list. */
function refOptions(ctx: SqlContext, name: (n: string) => string): Completion[] {
  const seen = new Set<string>();
  const out: Completion[] = [];
  for (const r of ctx.scope) {
    const n = refName(r);
    if (!n || seen.has(n.toLowerCase())) continue;
    seen.add(n.toLowerCase());
    out.push({ label: n, apply: name(n), type: "variable", detail: r.alias ? `alias of ${r.name || "subquery"}` : r.kind, boost: 0.5 });
  }
  return out;
}

/** Short alias for a table: initials of its words (`order_items` -> `oi`), unique among the names in scope. */
export function aliasFor(table: string, taken: Set<string>): string {
  const parts = table
    .replace(/([a-z])([A-Z])/g, "$1_$2")
    .split(/[^A-Za-z0-9]+/)
    .filter(Boolean);
  let base =
    parts
      .map((p) => p[0])
      .join("")
      .toLowerCase() || "t";
  if (!/^[a-z_]/.test(base)) base = "t" + base;
  let alias = base;
  for (let n = 2; taken.has(alias); n++) alias = base + n;
  return alias;
}

interface FkLink {
  /** Column of the table being joined. */
  joinedColumn: string;
  /** The table in scope and its column. */
  other: ScopeTable;
  otherColumn: string;
}

/** FK links between a candidate table and the tables in scope (either direction). */
function fkLinks(candidate: CompletionTable, scope: ScopeTable[], exclude?: ScopeTable): FkLink[] {
  const out: FkLink[] = [];
  for (const r of scope) {
    if (r === exclude || !r.table) continue;
    for (const c of candidate.columns) {
      if (c.fk && eqi(c.fk.table, r.table.name) && (!c.fk.schema || !r.table.schema || eqi(c.fk.schema, r.table.schema))) {
        out.push({ joinedColumn: c.name, other: r, otherColumn: c.fk.column });
      }
    }
    for (const c of r.table.columns) {
      if (c.fk && eqi(c.fk.table, candidate.name) && (!c.fk.schema || !candidate.schema || eqi(c.fk.schema, candidate.schema))) {
        out.push({ joinedColumn: c.fk.column, other: r, otherColumn: c.name });
      }
    }
  }
  return out;
}

/** After JOIN: `customers c ON c.id = o.customer_id` for every table linked by a foreign key to one in scope. */
function joinSuggestions(dialect: LangDialect, schema: CompletionSchema, ctx: SqlContext): Completion[] {
  const q = (n: string) => quoteIfNeeded(n, dialect);
  const taken = new Set(ctx.scope.map((r) => refName(r).toLowerCase()));
  const out: Completion[] = [];
  for (const t of schema.tables) {
    for (const link of fkLinks(t, ctx.scope)) {
      const alias = aliasFor(t.name, taken);
      const other = refName(link.other);
      const table = t.schema && !eqi(t.schema, schema.defaultSchema) && schema.schemas.length > 1 ? `${q(t.schema)}.${q(t.name)}` : q(t.name);
      const text = `${table} ${alias} ON ${alias}.${q(link.joinedColumn)} = ${q(other)}.${q(link.otherColumn)}`;
      out.push({ label: text, type: "class", detail: "foreign key", boost: 3 });
    }
  }
  return out;
}

/** After ON: conditions joining the table just joined to the others through foreign keys. */
function joinConditions(dialect: LangDialect, ctx: SqlContext): Completion[] {
  const target = ctx.joinTarget;
  if (!target?.table) return [];
  const q = (n: string) => quoteIfNeeded(n, dialect);
  const me = refName(target);
  return fkLinks(target.table, ctx.scope, target).map((link) => ({
    label: `${me}.${link.joinedColumn} = ${refName(link.other)}.${link.otherColumn}`,
    apply: `${q(me)}.${q(link.joinedColumn)} = ${q(refName(link.other))}.${q(link.otherColumn)}`,
    type: "text",
    detail: "foreign key",
    boost: 3,
  }));
}
