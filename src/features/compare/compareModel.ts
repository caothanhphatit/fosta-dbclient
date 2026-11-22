/**
 * Schema compare, the pure part: what a compare tab compares (`CompareRequest`), the default options, how the diff
 * items are grouped, filtered and selected, the rows of the changes table, the target's safety notice and the file name
 * of a saved script. No DOM, so it is unit-tested.
 */
import type { Change, CompareOptions, DiffItem, DiffStatus, Risk, ScriptWarning } from "../../core/contract";
import type { SafetySettings } from "../../core/types";
import { effectiveRules } from "../safety/safety";

/** One side of a compare: a connection, a database on it, and a schema for engines with schemas. */
export interface CompareSide {
  connectionId: string;
  database: string;
  schema: string | null;
}

/** What a compare tab compares; the diff makes `target` match `source`. */
export interface CompareRequest {
  source: CompareSide;
  target: CompareSide;
  options: CompareOptions;
}

/** Mirrors `CompareOptions::default()` (`contract/migration.rs`). */
export const DEFAULT_EXCLUDES = [
  "flyway_schema_history",
  "__EFMigrationsHistory",
  "schema_migrations",
  "ar_internal_metadata",
  "knex_migrations*",
  "_prisma_migrations",
  "alembic_version",
  "django_migrations",
  "goose_db_version",
];

export function defaultOptions(): CompareOptions {
  return {
    ignoreColumnOrder: true,
    ignoreComments: false,
    ignoreCollation: true,
    ignoreDefaults: false,
    ignoreConstraintNames: false,
    ignoreIndexNames: false,
    caseInsensitiveNames: null,
    includeKinds: ["table", "view"],
    exclude: [...DEFAULT_EXCLUDES],
    transactional: null,
    detectRenames: true,
  };
}

/** The boolean options the options panel shows, in order. */
export const OPTION_TOGGLES: { key: keyof CompareOptions & string; label: string; hint: string }[] = [
  { key: "ignoreColumnOrder", label: "Ignore column order", hint: "Columns in another order are not a difference" },
  { key: "ignoreComments", label: "Ignore comments", hint: "Table, column and view comments" },
  { key: "ignoreCollation", label: "Ignore collations", hint: "Column collations" },
  { key: "ignoreDefaults", label: "Ignore defaults", hint: "Column default expressions" },
  { key: "ignoreConstraintNames", label: "Ignore constraint names", hint: "Match keys, checks and foreign keys by definition only" },
  { key: "ignoreIndexNames", label: "Ignore index names", hint: "Match indexes by definition only" },
  { key: "detectRenames", label: "Hint possible renames", hint: "One column removed and one added with the same type" },
];

/** Options read back from saved state: known fields only, the rest from the defaults. */
export function parseOptions(value: unknown): CompareOptions {
  const options = defaultOptions();
  if (typeof value !== "object" || value === null) return options;
  const record = value as Record<string, unknown>;
  for (const { key } of OPTION_TOGGLES) if (typeof record[key] === "boolean") (options as unknown as Record<string, unknown>)[key] = record[key];
  if (Array.isArray(record.exclude) && record.exclude.every((item) => typeof item === "string")) options.exclude = record.exclude as string[];
  if (typeof record.transactional === "boolean") options.transactional = record.transactional;
  if (typeof record.caseInsensitiveNames === "boolean") options.caseInsensitiveNames = record.caseInsensitiveNames;
  return options;
}

/** "shop.public", or the database alone for engines without schemas. */
export function sideName(side: CompareSide): string {
  return side.schema ? `${side.database}.${side.schema}` : side.database;
}

/** The tab title: the schema (or database) names, with the connections when the names alone are the same. */
export function compareTitle(request: CompareRequest, connectionName: (id: string) => string): string {
  const short = (side: CompareSide) => side.schema ?? side.database;
  const source = short(request.source);
  const target = short(request.target);
  if (source !== target) return `${source} → ${target}`;
  if (request.source.connectionId !== request.target.connectionId)
    return `${connectionName(request.source.connectionId)} → ${connectionName(request.target.connectionId)}`;
  return `${sideName(request.source)} → ${sideName(request.target)}`;
}

