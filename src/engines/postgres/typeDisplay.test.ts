import { describe, expect, it } from "vitest";
import { enumLabels, shortType } from "./typeDisplay";

describe("PostgreSQL type display", () => {
  it("uses the short aliases and keeps modifiers", () => {
    expect(shortType("character varying(200)")).toBe("varchar(200)");
    expect(shortType("timestamp with time zone")).toBe("timestamptz");
    expect(shortType("numeric(12,2)")).toBe("numeric(12,2)");
    expect(shortType("double precision")).toBe("float8");
    expect(shortType("order_status")).toBe("order_status");
  });

  it("shows arrays as T[]", () => {
    expect(shortType("text[]")).toBe("text[]");
    expect(shortType("timestamp without time zone[]")).toBe("timestamp[]");
  });

  it("reads the labels of an enum", () => {
    expect(
      enumLabels([
        ["kind", "enum"],
        ["labels", "new, paid,shipped"],
      ]),
    ).toEqual(["new", "paid", "shipped"]);
    expect(enumLabels([["kind", "domain"]])).toEqual([]);
  });
});
