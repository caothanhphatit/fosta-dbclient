import { describe, expect, it } from "vitest";
import { creation } from "./keysView";
import {
  blobBytes,
  blobDisplay,
  blobFromBytes,
  conflictOf,
  describeCurrent,
  formatBytes,
  formatMicros,
  formatTtl,
  keyBlob,
  keyIdentity,
  matchPattern,
  parseTtl,
  sameBlob,
  splitKey,
  typeName,
  type RedisConflict,
} from "./model";
import { commandLine, hitRate } from "./serverView";
import { validStreamId } from "./streamEditor";
import { isCanonicalJson } from "./valueEditors";

describe("bytes", () => {
  it("shows text as is and binary as hex", () => {
    expect(blobDisplay({ text: "user:1" })).toBe("user:1");
    expect(blobDisplay({ base64: "/wBB" })).toBe("0xff0041");
    expect(blobDisplay(null)).toBe("");
  });

  it("round-trips bytes through the text or base64 form", () => {
    expect(blobFromBytes(new TextEncoder().encode("héllo"))).toEqual({ text: "héllo" });
    const binary = blobFromBytes(new Uint8Array([0xff, 0x00, 0x41]));
    expect(binary).toEqual({ base64: "/wBB" });
    expect([...blobBytes(binary)]).toEqual([0xff, 0x00, 0x41]);
  });

  it("names keys for the tree: base64 for binary", () => {
    expect(keyIdentity({ text: "a b" })).toEqual({ name: "a b", binary: false });
    expect(keyIdentity({ base64: "/wBB" })).toEqual({ name: "/wBB", binary: true });
    expect(keyBlob("/wBB", true)).toEqual({ base64: "/wBB" });
    expect(keyBlob("a", false)).toEqual({ text: "a" });
    expect(sameBlob({ text: "a" }, { text: "a" })).toBe(true);
    expect(sameBlob({ text: "a" }, { base64: "YQ==" })).toBe(false);
    expect(sameBlob(null, undefined)).toBe(true);
  });
});

describe("ttl", () => {
  it("parses a number with an optional unit, seconds by default", () => {
    expect(parseTtl("90")).toBe(90_000);
    expect(parseTtl("90s")).toBe(90_000);
    expect(parseTtl("10m")).toBe(600_000);
    expect(parseTtl("1.5h")).toBe(5_400_000);
    expect(parseTtl("2d")).toBe(172_800_000);
    expect(parseTtl("500ms")).toBe(500);
    expect(parseTtl(" 5 M ")).toBe(300_000);
  });

  it("refuses anything else, zero included", () => {
    for (const text of ["", "abc", "-5", "0", "10x", "1e3", "5 minutes"]) expect(parseTtl(text), text).toBeNull();
  });

  it("formats readable lifetimes", () => {
    expect(formatTtl(-1)).toBe("no expiry");
    expect(formatTtl(-2)).toBe("missing");
    expect(formatTtl(400)).toBe("400 ms");
    expect(formatTtl(59_000)).toBe("59s");
    expect(formatTtl(600_000)).toBe("10m");
    expect(formatTtl(3_661_000)).toBe("61m 1s");
    expect(formatTtl(10_800_000)).toBe("3h");
    expect(formatTtl(900_000_000)).toBe("10d 10h");
    expect(formatTtl(null)).toBe("");
  });
});

describe("key list", () => {
  it("matches plain text anywhere and keeps globs", () => {
    expect(matchPattern("")).toBeUndefined();
    expect(matchPattern("  ")).toBeUndefined();
    expect(matchPattern("user")).toBe("*user*");
    expect(matchPattern("user:*")).toBe("user:*");
    expect(matchPattern("h?llo")).toBe("h?llo");
    expect(matchPattern("[ab]c")).toBe("[ab]c");
  });

  it("splits a key at the last colon", () => {
    expect(splitKey("users:42:profile")).toEqual({ prefix: "users:42:", leaf: "profile" });
    expect(splitKey("leaderboard")).toEqual({ prefix: "", leaf: "leaderboard" });
    expect(splitKey("a/b/c", "/")).toEqual({ prefix: "a/b/", leaf: "c" });
  });

  it("names types", () => {
    expect(typeName("zset")).toBe("zset");
    expect(typeName("timeSeries")).toBe("timeseries");
    expect(typeName({ other: "MyModule" })).toBe("MyModule");
    expect(typeName(null)).toBe("");
  });
});