/** The tab key's parts after the source connection: the same pair opens the same tab. */
export function compareKeyParts(request: CompareRequest): string[] {
  const { source, target } = request;
  return [`${source.database}/${source.schema ?? ""}→${target.connectionId}/${target.database}/${target.schema ?? ""}`];
}

export function swapped(request: CompareRequest): CompareRequest {
  return { ...request, source: request.target, target: request.source };
}

/** Two connections can be compared when their engines are the same. */
export function comparableEngines(source: string, target: string): boolean {
  return source === target;
}

// --- The list ------------------------------------------------------------------------------------------------------------

export interface ItemGroup {
  status: DiffStatus;
  label: string;
  /** The items shown (after the filter). */
  items: DiffItem[];
  /** Every item of the status, filtered or not. */
  total: number;
}

const GROUPS: { status: DiffStatus; label: string }[] = [
  { status: "different", label: "Different" },
  { status: "onlyInSource", label: "Only in source (will be created)" },
  { status: "onlyInTarget", label: "Only in target (will be dropped)" },
  { status: "identical", label: "Identical" },
];

/** What an item's badge shows: dropping a target-only table loses its rows, whatever the item's own risk says. */
export function displayRisk(item: DiffItem): Risk {
  if (item.status === "onlyInTarget" && item.kind === "table") return "dataLoss";
  return item.risk;
}

export function riskLabel(risk: Risk): string {
  return risk === "dataLoss" ? "Data loss" : risk === "review" ? "Review" : "";
}

/** Items that change something on the target (identical ones have nothing to script). */
export function selectable(item: DiffItem): boolean {
  return item.status !== "identical";
}

/** Different and source-only items start checked; target-only ones (drops) start unchecked. */
export function defaultSelected(item: DiffItem): boolean {
  return item.status === "different" || item.status === "onlyInSource";
}

export function defaultSelection(items: readonly DiffItem[]): Set<string> {
  return new Set(items.filter(defaultSelected).map((item) => item.id));
}

/**
 * The selection after a re-diff (options changed, swap, compare again): an item keeps the user's choice while its status
 * is the same as before; new items and items whose status changed take the default.
 */
export function carrySelection(previousItems: readonly DiffItem[], previous: ReadonlySet<string>, items: readonly DiffItem[]): Set<string> {
  const before = new Map(previousItems.map((item) => [item.id, item.status]));
  const selection = new Set<string>();
  for (const item of items) {
    if (!selectable(item)) continue;
    const keep = before.get(item.id) === item.status;
    if (keep ? previous.has(item.id) : defaultSelected(item)) selection.add(item.id);
  }
  return selection;
}

/** Case-insensitive match on the name, kind or id. */
export function matchesFilter(item: DiffItem, filter: string): boolean {
  const text = filter.trim().toLowerCase();
  if (!text) return true;
  return item.name.toLowerCase().includes(text) || item.id.toLowerCase().includes(text);
}

/** The groups in display order; empty groups are left out, and Identical only when asked for. */
export function groupItems(items: readonly DiffItem[], filter: string, showIdentical: boolean): ItemGroup[] {
  return GROUPS.filter((group) => showIdentical || group.status !== "identical")
    .map((group) => {
      const all = items.filter((item) => item.status === group.status);
      return { ...group, items: all.filter((item) => matchesFilter(item, filter)), total: all.length };
    })
    .filter((group) => group.total > 0);
}

/** The state of a group's checkbox over its shown items. */
export function groupCheckState(group: ItemGroup, selection: ReadonlySet<string>): "all" | "none" | "some" {
  const items = group.items.filter(selectable);
  const checked = items.filter((item) => selection.has(item.id)).length;
  return checked === 0 ? "none" : checked === items.length ? "all" : "some";
}

/** Checks or unchecks every shown item of a group. */
export function setGroup(selection: ReadonlySet<string>, group: ItemGroup, checked: boolean): Set<string> {
  const next = new Set(selection);
  for (const item of group.items) {
    if (!selectable(item)) continue;
    if (checked) next.add(item.id);
    else next.delete(item.id);
  }
  return next;
}

/** The ids `schema_sync_script` gets, in diff order. */
export function includedIds(items: readonly DiffItem[], selection: ReadonlySet<string>): string[] {
  return items.filter((item) => selectable(item) && selection.has(item.id)).map((item) => item.id);
}

