/**
 * The `vector.*` operations of the engine mocks (answered through `invoke_capability` like the backend): canned info,
 * health and recall, and a search returning the first rows of the table with growing distances.
 */
import type { ColumnInfo, ObjectRef, Value } from "../../core/contract";
import type { MockCatalog } from "./catalog";

/** A result-set column as the engine reports it (only the name and type are known). */
export function resultColumn(name: string, typeName: string, kind: "float" | "text"): ColumnInfo {
  return { name, typeName, kind: { kind }, nullable: true, default: null, comment: null, identity: null, generated: null, hints: {} };
}

export interface VectorMockSpec {
  catalog: MockCatalog;
  object: ObjectRef;
  column: string;
  dims: number;
  typeName: string;
  index: { name: string; kind: string; metric: string; m: number | null; params: string };
  metrics: string[];
  quote: (name: string) => string;
  /** The ORDER BY expression for `vector` (a placeholder or a literal). */
  distance: (vector: string) => string;
  placeholder: string;
  literal: (text: string) => string;
  plan: { columns: ColumnInfo[]; rows: Value[][] };
}

export function vectorOps(spec: VectorMockSpec): Record<string, (args: Record<string, unknown>) => unknown> {
  const { catalog, object, index } = spec;
  const sample = "[0.12,-0.4,0.33,0.9]";
  return {
    "vector.info": () => ({
      supported: true,
      columns: [{ name: spec.column, dims: spec.dims, typeName: spec.typeName }],
      indexes: [{ column: spec.column, ...index }],
      metrics: spec.metrics,
    }),
    "vector.fetch": () => [0.12, -0.4, 0.33, 0.9],
    "vector.search": (args) => {
      const k = Number(args.k ?? 10);
      const page = catalog.browse({
        object,
        filter: { kind: "none" },
        sort: [],
        offset: 0,
        limit: k,
        after: null,
        estimateTotal: false,
        exactTotal: false,
        columns: null,
      });
      // Like the engines: vector columns are left out of the select list, the distance comes first.
      const kept = page.columns.flatMap((column, position) => (column.hints.vectorDims == null ? [position] : []));
      const target = [object.schema, object.name]
        .filter(Boolean)
        .map((part) => spec.quote(part!))
        .join(".");
      const selectList = kept.map((position) => spec.quote(page.columns[position]?.name ?? "")).join(", ");
      const statement = (vector: string) =>
        `SELECT ${spec.distance(vector)} AS ${spec.quote("vec_distance")}, ${selectList} FROM ${target} ORDER BY ${spec.distance(vector)} LIMIT ${k}`;
      return {
        result: {
          columns: [resultColumn("vec_distance", "double", "float"), ...kept.map((position) => page.columns[position]!)],
          rows: page.rows.map((row, position) => [{ t: "float", v: 0.05 + position * 0.031 }, ...kept.map((column) => row[column] ?? { t: "null", v: null })]),
        },
        plan: spec.plan,
        indexUsed: index.name,
        sql: statement(spec.placeholder),
        runnableSql: statement(spec.literal(sample)),
        elapsedMs: 4,
        queryDims: spec.dims,
        primaryKey: page.keyColumns,
      };
    },
    "vector.health": () => [
      { label: "Column", status: "info", value: spec.typeName, detail: "" },
      { label: "Vector index", status: "ok", value: `${index.name} · ${index.kind} · ${index.metric} · ${index.params}`, detail: "" },
      { label: "Zero vectors", status: "warn", value: "3", detail: "Cosine distance is undefined for all-zero vectors." },
    ],
    "vector.recall": (args) => ({
      index: index.name,
      metric: index.metric,
      k: Number(args.k ?? 10),
      queries: Number(args.queries ?? 20),
      recall: 0.97,
      worstRecall: 0.8,
      annMs: 1.4,
      exactMs: 9.8,
    }),
  };
}
