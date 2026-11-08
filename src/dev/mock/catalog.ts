/**
 * An in-memory catalog (databases → tables / collections with typed rows) and the contract operations on it: tree
 * nodes, describe, browse (structured and raw filters, sort, offset and keyset paging, totals, row versions), apply with
 * optimistic concurrency (`CONFLICT` when a row changed or vanished since it was read), the ER graph and completion
 * names. Engine mocks give it their data and a `CatalogStyle` (quoting, labels, error texts).
 */
import type {
  ApplyResult,
  BrowseRequest,
  ColumnInfo,
  Field,
  FilterCond,
  ForeignKeyInfo,
  IndexInfo,
  NodeAction,
  ObjectDescription,
  ObjectNode,
  ObjectRef,
  Page,
  RawFilterLang,
  RowChange,
  RowChangeSet,
  RowConflict,
  SortKey,
  TreeRequest,
  Value,
  ValueKind,
} from "../../core/contract";
import type { CompletionSchema } from "../../core/api";
import type { SchemaGraph } from "../../core/types";
import { displayValue, isNumericKind, valueToJson } from "../../core/values";
import { mockError } from "./errors";

// --- Building data --------------------------------------------------------------------------------------------------------

export function column(
  name: string,
  typeName: string,
  kind: ValueKind | ValueKind["kind"],
  options: Partial<ColumnInfo> & { primary?: boolean } = {},
): ColumnInfo {
  const { primary, ...rest } = options;
  return {
    name,
    typeName,
    kind: typeof kind === "string" ? ({ kind } as ValueKind) : kind,
    nullable: !primary,
    default: null,
    comment: null,
    identity: null,
    generated: null,
    hints: primary ? { primary: true } : {},
    ...rest,
  };
}

export const v = {
  null: (): Value => ({ t: "null", v: null }),
  int: (n: number): Value => ({ t: "int", v: n }),
  float: (n: number): Value => ({ t: "float", v: n }),
  dec: (text: string): Value => ({ t: "decimal", v: text }),
  text: (s: string): Value => ({ t: "text", v: s }),
  bool: (b: boolean): Value => ({ t: "bool", v: b }),
  date: (s: string): Value => ({ t: "date", v: s }),
  ts: (s: string): Value => ({ t: "timestamp", v: s }),
  tstz: (s: string): Value => ({ t: "timestamptz", v: s }),
  json: (value: unknown): Value => ({ t: "json", v: JSON.stringify(value) }),
  uuid: (s: string): Value => ({ t: "uuid", v: s }),
  oid: (s: string): Value => ({ t: "objectId", v: s }),
  bytes: (hex: string): Value => ({ t: "bytes", v: hex }),
  other: (type: string, s: string): Value => ({ t: "other", type, v: s }),
  arr: (items: Value[]): Value => ({ t: "array", v: items }),
  doc: (fields: [string, Value][]): Value => ({ t: "doc", v: fields }),
  /** NULL when `value` is null/undefined, else `make(value)`. */
  opt: <T>(value: T | null | undefined, make: (value: T) => Value): Value => (value === null || value === undefined ? { t: "null", v: null } : make(value)),
};

export interface TableSpec {
  database: string;
  schema?: string | null;
  name: string;
  kind: "table" | "view" | "collection";
  columns: ColumnInfo[];
  /** Primary key columns (for a collection: `_id`). */
  key: string[];
  rows: () => Value[][];
  comment?: string;
  foreignKeys?: ForeignKeyInfo[];
  indexes?: IndexInfo[];
  ddl?: string;
  /** Pages carry `rowVersions` (SQL Server rowversion) and edits are checked by version. */
  versioned?: boolean;
  extra?: [string, string][];
}

export interface DatabaseSpec {
  name: string;
  system?: boolean;
  tables: TableSpec[];
}

export class MockTable {
  private data: Value[][] | null = null;
  private versionList: number[] = [];

  constructor(
    readonly spec: TableSpec,
    private readonly nextVersion: () => number,
  ) {}

  get object(): ObjectRef {
    return { database: this.spec.database, schema: this.spec.schema ?? null, name: this.spec.name, kind: this.spec.kind };
  }

  get rows(): Value[][] {
    if (!this.data) {
      this.data = this.spec.rows();
      this.versionList = this.data.map(() => this.nextVersion());
    }
    return this.data;
  }

  get versions(): number[] {
    void this.rows;
    return this.versionList;
  }

  bump(index: number): void {
    this.versionList[index] = this.nextVersion();
  }

  insert(row: Value[]): void {
    this.rows.push(row);
    this.versionList.push(this.nextVersion());
  }

  remove(index: number): void {
    this.rows.splice(index, 1);
    this.versionList.splice(index, 1);
  }

  columnIndex(name: string): number {
    return this.spec.columns.findIndex((column) => column.name.toLowerCase() === name.toLowerCase());
  }
}

