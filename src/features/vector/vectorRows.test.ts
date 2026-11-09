import { describe, expect, it } from "vitest";
import type { ColumnInfo, Value } from "../../core/contract";
import type { VectorInfo } from "../../core/types";
import { columnMetrics, keyCells, searchTakesFilter } from "./vectorRows";

describe("columnMetrics", () => {
  const info: VectorInfo = {
    supported: true,
    columns: [
      { name: "embedding", dims: 4, typeName: "dense_vector(4)", metrics: ["cosine"] },
      { name: "raw", dims: 4, typeName: "dense_vector(4)", metrics: [] },
      { name: "plain", dims: 4, typeName: "vector(4)" },
    ],
    indexes: [],
    metrics: ["cosine", "euclidean", "dot"],
  };

  it("offers only the metrics of a column whose similarity is fixed", () => {
    expect(columnMetrics(info, "embedding")).toEqual(["cosine"]);
  });

  it("falls back to the engine's metrics", () => {
    expect(columnMetrics(info, "raw")).toEqual(["cosine", "euclidean", "dot"]);
    expect(columnMetrics(info, "plain")).toEqual(["cosine", "euclidean", "dot"]);
    expect(columnMetrics(info, "missing")).toEqual(["cosine", "euclidean", "dot"]);
  });
});

describe("searchTakesFilter", () => {
  it("follows the engine's raw filter language", () => {
    expect(searchTakesFilter({ filter: { structured: true, raw: "sql", placeholder: "WHERE" } })).toBe(true);
    expect(searchTakesFilter({ filter: { structured: false, raw: "esQueryDsl", placeholder: "Query DSL" } })).toBe(true);
    expect(searchTakesFilter({ filter: { structured: false, raw: null, placeholder: "Key pattern, e.g. user:*" } })).toBe(false);
  });
});

describe("keyCells", () => {
  const column = (name: string, vectorDims?: number) => ({ name, hints: vectorDims == null ? {} : { vectorDims } }) as unknown as ColumnInfo;
  const columns = [column("vec_distance"), column("id"), column("label"), column("embedding", 4)];
  const row: Value[] = [
    { t: "float", v: 0.1 },
    { t: "int", v: 7 },
    { t: "text", v: "seven" },
    { t: "text", v: "[4] …" },
  ];

  it("takes the key columns", () => {
    expect(keyCells(columns, row, ["id"])).toEqual([{ column: "id", value: { t: "int", v: 7 } }]);
  });

  it("falls back to every stored non-vector column without a key, never the distance", () => {
    expect(keyCells(columns, row, []).map((cell) => cell.column)).toEqual(["id", "label"]);
  });
});
