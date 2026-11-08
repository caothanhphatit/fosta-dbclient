import { describe, expect, it } from "vitest";
import conflictFixture from "./__fixtures__/contract/conflict-error.json";
import { describePosition, errorCode, errorText, formatError, positionOffset, toAppError } from "./errors";
import type { Diagnostics } from "./types";

describe("toAppError", () => {
  it("normalises strings, Errors and objects", () => {
    expect(toAppError("Signed out")).toEqual({ code: "ERROR", message: "Signed out" });
    expect(toAppError(new Error("boom"))).toEqual({ code: "ERROR", message: "boom" });
    expect(toAppError(Object.assign(new Error("Needs a confirmation"), { code: "CONFIRMATION_REQUIRED", token: "t" }))).toEqual({
      code: "CONFIRMATION_REQUIRED",
      message: "Needs a confirmation",
      token: "t",
    });
    expect(toAppError(undefined)).toEqual({ code: "ERROR", message: "Unknown error" });
    expect(toAppError(42)).toEqual({ code: "ERROR", message: "42" });
    expect(toAppError({ detail: "x" })).toEqual({ code: "ERROR", message: '{"detail":"x"}', detail: "x" });
  });

  it("keeps the backend's structured fields", () => {
    const error = toAppError({
      code: "ENGINE_ERROR",
      message: "You have an error in your SQL syntax",
      engine: "mariadb",
      sqlstate: "42000",
      vendorCode: 1064,
      position: { line: 1, column: 1, offset: 0 },
      statement: { index: 0, text: "SELEC 1" },
      retryable: false,
    });
    expect(error.code).toBe("ENGINE_ERROR");
    expect(error.position).toEqual({ line: 1, column: 1, offset: 0 });
    expect(error.statement?.text).toBe("SELEC 1");
  });

  it("drops malformed positions and statements", () => {
    const error = toAppError({ code: "X", message: "m", position: "line 3", statement: 7, cause: "io" });
    expect(error.position).toBeUndefined();
    expect(error.statement).toBeUndefined();
    expect(error.cause).toEqual(["io"]);
  });

  it("reads code and text like the old helpers", () => {
    expect(errorCode("text")).toBeNull();
    expect(errorCode({ code: "PASSWORD_REQUIRED", message: "x" })).toBe("PASSWORD_REQUIRED");
    expect(errorText("plain")).toBe("plain");
    expect(errorText({ code: "X", message: "structured" })).toBe("structured");
    expect(errorText(conflictFixture)).toBe((conflictFixture as { message: string }).message);
  });
});

describe("formatError", () => {
  const diagnostics: Diagnostics = {
    appVersion: "0.2.0",
    edition: "desktop",
    os: "windows",
    arch: "x86_64",
    osVersion: null,
    logFolder: null,
    debugLogging: false,
    connections: [{ engine: "mariadb", version: "11.8.2", crashed: false }],
    recentErrors: [],
  };

  it("lists every field and the diagnostics", () => {
    const text = formatError(
      {
        code: "ENGINE_ERROR",
        message: "syntax error",
        engine: "mariadb",
        sqlstate: "42000",
        vendorCode: 1064,
        detail: "near 'SELEC 1'",
        hint: "Check the keyword",
        position: { line: 2, column: 3, offset: 9 },
        statement: { index: 1, text: "SELEC 1" },
        cause: ["a", "b"],
        retryable: false,
        kind: "server",
      },
      { context: "Query on Local MariaDB", diagnostics },
    );
    expect(text).toContain("Error ENGINE_ERROR: syntax error");
    expect(text).toContain("While: Query on Local MariaDB");
    expect(text).toContain("Engine: mariadb");
    expect(text).toContain("Codes: SQLSTATE 42000 · vendor code 1064");
    expect(text).toContain("Detail: near 'SELEC 1'");
    expect(text).toContain("Hint: Check the keyword");
    expect(text).toContain("Position: line 2, column 3 (offset 9)");
    expect(text).toContain("Statement 2: SELEC 1");
    expect(text).toContain("Cause: a <- b");
    expect(text).toContain("Retryable: no");
    expect(text).toContain('"kind": "server"');
    expect(text).toContain("--- Diagnostics ---");
    expect(text).toContain("Fosta Studio 0.2.0 (desktop)");
    expect(text).toContain("Open connections: mariadb 11.8.2");
  });

  it("formats a plain string error", () => {
    expect(formatError("Cannot reach the server")).toBe("Error ERROR: Cannot reach the server");
  });
});

describe("positions", () => {
  it("describes a position", () => {
    expect(describePosition({ line: 2, column: 3, offset: 9 })).toBe("line 2, column 3 (offset 9)");
    expect(describePosition({ offset: 4 })).toBe("offset 4");
    expect(describePosition(undefined)).toBeNull();
  });

  it("maps a position to an offset in the text", () => {
    const text = "SELECT\n  x FORM t";
    expect(positionOffset(text, { offset: 11 })).toBe(11);
    expect(positionOffset(text, { line: 2, column: 5 })).toBe(11);
    expect(positionOffset(text, { line: 2, column: 99 })).toBe(text.length);
    expect(positionOffset(text, { line: 5, column: 1 })).toBeNull();
    expect(positionOffset(text, { offset: 99 })).toBeNull();
  });
});