/** How an engine names, quotes and reports things. */
export interface CatalogStyle {
  engine: string;
  /** Statement text shown with a page. */
  quote(name: string): string;
  /** The tree label and old-command name of a table ("dbo.Customers", "products"). */
  label(table: TableSpec): string;
  rawFilter: RawFilterLang | null;
  databaseActions: NodeAction[];
  tableActions(table: TableSpec): NodeAction[];
  /** "~12 rows" / "~12 docs". */
  countUnit: string;
  errors: {
    noTable(name: string): unknown;
    noColumn(name: string, where: string): unknown;
    syntax(near: string): unknown;
  };
}

export function builtin(id: string, label: string, name: string, separatorBefore = false): NodeAction {
  return { id, label, icon: null, target: { kind: "builtin", name: name as never }, danger: false, separatorBefore };
}

export function viewAction(id: string, label: string, role: string): NodeAction {
  return { id, label, icon: null, target: { kind: "view", role }, danger: false, separatorBefore: false };
}

export function queryAction(id: string, label: string, template: string, danger = false): NodeAction {
  return { id, label, icon: null, target: { kind: "query", template }, danger, separatorBefore: danger };
}

// --- Comparing values -----------------------------------------------------------------------------------------------------

const NUMERIC_TAGS = new Set(["int", "float", "decimal"]);

function isNumber(text: string): boolean {
  return text.trim() !== "" && !Number.isNaN(Number(text));
}

/** Orders a cell against typed text; null when the cell is NULL. Text compares case-insensitively (like *_ci collations). */
export function compareToText(value: Value, operand: string): number | null {
  if (value.t === "null") return null;
  const shown = displayValue(value) ?? "";
  if (NUMERIC_TAGS.has(value.t) && isNumber(operand)) return Number(shown) - Number(operand);
  if (value.t === "bool") return Number(value.v) - Number(["true", "1", "yes"].includes(operand.toLowerCase()));
  return shown.toLowerCase().localeCompare(operand.toLowerCase());
}

export function compareValues(a: Value, b: Value): number {
  if (a.t === "null" || b.t === "null") return a.t === b.t ? 0 : a.t === "null" ? -1 : 1;
  const left = displayValue(a) ?? "";
  const right = displayValue(b) ?? "";
  if (NUMERIC_TAGS.has(a.t) && NUMERIC_TAGS.has(b.t)) return Number(left) - Number(right);
  return left < right ? -1 : left > right ? 1 : 0;
}

export function sameValue(a: Value, b: Value): boolean {
  if (a.t === "null" || b.t === "null") return a.t === b.t;
  return displayValue(a) === displayValue(b);
}

function likeRegex(pattern: string): RegExp {
  const escaped = pattern
    .replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
    .replace(/%/g, ".*")
    .replace(/_/g, ".");
  return new RegExp(`^${escaped}$`, "is");
}

type RowPredicate = (row: Value[]) => boolean;

function conditionPredicate(table: MockTable, condition: FilterCond, style: CatalogStyle): RowPredicate {
  const index = table.columnIndex(condition.column);
  if (index < 0) throw style.errors.noColumn(condition.column, "where clause");
  const [first = "", second = ""] = condition.values;
  const text = (row: Value[]) => displayValue(row[index]);
  const order = (row: Value[], operand: string) => compareToText(row[index], operand);
  switch (condition.op) {
    case "eq":
      return (row) => order(row, first) === 0;
    case "ne":
      return (row) => {
        const result = order(row, first);
        return result !== null && result !== 0;
      };
    case "lt":
      return (row) => (order(row, first) ?? 0) < 0 && row[index].t !== "null";
    case "le":
      return (row) => (order(row, first) ?? 1) <= 0 && row[index].t !== "null";
    case "gt":
      return (row) => (order(row, first) ?? 0) > 0;
    case "ge":
      return (row) => (order(row, first) ?? -1) >= 0;
    case "contains":
      return (row) => text(row)?.toLowerCase().includes(first.toLowerCase()) ?? false;
    case "notContains":
      return (row) => !(text(row)?.toLowerCase().includes(first.toLowerCase()) ?? true);
    case "startsWith":
      return (row) => text(row)?.toLowerCase().startsWith(first.toLowerCase()) ?? false;
    case "endsWith":
      return (row) => text(row)?.toLowerCase().endsWith(first.toLowerCase()) ?? false;
    case "like":
      return (row) => likeRegex(first).test(text(row) ?? "\u0000");
    case "notLike":
      return (row) => text(row) !== null && !likeRegex(first).test(text(row) ?? "");
    case "in":
      return (row) => condition.values.some((operand) => order(row, operand) === 0);
    case "notIn":
      return (row) => row[index].t !== "null" && condition.values.every((operand) => order(row, operand) !== 0);
    case "between":
      return (row) => (order(row, first) ?? -1) >= 0 && (order(row, second) ?? 1) <= 0;
    case "isNull":
      return (row) => row[index].t === "null";
    case "isNotNull":
      return (row) => row[index].t !== "null";
  }
}

// --- Raw SQL WHERE text ---------------------------------------------------------------------------------------------------