describe("numbers", () => {
  it("formats sizes and durations", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KiB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MiB");
    expect(formatBytes(null)).toBe("");
    expect(formatMicros(850)).toBe("850 µs");
    expect(formatMicros(12_400)).toBe("12.4 ms");
    expect(formatMicros(1_200_000)).toBe("1.20 s");
  });

  it("computes the hit rate and shortens slow log commands", () => {
    expect(hitRate(90, 10)).toBe("90.0%");
    expect(hitRate(0, 0)).toBe("");
    expect(hitRate(null, null)).toBe("");
    expect(commandLine(["SET", "a b", "x"])).toBe('SET "a b" x');
    expect(commandLine(["GET", "k".repeat(300)], 20)).toBe(`GET ${"k".repeat(16)}…`);
  });
});

describe("conflicts", () => {
  const conflict = (current: RedisConflict["current"]): RedisConflict => ({
    code: "CONFLICT",
    message: "Nothing was saved",
    key: { text: "k" },
    expected: { kind: "string", value: { text: "a" } },
    current,
  });

  it("recognises the structured CONFLICT error and nothing else", () => {
    expect(conflictOf(conflict(null))).not.toBeNull();
    expect(conflictOf({ code: "ENGINE_ERROR", message: "x" })).toBeNull();
    expect(conflictOf({ code: "CONFLICT", message: "a row conflict without expected" })).toBeNull();
    expect(conflictOf("text")).toBeNull();
    expect(conflictOf(null)).toBeNull();
  });

  it("says what the key holds now", () => {
    expect(describeCurrent(conflict({ kind: "string", value: { text: "b" } }))).toBe("Now: b");
    expect(describeCurrent(conflict({ kind: "string", value: null }))).toBe("The key no longer exists.");
    expect(describeCurrent(conflict({ kind: "hash", fields: [{ field: { text: "f" }, value: { text: "1" } }, { field: { text: "g" } }] }))).toBe(
      "f = 1, g = (missing)",
    );
    expect(describeCurrent(conflict({ kind: "listItem", index: 2, value: null }))).toBe("Item 2 no longer exists.");
    expect(describeCurrent(conflict({ kind: "set", members: [{ member: { text: "m" }, present: false }] }))).toBe("m is not in the set");
    expect(describeCurrent(conflict({ kind: "zset", members: [{ member: { text: "m" }, score: 2.5 }] }))).toBe("m: 2.5");
    expect(describeCurrent(conflict({ kind: "exists" }))).toBe("The key already exists.");
    expect(describeCurrent(conflict(null))).toContain("changed while it was being saved");
  });
});

describe("creating keys", () => {
  it("builds the edit for each type and refuses missing input", () => {
    expect(creation("string", "k", "v", "")).toMatchObject({ op: "setString", condition: "ifMissing", key: { text: "k" } });
    expect(creation("hash", "k", "f", "v")).toMatchObject({ op: "hashSet", fields: [{ field: { text: "f" }, value: { text: "v" } }] });
    expect(creation("list", "k", "a", "")).toMatchObject({ op: "listPush", end: "tail" });
    expect(creation("set", "k", "a", "")).toMatchObject({ op: "setAdd" });
    expect(creation("zset", "k", "a", "2.5")).toMatchObject({ op: "zsetAdd", members: [{ score: 2.5 }] });
    expect(creation("stream", "k", "f", "v")).toMatchObject({ op: "streamAdd", id: null });
    expect(typeof creation("hash", "k", "", "v")).toBe("string");
    expect(typeof creation("zset", "k", "a", "x")).toBe("string");
    expect(typeof creation("zset", "k", "a", "")).toBe("string");
    expect(typeof creation("", "k", "a", "")).toBe("string");
  });
});

describe("pretty JSON", () => {
  it("is offered only where re-serialising gives back the stored text", () => {
    expect(isCanonicalJson('{"a":1,"b":[1,2]}')).toBe(true);
    expect(isCanonicalJson("[]")).toBe(true);
    expect(isCanonicalJson('{ "a": 1 }')).toBe(false);
    expect(isCanonicalJson('{"id":12345678901234567890}')).toBe(false);
    expect(isCanonicalJson('{"a":1.0}')).toBe(false);
    expect(isCanonicalJson("{broken")).toBe(false);
  });
});

describe("stream ids", () => {
  it("accepts automatic ids, times and time-sequence pairs", () => {
    for (const id of ["", "*", "1712345678901", "1712345678901-0", "1712345678901-*"]) expect(validStreamId(id), id).toBe(true);
    for (const id of ["abc", "-1", "1-2-3", "1-"]) expect(validStreamId(id), id).toBe(false);
  });
});
