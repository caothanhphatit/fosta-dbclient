import { describe, expect, it } from "vitest";
import { clientErrorKind, isClientError, megabytes, progressText } from "./clientApi";
import { splitExtra } from "./definitionView";

describe("Oracle Instant Client prompt helpers", () => {
  it("recognises client errors by code or by the backend's message", () => {
    expect(isClientError({ code: "ORACLE_CLIENT", message: "No oci.dll" })).toBe(true);
    expect(isClientError({ code: "CONNECTION_ERROR", message: "Oracle Instant Client was not found. Install it." })).toBe(true);
    expect(isClientError({ code: "CONNECTION_ERROR", message: "ORA-12541: TNS:no listener" })).toBe(false);
    expect(clientErrorKind({ code: "ORACLE_CLIENT", message: "x", kind: "load" } as never)).toBe("load");
    expect(clientErrorKind({ code: "ORACLE_CLIENT", message: "x" })).toBe("notInstalled");
  });

  it("describes sizes and download progress", () => {
    expect(megabytes(60_406_433)).toBe("60 MB");
    expect(megabytes(10)).toBe("1 MB");
    expect(progressText({ phase: "downloading", downloaded: 30_000_000, total: 60_406_433 })).toBe("Downloading 30 MB of 60 MB");
    expect(progressText({ phase: "verifying", downloaded: 1, total: 1 })).toContain("checksum");
  });
});

describe("Oracle definition view", () => {
  it("splits compile errors from the facts and labels the facts", () => {
    const { facts, errors } = splitExtra([
      ["status", "INVALID"],
      ["lastDdl", "2026-11-08 10:02:45"],
      ["error1", "PROCEDURE line 4, column 1: PLS-00103"],
      ["error2", "PROCEDURE line 9, column 3: PLS-00201"],
      ["custom", "kept"],
    ]);
    expect(facts).toEqual([
      ["Status", "INVALID"],
      ["Last changed", "2026-11-08 10:02:45"],
      ["custom", "kept"],
    ]);
    expect(errors).toEqual(["PROCEDURE line 4, column 1: PLS-00103", "PROCEDURE line 9, column 3: PLS-00201"]);
  });
});
