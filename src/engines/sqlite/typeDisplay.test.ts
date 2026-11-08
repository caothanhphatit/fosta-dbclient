import { describe, expect, it } from "vitest";
import { affinity, typeLabel } from "./typeDisplay";

describe("SQLite type display", () => {
  it("applies SQLite's affinity rules in order", () => {
    expect(affinity("INTEGER")).toBe("INTEGER");
    expect(affinity("BIGINT")).toBe("INTEGER");
    expect(affinity("VARCHAR(100)")).toBe("TEXT");
    expect(affinity("CLOB")).toBe("TEXT");
    expect(affinity("BLOB")).toBe("BLOB");
    expect(affinity("DOUBLE PRECISION")).toBe("REAL");
    expect(affinity("DECIMAL(12,2)")).toBe("NUMERIC");
    expect(affinity("BOOLEAN")).toBe("NUMERIC");
    // "CHARINT" contains INT, which wins over CHAR.
    expect(affinity("CHARINT")).toBe("INTEGER");
  });

  it("labels untyped columns as any", () => {
    expect(typeLabel("")).toBe("any");
    expect(typeLabel("DATE")).toBe("DATE · NUMERIC");
  });
});