/** Splits on a keyword at parenthesis depth 0, outside quotes. */
function splitTop(text: string, keyword: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let quote: string | null = null;
  let start = 0;
  const pattern = new RegExp(`^\\s${keyword}\\s`, "i");
  for (let i = 0; i < text.length; i += 1) {
    const char = text[i];
    if (quote) {
      if (char === quote) quote = text[i + 1] === quote ? (i++, quote) : null;
      continue;
    }
    if (char === "'" || char === '"' || char === "`") quote = char;
    else if (char === "[") quote = "]";
    else if (char === "(") depth += 1;
    else if (char === ")") depth -= 1;
    else if (depth === 0 && /\s/.test(char) && pattern.test(text.slice(i))) {
      const match = pattern.exec(text.slice(i));
      parts.push(text.slice(start, i));
      start = i + (match?.[0].length ?? 0);
      i = start - 1;
    }
  }
  parts.push(text.slice(start));
  return parts.map((part) => part.trim());
}

function stripParens(text: string): string {
  let result = text.trim();
  while (result.startsWith("(") && result.endsWith(")")) {
    let depth = 0;
    let wraps = true;
    for (let i = 0; i < result.length; i += 1) {
      if (result[i] === "(") depth += 1;
      else if (result[i] === ")") depth -= 1;
      if (depth === 0 && i < result.length - 1) {
        wraps = false;
        break;
      }
    }
    if (!wraps) break;
    result = result.slice(1, -1).trim();
  }
  return result;
}

function literal(text: string, style: CatalogStyle): string {
  const t = text.trim();
  const quoted = /^N?'((?:[^']|'')*)'$/is.exec(t);
  if (quoted) return quoted[1].replace(/''/g, "'").replace(/\\(.)/g, "$1");
  if (/^[+-]?\d+(\.\d+)?(e[+-]?\d+)?$/i.test(t)) return t;
  if (/^(true|false)$/i.test(t)) return t.toLowerCase();
  throw style.errors.syntax(t);
}

