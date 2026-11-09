import { describe, expect, it } from "vitest";
import { changesSchema, defaultDatabase, hasUnsavedText } from "./queryRules";

describe("defaultDatabase", () => {
  it("keeps the database asked for", () => {
    expect(defaultDatabase("shop", ["analytics", "shop"], true)).toBe("shop");
  });

  it("picks the only database, or the first one of an engine without separate databases", () => {
    expect(defaultDatabase(null, ["shop"], true)).toBe("shop");
    expect(defaultDatabase(null, ["main", "temp", "archive"], false)).toBe("main");
    expect(defaultDatabase(null, ["analytics", "shop"], true)).toBeNull();
    expect(defaultDatabase(null, [], false)).toBeNull();
  });
});

describe("changesSchema", () => {
  it("spots DDL", () => {
    expect(changesSchema("CREATE TABLE t (id int)")).toBe(true);
    expect(changesSchema("select 1;\nalter table t add column x int")).toBe(true);
    expect(changesSchema("DROP VIEW v")).toBe(true);
    expect(changesSchema('{ "createIndexes": "orders", "indexes": [] }')).toBe(true);
  });

  it("ignores reads, writes and DDL words in comments", () => {
    expect(changesSchema("SELECT * FROM orders")).toBe(false);
    expect(changesSchema("UPDATE t SET created_at = now()")).toBe(false);
    expect(changesSchema("-- drop table later\nSELECT 1")).toBe(false);
    expect(changesSchema("/* create */ SELECT 1")).toBe(false);
  });
});

describe("hasUnsavedText", () => {
  it("is false for an empty tab and for text that ran (or that the tab opened with)", () => {
    expect(hasUnsavedText("", "")).toBe(false);
    expect(hasUnsavedText("  \n", "")).toBe(false);
    expect(hasUnsavedText("SELECT 1", "SELECT 1")).toBe(false);
  });

  it("is true for text never run, or edited since it ran", () => {
    expect(hasUnsavedText("SELECT 1", "")).toBe(true);
    expect(hasUnsavedText("SELECT 12", "SELECT 1")).toBe(true);
  });
});
