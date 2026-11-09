import { describe, expect, it } from "vitest";
import type { VectorInfo } from "../../core/types";
import { columnMetrics, searchTakesFilter } from "./vectorRows";

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
