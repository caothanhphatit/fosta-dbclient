import { describe, expect, it } from "vitest";
import type { StatementCategory } from "../../core/types";
import { lostSessionMessage, onlyReads, recoverLostSession, sessionGone, type LostSessionSteps } from "./lostSession";

const lost = { code: "SESSION_LOST", message: "This query tab lost its session" };
const notConnected = { code: "NOT_CONNECTED", message: "Not connected" };

/** Steps that record what ran: reopening and classifying. */
function steps(categories: StatementCategory[] | Error, overrides: Partial<LostSessionSteps> = {}) {
  const calls: string[] = [];
  const value: LostSessionSteps = {
    received: false,
    hadTransaction: false,
    reopen: () => {
      calls.push("reopen");
      return Promise.resolve();
    },
    classify: () => {
      calls.push("classify");
      return categories instanceof Error ? Promise.reject(categories) : Promise.resolve(categories);
    },
    ...overrides,
  };
  return { value, calls };
}

describe("recoverLostSession", () => {
  it("runs again on its own when every statement reads", async () => {
    const { value, calls } = steps(["read", "read"]);
    expect(await recoverLostSession(lost, value)).toBe("retry");
    expect(calls).toEqual(["reopen", "classify"]);
    expect(await recoverLostSession(notConnected, steps(["read"]).value)).toBe("retry");
  });

  it("reopens but does not run anything that does more than read", async () => {
    for (const categories of [["read", "write"], ["ddl"], ["transaction"], ["unknown"], []] as StatementCategory[][]) {
      const { value, calls } = steps(categories);
      expect(await recoverLostSession(lost, value)).toBe("sessionReset");
      expect(calls).toEqual(["reopen", "classify"]);
    }
  });

  it("does not run when the new session cannot classify the text", async () => {
    expect(await recoverLostSession(lost, steps(new Error("gone again")).value)).toBe("sessionReset");
  });

  it("reopens but does not run when the lost session had an open transaction", async () => {
    const { value, calls } = steps(["read"], { hadTransaction: true });
    expect(await recoverLostSession(lost, value)).toBe("transactionLost");
    expect(calls).toEqual(["reopen"]);
  });

  it("keeps the error when rows had already arrived", async () => {
    const { value, calls } = steps(["read"], { received: true });
    expect(await recoverLostSession(lost, value)).toBe("rethrow");
    expect(calls).toEqual([]);
  });

  it("keeps any other error", async () => {
    const { value, calls } = steps(["read"]);
    expect(await recoverLostSession({ code: "ENGINE_ERROR", message: "syntax error" }, value)).toBe("rethrow");
    expect(await recoverLostSession("plain text", value)).toBe("rethrow");
    expect(calls).toEqual([]);
  });

  it("fails when the session cannot be reopened", async () => {
    const { value } = steps(["read"], { reopen: () => Promise.reject(notConnected) });
    await expect(recoverLostSession(lost, value)).rejects.toBe(notConnected);
  });
});

describe("helpers", () => {
  it("knows a lost session and a read-only run", () => {
    expect(sessionGone(lost)).toBe(true);
    expect(sessionGone(notConnected)).toBe(true);
    expect(sessionGone(new Error("x"))).toBe(false);
    expect(onlyReads(["read"])).toBe(true);
    expect(onlyReads([])).toBe(false);
    expect(onlyReads(["read", "admin"])).toBe(false);
  });

  it("says what was reset", () => {
    expect(lostSessionMessage("sessionReset")).toMatch(/SET ROLE, search_path/);
    expect(lostSessionMessage("sessionReset")).toMatch(/press Run again/);
    expect(lostSessionMessage("transactionLost")).toMatch(/rolled back/);
  });
});