/** "3 changes", "new", "drop", "" for identical items. */
export function changeSummary(item: DiffItem): string {
  if (item.status === "onlyInSource") return "new";
  if (item.status === "onlyInTarget") return "drop";
  if (item.status === "identical") return "";
  return item.changes.length === 1 ? "1 change" : `${item.changes.length} changes`;
}

/** Counts for the toolbar: how many items differ, and how many are checked. */
export function summary(items: readonly DiffItem[], selection: ReadonlySet<string>): string {
  const changed = items.filter(selectable).length;
  if (changed === 0) return items.length === 0 ? "Nothing to compare" : "No differences";
  const checked = includedIds(items, selection).length;
  return `${changed} ${changed === 1 ? "difference" : "differences"} · ${checked} selected`;
}

// --- Details -------------------------------------------------------------------------------------------------------------

const PART_LABELS: Record<string, string> = {
  column: "Column",
  columnOrder: "Column order",
  primaryKey: "Primary key",
  unique: "Unique",
  check: "Check",
  index: "Index",
  foreignKey: "Foreign key",
  comment: "Comment",
  option: "Option",
  definition: "Definition",
};

export function partLabel(part: string): string {
  return PART_LABELS[part] ?? part;
}

const CHANGE_LABELS: Record<Change["what"], string> = { added: "Added", removed: "Removed", modified: "Modified" };

/** The changes table: part | name | change | field | source | target | note, one row per changed field. */
export function changeRows(item: DiffItem): string[][] {
  const rows: string[][] = [];
  for (const change of item.changes) {
    const note = [riskLabel(change.risk), change.note ?? ""].filter(Boolean).join(": ");
    const base = [partLabel(change.part), change.name, CHANGE_LABELS[change.what]];
    if (change.fields.length === 0) rows.push([...base, "", "", "", note]);
    change.fields.forEach((field, index) => rows.push([...base, field.field, field.source ?? "", field.target ?? "", index === 0 ? note : ""]));
  }
  return rows;
}

/** The script's warnings grouped by item id (clicking one selects its item). */
export function warningsOf(warnings: readonly ScriptWarning[], itemId: string): ScriptWarning[] {
  return warnings.filter((warning) => warning.itemId === itemId);
}

// --- The target's rules --------------------------------------------------------------------------------------------------

export interface SafetyNotice {
  tone: "danger" | "warning" | "info";
  text: string;
}

/**
 * What the target's rules mean for running the script in a query tab there (empty when nothing gets in the way).
 * `production`: the target's workspace is marked Production.
 */
export function safetyNotice(settings: SafetySettings | null, production: boolean): SafetyNotice[] {
  if (!settings) return [];
  const rules = effectiveRules(settings);
  const notices: SafetyNotice[] = [];
  if (rules.rules.ddl === "block" || rules.rules.destructive === "block") {
    const where = production ? "The target is a Production workspace and its rules block DDL" : "The target's rules block DDL";
    notices.push({ tone: "danger", text: `${where}: save the script and apply it through your migration process.` });
    return notices;
  }
  if (!rules.allowMultiStatement)
    notices.push({ tone: "warning", text: "The target's workspace runs one statement at a time: run the script statement by statement." });
  const asked = [
    rules.rules.ddl === "confirm" ? "a confirmation for DDL" : rules.rules.ddl === "typed" ? "a typed confirmation for DDL" : "",
    rules.rules.destructive === "confirm" ? "a confirmation for DROP" : rules.rules.destructive === "typed" ? "a typed confirmation for DROP" : "",
  ].filter(Boolean);
  if (asked.length > 0) notices.push({ tone: "info", text: `Running it on the target asks for ${asked.join(" and ")}.` });
  if (rules.statementTimeoutSecs > 0)
    notices.push({ tone: "info", text: `Statements stop after ${rules.statementTimeoutSecs} s (the target's statement timeout); a long ALTER may need more.` });
  return notices;
}

/** `202611221530_sync_shop.sql` */
export function scriptFileName(now: Date, targetDatabase: string): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}${pad(now.getHours())}${pad(now.getMinutes())}`;
  const name = targetDatabase.replace(/[^A-Za-z0-9_.-]+/g, "_") || "target";
  return `${stamp}_sync_${name}.sql`;
}
