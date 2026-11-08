/**
 * The pre-contract commands (`list_databases`, `browse_table`, `run_sql`, …) answered from the engine mocks' contract
 * operations, like the Rust shims. Removed with the old commands in step 16.
 */
import type { ColumnInfo, NodeKey, ObjectNode, ObjectRef, RowChange, StreamEvent, Value } from "../../core/contract";
import type {
  BrowseRequest as OldBrowseRequest,
  CellValue,
  ColumnMeta,
  QueryOutcome,
  ResultSet,
  RowChange as OldRowChange,
  TableDesign,
  TableInfo,
  TablePage,
  TableRelations,
} from "../../core/types";
import { displayValue, isNumericKind, parseInput } from "../../core/values";
import { mockError } from "./errors";
import type { EngineMock, RunContext } from "./types";

function required<K extends keyof EngineMock>(engine: EngineMock, member: K): NonNullable<EngineMock[K]> {
  const value = engine[member];
  if (!value)
    throw mockError("ENGINE_NOT_WIRED", `${engine.descriptor.label} support is planned but not available in this build of Fosta Studio yet.`, {
      engine: engine.descriptor.id,
    });
  return value as NonNullable<EngineMock[K]>;
}

function databaseKey(database: string): NodeKey {
  return { id: `db:${database}`, kind: "database", object: { database, schema: null, name: database, kind: "database" } };
}

export function listDatabases(engine: EngineMock): string[] {
  return required(engine, "tree")(null).map((node) => node.label);
}

function tableNodes(engine: EngineMock, database: string): ObjectNode[] {
  return required(engine, "tree")(databaseKey(database));
}

export function listTables(engine: EngineMock, database: string): TableInfo[] {
  return tableNodes(engine, database).map((node) => ({
    name: node.label,
    kind: node.kind === "view" ? "view" : "table",
    rows: node.detail ? Number(/~(\d+)/.exec(node.detail)?.[1] ?? NaN) || null : null,
    comment: "",
  }));
}

/** The object the old commands name as database + label ("dbo.Customers"). */
export function objectOf(engine: EngineMock, database: string, label: string): ObjectRef {
  const node = tableNodes(engine, database).find((item) => item.label.toLowerCase() === label.toLowerCase());
  if (!node?.object) throw mockError("ENGINE_ERROR", `Table '${database}.${label}' doesn't exist`, { engine: engine.descriptor.id });
  return node.object;
}

export function columnMeta(column: ColumnInfo): ColumnMeta {
  return {
    name: column.name,
    typeName: column.typeName,
    numeric: isNumericKind(column.kind),
    binary: Boolean(column.hints.binary) || column.kind.kind === "bytes",
    primary: Boolean(column.hints.primary),
    vectorDims: column.hints.vectorDims ?? null,
  };
}

const cell = (value: Value) => displayValue(value);

export function describeTable(engine: EngineMock, database: string, table: string): TableDesign {
  const description = required(engine, "describe")(objectOf(engine, database, table));
  return {
    columns: description.columns.map((column) => ({
      name: column.name,
      columnType: column.typeName,
      nullable: column.nullable,
      key: column.hints.primary ? "PRI" : "",
      defaultValue: column.default,
      extra: column.identity ? column.identity.toLowerCase() : "",
      comment: column.comment ?? "",
    })),
    ddl: description.ddl ?? "",
  };
}

export function browseTable(engine: EngineMock, request: OldBrowseRequest): TablePage {
  const language = engine.descriptor.filter.raw;
  const text = request.filter?.trim() ?? "";
  const page = required(
    engine,
    "browse",
  )({
    object: objectOf(engine, request.database, request.table),
    filter: text && language ? { kind: "raw", language, text } : { kind: "none" },
    sort: request.orderBy ? [{ column: request.orderBy, descending: request.descending }] : [],
    offset: request.offset,
    limit: request.limit,
    after: null,
    estimateTotal: false,
    exactTotal: true,
    columns: null,
  });
  return {
    columns: page.columns.map(columnMeta),
    rows: page.rows.map((row) => row.map(cell)),
    total: page.total?.value ?? page.rows.length,
    primaryKey: page.keyColumns,
    elapsedMs: page.elapsedMs,
  };
}

/** Old row changes (text cells) become typed contract changes without `original`: applied by key, as before. */
export function applyChanges(engine: EngineMock, database: string, table: string, changes: OldRowChange[]): number {
  const object = objectOf(engine, database, table);
  const columns = required(engine, "describe")(object).columns;
  const typed = (cells: CellValue[]) =>
    cells.map(({ column, value }) => {
      const info = columns.find((item) => item.name === column);
      if (value === null) return { column, value: { t: "null", v: null } as Value };
      const parsed = info ? parseInput(info.kind, value) : ({ t: "text", v: value } as Value);
      if (parsed instanceof Error) throw mockError("ERROR", `${column}: ${parsed.message}`);
      return { column, value: parsed };
    });
  const converted = changes.map((change): RowChange => {
    if (change.kind === "insert") return { op: "insert", values: typed(change.values) };
    if (change.kind === "update") return { op: "update", key: typed(change.key), values: typed(change.values), original: [] };
    return { op: "delete", key: typed(change.key), original: [] };
  });
  const result = required(engine, "apply")({ object, changes: converted });
  return result.inserted + result.updated + result.deleted;
}

export function tableRelations(engine: EngineMock, database: string, table: string): TableRelations {
  const description = required(engine, "describe")(objectOf(engine, database, table));
  const name = (schema: string | null, item: string) => (schema && engine.descriptor.capabilities.includes("schemas") ? `${schema}.${item}` : item);
  return {
    outgoing: description.foreignKeys.map((key) => ({
      constraint: key.name ?? "",
      database,
      table,
      columns: key.columns,
      referencedDatabase: key.refDatabase ?? database,
      referencedTable: name(key.refSchema, key.refTable),
      referencedColumns: key.refColumns,
    })),
    incoming: description.referencedBy.map((key) => ({
      constraint: key.name ?? "",
      database: key.database ?? database,
      table: name(key.schema, key.table),
      columns: key.columns,
      referencedDatabase: database,
      referencedTable: table,
      referencedColumns: key.refColumns,
    })),
  };
}

/** `run_sql`: the streamed run collected into result sets. */
export async function runSql(engine: EngineMock, sql: string, database: string | null, context: RunContext): Promise<QueryOutcome> {
  const started = performance.now();
  const sets: ResultSet[] = [];
  const byIndex = new Map<number, ResultSet>();
  const emit = (event: StreamEvent) => {
    if (event.event === "statement") return;
    let set = byIndex.get(event.index);
    if (!set) {
      set = { columns: [], rows: [], affectedRows: 0, truncated: false };
      byIndex.set(event.index, set);
      sets.push(set);
    }
    if (event.event === "columns") set.columns = event.columns.map(columnMeta);
    else if (event.event === "rows") set.rows.push(...event.rows.map((row) => row.map(cell)));
    else if (event.event === "done") {
      set.affectedRows = event.affected ?? 0;
      set.truncated = event.truncated;
    }
  };
  await required(engine, "run")({ text: sql, database, offset: 0 }, emit, context);
  const transaction = context.session.transaction;
  return {
    sets,
    elapsedMs: Math.round(performance.now() - started),
    transaction: { supported: transaction.supported, open: transaction.open, autoCommit: transaction.autoCommit },
  };
}