const ATOM =
  /^(`[^`]+`|"[^"]+"|\[[^\]]+\]|[\w$.]+)\s*(IS\s+NOT\s+NULL|IS\s+NULL|NOT\s+LIKE|LIKE|NOT\s+IN|IN|NOT\s+BETWEEN|BETWEEN|<>|!=|>=|<=|=|>|<)\s*([\s\S]*)$/i;

function unquoteIdent(text: string): string {
  const last =
    text
      .replace(/^[`"[]|[`"\]]$/g, "")
      .split(/[`"\]]?\.[`"[]?/)
      .pop() ?? text;
  return last.replace(/^[`"[]|[`"\]]$/g, "");
}

function sqlPredicate(table: MockTable, text: string, style: CatalogStyle): RowPredicate {
  const expression = stripParens(text);
  if (!expression) return () => true;
  const ors = splitTop(expression, "OR");
  if (ors.length > 1) {
    const parts = ors.map((part) => sqlPredicate(table, part, style));
    return (row) => parts.some((part) => part(row));
  }
  // BETWEEN a AND b: glue the AND back.
  const ands = splitTop(expression, "AND").reduce<string[]>((items, part) => {
    if (items.length > 0 && /\bBETWEEN\s+\S+$/i.test(items[items.length - 1])) items[items.length - 1] += ` AND ${part}`;
    else items.push(part);
    return items;
  }, []);
  if (ands.length > 1) {
    const parts = ands.map((part) => sqlPredicate(table, part, style));
    return (row) => parts.every((part) => part(row));
  }
  const negated = /^NOT\s+([\s\S]+)$/i.exec(expression);
  if (negated) {
    const inner = sqlPredicate(table, negated[1], style);
    return (row) => !inner(row);
  }
  const match = ATOM.exec(expression);
  if (!match) throw style.errors.syntax(expression.slice(0, 40));
  const name = unquoteIdent(match[1]);
  const operator = match[2].toUpperCase().replace(/\s+/g, " ");
  const rest = match[3].trim();
  const op = (op: FilterCond["op"], values: string[]) => conditionPredicate(table, { column: name, op, values }, style);
  switch (operator) {
    case "IS NULL":
      return op("isNull", []);
    case "IS NOT NULL":
      return op("isNotNull", []);
    case "LIKE":
      return op("like", [literal(rest, style)]);
    case "NOT LIKE":
      return op("notLike", [literal(rest, style)]);
    case "IN":
    case "NOT IN": {
      const list = /^\(([\s\S]*)\)$/.exec(rest);
      if (!list) throw style.errors.syntax(rest);
      const values = splitTop(list[1].replace(/,/g, " , "), ",").flatMap((item) => (item ? [literal(item, style)] : []));
      return op(operator === "IN" ? "in" : "notIn", values);
    }
    case "BETWEEN":
    case "NOT BETWEEN": {
      const [low, high] = splitTop(rest, "AND");
      if (!high) throw style.errors.syntax(rest);
      const between = op("between", [literal(low, style), literal(high, style)]);
      return operator === "BETWEEN" ? between : (row) => !between(row);
    }
    default: {
      const operand = literal(rest, style);
      const ops: Record<string, FilterCond["op"]> = { "=": "eq", "<>": "ne", "!=": "ne", "<": "lt", "<=": "le", ">": "gt", ">=": "ge" };
      return op(ops[operator], [operand]);
    }
  }
}

/** A row predicate for a raw SQL WHERE text (errors like the engine: unknown column, syntax). */
export function rawSqlFilter(table: MockTable, text: string, style: CatalogStyle): RowPredicate {
  return sqlPredicate(table, text, style);
}

// --- MongoDB JSON filter --------------------------------------------------------------------------------------------------

function jsonAt(row: Value[], table: MockTable, path: string): unknown {
  const [head, ...rest] = path.split(".");
  const index = table.columnIndex(head);
  if (index < 0) return undefined;
  let current: unknown = valueToJson(row[index]);
  for (const part of rest) {
    if (typeof current !== "object" || current === null) return undefined;
    current = (current as Record<string, unknown>)[part];
  }
  return current;
}

function jsonCompare(a: unknown, b: unknown): number {
  // Decimals arrive as text in relaxed JSON; compare them as numbers against numbers.
  if ((typeof a === "number" || typeof b === "number") && isNumber(String(a)) && isNumber(String(b))) return Number(a) - Number(b);
  return String(a).localeCompare(String(b));
}

function jsonEqual(a: unknown, b: unknown): boolean {
  if (Array.isArray(a) && !Array.isArray(b)) return a.some((item) => jsonEqual(item, b));
  return JSON.stringify(a) === JSON.stringify(b);
}

function mongoMatches(row: Value[], table: MockTable, filter: Record<string, unknown>): boolean {
  return Object.entries(filter).every(([key, condition]) => {
    if (key === "$and" && Array.isArray(condition)) return condition.every((part) => mongoMatches(row, table, part as Record<string, unknown>));
    if (key === "$or" && Array.isArray(condition)) return condition.some((part) => mongoMatches(row, table, part as Record<string, unknown>));
    const actual = jsonAt(row, table, key);
    const isOperators =
      typeof condition === "object" && condition !== null && !Array.isArray(condition) && Object.keys(condition).every((name) => name.startsWith("$"));
    if (!isOperators || "$oid" in (condition as object)) return jsonEqual(actual ?? null, condition);
    return Object.entries(condition as Record<string, unknown>).every(([operator, operand]) => {
      switch (operator) {
        case "$eq":
          return jsonEqual(actual ?? null, operand);
        case "$ne":
          return !jsonEqual(actual ?? null, operand);
        case "$gt":
          return actual !== undefined && actual !== null && jsonCompare(actual, operand) > 0;
        case "$gte":
          return actual !== undefined && actual !== null && jsonCompare(actual, operand) >= 0;
        case "$lt":
          return actual !== undefined && actual !== null && jsonCompare(actual, operand) < 0;
        case "$lte":
          return actual !== undefined && actual !== null && jsonCompare(actual, operand) <= 0;
        case "$in":
          return Array.isArray(operand) && operand.some((item) => jsonEqual(actual ?? null, item));
        case "$nin":
          return Array.isArray(operand) && !operand.some((item) => jsonEqual(actual ?? null, item));
        case "$exists":
          return (actual !== undefined && actual !== null) === Boolean(operand);
        case "$regex":
          return typeof actual === "string" && new RegExp(String(operand), "i").test(actual);
        default:
          throw mockError("ENGINE_ERROR", `unknown operator: ${operator}`, { engine: "mongodb", vendorCode: 2, kind: "server" });
      }
    });
  });
}

export function mongoFilter(table: MockTable, text: string): RowPredicate {
  let filter: unknown;
  try {
    filter = JSON.parse(text);
  } catch (error) {
    throw mockError("INVALID_FILTER", `The filter is not valid JSON: ${error instanceof Error ? error.message : String(error)}`, {
      hint: 'Write a query document, e.g. { "status": "ACTIVE", "stock": { "$gt": 0 } }',
    });
  }
  if (typeof filter !== "object" || filter === null || Array.isArray(filter)) throw mockError("INVALID_FILTER", "The filter must be a JSON object");
  return (row) => mongoMatches(row, table, filter as Record<string, unknown>);
}

// --- The catalog ----------------------------------------------------------------------------------------------------------

const MAX_PAGE = 5000;

export class MockCatalog {
  readonly databases: { name: string; system: boolean; tables: MockTable[] }[];
  private version = 2000;

  constructor(
    readonly style: CatalogStyle,
    databases: DatabaseSpec[],
  ) {
    this.databases = databases.map((database) => ({
      name: database.name,
      system: Boolean(database.system),
      tables: database.tables.map((spec) => new MockTable(spec, () => (this.version += 1))),
    }));
  }

  /** Tables of one database in tree order (tables first, then views). */
  tablesOf(database: string): MockTable[] {
    return this.databases.find((item) => item.name.toLowerCase() === database.toLowerCase())?.tables ?? [];
  }

  allTables(): MockTable[] {
    return this.databases.flatMap((database) => database.tables);
  }

  table(object: Pick<ObjectRef, "database" | "schema" | "name">): MockTable {
    const name = object.name.toLowerCase();
    const schema = object.schema?.toLowerCase() ?? null;
    const found = this.tablesOf(object.database ?? "").find(
      (table) => table.spec.name.toLowerCase() === name && (schema === null || (table.spec.schema ?? "").toLowerCase() === schema),
    );
    if (!found) throw this.style.errors.noTable([object.database, object.schema, object.name].filter(Boolean).join("."));
    return found;
  }

  /** A table by its label in a database ("dbo.Customers", "products"), as the old commands name it. */
  tableByLabel(database: string, label: string): MockTable {
    const found = this.tablesOf(database).find((table) => this.style.label(table.spec).toLowerCase() === label.toLowerCase());
    if (!found) throw this.style.errors.noTable(`${database}.${label}`);
    return found;
  }

  /** A table named in SQL (`db.table`, `schema.table`, `[dbo].[T]`), looked up in `database` first. */
  resolve(name: string, database: string | null): MockTable | null {
    const parts = name.split(".").map((part) => part.replace(/^[`"[]|[`"\]]$/g, ""));
    const table = parts[parts.length - 1].toLowerCase();
    const qualifier = parts.length > 1 ? parts[parts.length - 2].toLowerCase() : null;
    const candidates = this.allTables().filter((item) => item.spec.name.toLowerCase() === table);
    return (
      candidates.find((item) => qualifier && (item.spec.database.toLowerCase() === qualifier || (item.spec.schema ?? "").toLowerCase() === qualifier)) ??
      candidates.find((item) => item.spec.database.toLowerCase() === (database ?? "").toLowerCase()) ??
      (qualifier ? null : (candidates[0] ?? null))
    );
  }

  tree(parent: TreeRequest["parent"]): ObjectNode[] {
    if (!parent) return this.databases.map((database) => this.databaseNode(database.name, database.system));
    const database = parent.object?.database ?? parent.object?.name ?? "";
    if (parent.kind !== "database") return [];
    return this.tablesOf(database).map((table) => this.tableNode(table));
  }

  private databaseNode(name: string, system: boolean): ObjectNode {
    const object: ObjectRef = { database: name, schema: null, name, kind: "database" };
    return {
      key: { id: `db:${name}`, kind: "database", object },
      label: name,
      kind: "database",
      icon: "database",
      object,
      detail: system ? "system" : null,
      badges: [],
      expandable: true,
      defaultView: null,
      actions: this.style.databaseActions,
    };
  }

  private tableNode(table: MockTable): ObjectNode {
    const spec = table.spec;
    const object = table.object;
    const schemaPart = spec.schema ? `schema:${spec.schema}/` : "";
    return {
      key: { id: `db:${spec.database}/${schemaPart}${spec.kind}:${spec.name}`, kind: spec.kind, object },
      label: this.style.label(spec),
      kind: spec.kind,
      icon: spec.kind === "view" ? "view" : "table",
      object,
      detail: spec.kind === "view" ? null : `~${table.rows.length} ${this.style.countUnit}`,
      badges: [],
      expandable: false,
      defaultView: "data",
      actions: this.style.tableActions(spec),
    };
  }

  describe(object: ObjectRef): ObjectDescription {
    const table = this.table(object);
    const spec = table.spec;
    const referencedBy = this.allTables().flatMap((other) =>
      (other.spec.foreignKeys ?? [])
        .filter((key) => key.refTable.toLowerCase() === spec.name.toLowerCase() && (key.refDatabase ?? other.spec.database) === spec.database)
        .map((key) => ({
          name: key.name,
          database: other.spec.database,
          schema: other.spec.schema ?? null,
          table: other.spec.name,
          columns: key.columns,
          refColumns: key.refColumns,
        })),
    );
    const primary: IndexInfo[] =
      spec.key.length > 0 ? [{ name: "PRIMARY", columns: spec.key, unique: true, primary: true, method: "BTREE", definition: null }] : [];
    return {
      object: table.object,
      comment: spec.comment ?? null,
      columns: spec.columns,
      primaryKey: spec.key.length > 0 ? { name: "PRIMARY", columns: spec.key } : null,
      uniqueKeys: [],
      foreignKeys: spec.foreignKeys ?? [],
      referencedBy,
      indexes: [...primary, ...(spec.indexes ?? [])],
      rowEstimate: spec.kind === "view" ? null : table.rows.length,
      ddl: spec.ddl ?? this.ddlOf(spec),
      extra: spec.extra ?? [],
    };
  }

  private ddlOf(spec: TableSpec): string {
    const q = this.style.quote;
    const lines = spec.columns.map(
      (item) => `  ${q(item.name)} ${item.typeName}${item.nullable ? "" : " NOT NULL"}${item.identity ? ` ${item.identity}` : ""}`,
    );
    if (spec.key.length > 0) lines.push(`  PRIMARY KEY (${spec.key.map(q).join(", ")})`);
    for (const key of spec.foreignKeys ?? [])
      lines.push(
        `  CONSTRAINT ${q(key.name ?? "fk")} FOREIGN KEY (${key.columns.map(q).join(", ")}) REFERENCES ${q(key.refTable)} (${key.refColumns.map(q).join(", ")})`,
      );
    const name = spec.schema ? `${q(spec.schema)}.${q(spec.name)}` : q(spec.name);
    return `CREATE ${spec.kind === "view" ? "VIEW" : "TABLE"} ${name} (\n${lines.join(",\n")}\n)`;
  }

  private versionValue(version: number): Value {
    return { t: "bytes", v: `0x${version.toString(16).padStart(16, "0")}` };
  }

  private keyOf(table: MockTable, row: Value[]): Value[] {
    return table.spec.key.map((name) => row[table.columnIndex(name)]);
  }

  predicate(table: MockTable, request: Pick<BrowseRequest, "filter">): RowPredicate {
    const filter = request.filter;
    if (filter.kind === "none") return () => true;
    if (filter.kind === "conditions") {
      const parts = filter.conditions.map((condition) => conditionPredicate(table, condition, this.style));
      if (parts.length === 0) return () => true;
      return filter.matchAny ? (row) => parts.some((part) => part(row)) : (row) => parts.every((part) => part(row));
    }
    if (filter.language === "mongoJson") return filter.text.trim() ? mongoFilter(table, filter.text) : () => true;
    if (filter.language !== "sql") throw mockError("INVALID_FILTER", `This engine does not take ${filter.language} filters`);
    return rawSqlFilter(table, filter.text, this.style);
  }

  sorter(table: MockTable, sort: SortKey[]): ((a: Value[], b: Value[]) => number) | null {
    if (sort.length === 0) return null;
    const keys = sort.map((key) => {
      const index = table.columnIndex(key.column);
      if (index < 0) throw this.style.errors.noColumn(key.column, "order clause");
      return { index, sign: key.descending ? -1 : 1 };
    });
    return (a, b) => {
      for (const { index, sign } of keys) {
        const order = compareValues(a[index], b[index]);
        if (order !== 0) return order * sign;
      }
      return 0;
    };
  }

  browse(request: BrowseRequest): Page {
    const started = performance.now();
    const table = this.table(request.object);
    const predicate = this.predicate(table, request);
    let entries = table.rows.map((row, index) => ({ row, version: table.versions[index] })).filter((entry) => predicate(entry.row));
    const sorter = this.sorter(table, request.sort);
    if (sorter) entries = [...entries].sort((a, b) => sorter(a.row, b.row));
    const sortedByKey = request.sort.length === 0 || request.sort.every((key, index) => key.column === table.spec.key[index] && !key.descending);
    let start = Math.max(request.offset, 0);
    if (request.after && sortedByKey && table.spec.key.length > 0) {
      const after = request.after;
      const next = entries.findIndex((entry) => {
        const key = this.keyOf(table, entry.row);
        for (let i = 0; i < key.length; i += 1) {
          const order = compareValues(key[i], after[i] ?? { t: "null", v: null });
          if (order !== 0) return order > 0;
        }
        return false;
      });
      start = next < 0 ? entries.length : next;
    }
    const limit = Math.min(Math.max(request.limit, 1), MAX_PAGE);
    const slice = entries.slice(start, start + limit);
    const hasMore = start + limit < entries.length;
    const projected = request.columns?.length
      ? request.columns.map((name) => {
          const index = table.columnIndex(name);
          if (index < 0) throw this.style.errors.noColumn(name, "field list");
          return index;
        })
      : table.spec.columns.map((_, index) => index);
    const unfiltered = request.filter.kind === "none";
    const total = request.exactTotal
      ? { value: entries.length, exact: true }
      : request.estimateTotal
        ? { value: unfiltered ? Math.round(entries.length / 10) * 10 || entries.length : entries.length, exact: !unfiltered }
        : null;
    const last = slice[slice.length - 1];
    return {
      columns: projected.map((index) => table.spec.columns[index]),
      rows: slice.map((entry) => projected.map((index) => entry.row[index])),
      hasMore,
      keyColumns: table.spec.key,
      nextKeyset: hasMore && sortedByKey && last && table.spec.key.length > 0 ? this.keyOf(table, last.row) : null,
      total,
      statement: this.statementOf(table, request, start, limit),
      elapsedMs: Math.max(1, Math.round(performance.now() - started)) + 3,
      ...(table.spec.versioned ? { rowVersions: slice.map((entry) => this.versionValue(entry.version)) } : {}),
    };
  }

  private statementOf(table: MockTable, request: BrowseRequest, offset: number, limit: number): string {
    const q = this.style.quote;
    const spec = table.spec;
    if (this.style.rawFilter === "mongoJson") {
      const filter = request.filter.kind === "raw" ? request.filter.text : request.filter.kind === "conditions" ? "{ …conditions… }" : "{}";
      return `db.getSiblingDB("${spec.database}").${spec.name}.find(${filter}).skip(${offset}).limit(${limit})`;
    }
    const name = [spec.database, spec.schema, spec.name]
      .filter(Boolean)
      .map((part) => q(String(part)))
      .join(".");
    const where =
      request.filter.kind === "raw"
        ? ` WHERE ${request.filter.text}`
        : request.filter.kind === "conditions" && request.filter.conditions.length > 0
          ? ` WHERE ${request.filter.conditions.map((condition) => `${q(condition.column)} ${condition.op} ?`).join(request.filter.matchAny ? " OR " : " AND ")}`
          : "";
    const order = request.sort.length > 0 ? ` ORDER BY ${request.sort.map((key) => `${q(key.column)}${key.descending ? " DESC" : ""}`).join(", ")}` : "";
    return `SELECT * FROM ${name}${where}${order} LIMIT ${limit} OFFSET ${offset}`;
  }

  private findRow(table: MockTable, key: Field[]): number {
    return table.rows.findIndex((row) =>
      key.every((field) => {
        const index = table.columnIndex(field.column);
        return index >= 0 && sameValue(row[index], field.value);
      }),
    );
  }

  private fieldsOf(table: MockTable, row: Value[]): Field[] {
    return table.spec.columns.map((item, index) => ({ column: item.name, value: row[index] }));
  }

  /** Applies the set if every update and delete still matches what was read; else nothing and `CONFLICT`. */
  apply(set: RowChangeSet): ApplyResult {
    const table = this.table(set.object);
    if (table.spec.kind === "view")
      throw mockError("ENGINE_ERROR", `${this.style.label(table.spec)} is a view; its rows cannot be edited here.`, { engine: this.style.engine });
    const conflicts: RowConflict[] = [];
    set.changes.forEach((change, index) => {
      if (change.op === "insert" || change.force) return;
      const rowIndex = this.findRow(table, change.key);
      const conflict = (current: Field[] | null): RowConflict => ({
        index,
        op: change.op,
        key: change.key,
        original: change.original,
        current,
        attempted: change.op === "update" ? change.values : [],
      });
      if (rowIndex < 0) {
        conflicts.push(conflict(null));
        return;
      }
      const row = table.rows[rowIndex];
      const changed = change.version
        ? !sameValue(change.version, this.versionValue(table.versions[rowIndex]))
        : change.original.some((field) => {
            const columnIndex = table.columnIndex(field.column);
            return columnIndex >= 0 && !sameValue(row[columnIndex], field.value);
          });
      if (changed) conflicts.push(conflict(this.fieldsOf(table, row)));
    });
    if (conflicts.length > 0) throw conflictError(table.object, conflicts, this.style.label(table.spec));
    const result: ApplyResult = { inserted: 0, updated: 0, deleted: 0 };
    for (const change of set.changes) this.applyOne(table, change, result);
    return result;
  }

  private applyOne(table: MockTable, change: RowChange, result: ApplyResult): void {
    if (change.op === "insert") {
      const row = table.spec.columns.map((): Value => ({ t: "null", v: null }));
      for (const field of change.values) {
        const index = table.columnIndex(field.column);
        if (index < 0) throw this.style.errors.noColumn(field.column, "field list");
        row[index] = field.value;
      }
      for (const name of table.spec.key) {
        const index = table.columnIndex(name);
        if (row[index].t !== "null") continue;
        const kind = table.spec.columns[index].kind.kind;
        if (kind === "objectId") row[index] = { t: "objectId", v: objectIdFor(Date.now() / 1000, table.rows.length + 1) };
        else if (kind === "int") row[index] = { t: "int", v: Math.max(0, ...table.rows.map((item) => Number(displayValue(item[index]) ?? 0))) + 1 };
      }
      table.insert(row);
      result.inserted += 1;
      return;
    }
    const rowIndex = this.findRow(table, change.key);
    if (rowIndex < 0) return;
    if (change.op === "delete") {
      table.remove(rowIndex);
      result.deleted += 1;
      return;
    }
    const row = table.rows[rowIndex];
    for (const field of change.values) {
      const index = table.columnIndex(field.column);
      if (index < 0) throw this.style.errors.noColumn(field.column, "field list");
      row[index] = field.value;
    }
    table.bump(rowIndex);
    result.updated += 1;
  }

  /**
   * Mock-only: changes (or deletes) one row as if another client did. Without `column`, every text and number column
   * outside the key changes, so a pending edit of any of them conflicts.
   */
  touch(tableName: string, key: unknown, remove: boolean, column?: string): string {
    const [first, second] = tableName.split(".");
    const table =
      (second ? this.allTables().find((item) => item.spec.database === first && item.spec.name.toLowerCase() === second.toLowerCase()) : undefined) ??
      this.allTables().find(
        (item) => this.style.label(item.spec).toLowerCase() === tableName.toLowerCase() || item.spec.name.toLowerCase() === tableName.toLowerCase(),
      );
    if (!table) throw new Error(`No table ${tableName}`);
    const keys = key === undefined || key === null ? [] : Array.isArray(key) ? key : typeof key === "object" ? Object.values(key as object) : [key];
    const rowIndex =
      keys.length === 0
        ? 0
        : table.rows.findIndex((row) =>
            table.spec.key.every((name, i) => keys[i] === undefined || displayValue(row[table.columnIndex(name)]) === String(keys[i])),
          );
    if (rowIndex < 0) throw new Error(`No row with key ${JSON.stringify(key)} in ${tableName}`);
    const row = table.rows[rowIndex];
    const shownKey = table.spec.key.map((name) => displayValue(row[table.columnIndex(name)])).join(", ");
    if (remove) {
      table.remove(rowIndex);
      return `Deleted row ${shownKey} of ${this.style.label(table.spec)}`;
    }
    const targets = table.spec.columns
      .map((item, index) => ({ item, index }))
      .filter(({ item }) => (column ? item.name.toLowerCase() === column.toLowerCase() : !table.spec.key.includes(item.name)))
      .filter(({ item }) => item.kind.kind === "text" || isNumericKind(item.kind));
    if (targets.length === 0)
      throw new Error(`${tableName} has no ${column ? `text or number column ${column}` : "text or number column"} the mock can change`);
    const changes = targets.map(({ item, index }) => {
      const current = row[index];
      const shown = displayValue(current);
      row[index] =
        item.kind.kind === "text"
          ? { t: "text", v: `${shown ?? ""} (changed elsewhere)` }
          : item.kind.kind === "int"
            ? { t: "int", v: Number(shown ?? 0) + 1 }
            : item.kind.kind === "float"
              ? { t: "float", v: Number(shown ?? 0) + 1 }
              : { t: "decimal", v: (Number(shown ?? 0) + 1).toFixed(2) };
      return `${item.name}: ${shown} → ${displayValue(row[index])}`;
    });
    table.bump(rowIndex);
    return `Changed row ${shownKey} of ${this.style.label(table.spec)} (${changes.join("; ")})`;
  }

  schemaGraph(database: string): SchemaGraph {
    const tables = this.tablesOf(database);
    return {
      totalTables: tables.length,
      tables: tables.map((table) => ({
        name: this.style.label(table.spec),
        kind: table.spec.kind === "view" ? "view" : "table",
        rows: table.spec.kind === "view" ? null : table.rows.length,
        comment: table.spec.comment ?? "",
        columns: table.spec.columns.map((item) => ({
          name: item.name,
          columnType: item.typeName,
          nullable: item.nullable,
          primary: Boolean(item.hints.primary),
        })),
      })),
      relations: tables.flatMap((table) =>
        (table.spec.foreignKeys ?? []).map((key) => ({
          constraint: key.name ?? "",
          database,
          table: this.style.label(table.spec),
          columns: key.columns,
          referencedDatabase: key.refDatabase ?? database,
          referencedTable: key.refSchema ? `${key.refSchema}.${key.refTable}` : key.refTable,
          referencedColumns: key.refColumns,
        })),
      ),
    };
  }

  completion(database: string | null): CompletionSchema {
    const tables = database ? this.tablesOf(database) : [];
    const schemas = [...new Set(tables.map((table) => table.spec.schema).filter((schema): schema is string => Boolean(schema)))];
    return {
      databases: this.databases.map((item) => item.name),
      schemas,
      defaultSchema: schemas[0],
      tables: tables.map(({ spec }) => ({
        name: spec.name,
        schema: spec.schema ?? undefined,
        kind: spec.kind,
        columns: spec.columns.map((column) => {
          const key = (spec.foreignKeys ?? []).find((fk) => fk.columns.includes(column.name));
          const ref = key?.refColumns[key.columns.indexOf(column.name)];
          return {
            name: column.name,
            type: column.typeName,
            pk: spec.key.includes(column.name),
            fk: key && ref ? { table: key.refTable, column: ref, schema: key.refSchema ?? undefined } : undefined,
          };
        }),
      })),
    };
  }

  addDatabase(name: string, tables: TableSpec[] = []): void {
    if (this.databases.some((database) => database.name.toLowerCase() === name.toLowerCase()))
      throw mockError("ENGINE_ERROR", `Can't create database '${name}'; database exists`, { engine: this.style.engine, vendorCode: 1007, sqlstate: "HY000" });
    this.databases.push({ name, system: false, tables: tables.map((spec) => new MockTable(spec, () => (this.version += 1))) });
  }
}

export function conflictError(object: ObjectRef, conflicts: RowConflict[], label: string) {
  const missing = conflicts.filter((conflict) => conflict.current === null).length;
  const changed = conflicts.length - missing;
  const parts = [changed > 0 ? `${changed} row(s) were changed by someone else` : "", missing > 0 ? `${missing} row(s) no longer exist` : ""].filter(Boolean);
  return mockError("CONFLICT", `Nothing was saved: ${parts.join(" and ")} since they were loaded from ${label}.`, { object, conflicts });
}

/** A deterministic 24-hex ObjectId. */
export function objectIdFor(seconds: number, counter: number): string {
  return Math.floor(seconds).toString(16).padStart(8, "0") + "c0ffee" + counter.toString(16).padStart(10, "0");
}
