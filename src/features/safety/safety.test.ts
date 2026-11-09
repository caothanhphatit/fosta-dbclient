import { describe, expect, it } from "vitest";
import {
  commitsOpenTransaction,
  confirmationReason,
  connectionSafety,
  defaultSafety,
  effectiveRules,
  gatewayProblem,
  idleReminderDue,
  isRelaxed,
  newWorkspaceSafety,
  presetSettings,
  relaxations,
  riskExamples,
  transactionButtons,
  transactionLabel,
  typedMatches,
  unmarksLooserThanReadOnly,
  withoutConfirmation,
  writesBlocked,
} from "./safety";
import type { ConnectionProfile, SafetySettings, Workspace } from "../../core/types";

describe("presets", () => {
  it("map categories like the backend", () => {
    expect(effectiveRules(presetSettings("guarded")).rules).toEqual({ read: "allow", write: "confirm", ddl: "confirm", destructive: "typed", admin: "typed" });
    expect(effectiveRules(presetSettings("readOnly")).rules.write).toBe("block");
    expect(effectiveRules(presetSettings("open")).rules).toEqual({ read: "allow", write: "allow", ddl: "allow", destructive: "confirm", admin: "allow" });
    expect(effectiveRules(presetSettings("open")).statementTimeoutSecs).toBe(0);
    expect(effectiveRules(presetSettings("guarded")).statementTimeoutSecs).toBe(60);
  });

  it("don't ask again allows only the plain confirmations asked for", () => {
    const open = withoutConfirmation(presetSettings("open"), ["destructive"]);
    expect(open?.preset).toBe("custom");
    expect(open?.baseline).toBe("open");
    expect(open?.rules.destructive).toBe("allow");
    expect(withoutConfirmation(open!, ["destructive"])).toBeNull();
    const guarded = withoutConfirmation(presetSettings("guarded"), ["unknown", "destructive", "transaction"]);
    expect(guarded?.rules).toEqual({ read: "allow", write: "allow", ddl: "confirm", destructive: "typed", admin: "typed" });
    expect(guarded?.statementTimeoutSecs).toBe(60);
    expect(withoutConfirmation(presetSettings("readOnly"), ["write"])).toBeNull();
  });

  it("custom uses the stored matrix and clamps the row limit", () => {
    const custom = { ...presetSettings("custom"), maxRows: 10_000_000, statementTimeoutSecs: 5 };
    expect(effectiveRules(custom).maxRows).toBe(100_000);
    expect(effectiveRules(custom).statementTimeoutSecs).toBe(5);
  });

  it("defaults: existing workspaces Open, Production Read-only, new ones Guarded", () => {
    expect(defaultSafety("laptop").preset).toBe("open");
    expect(defaultSafety("shield").preset).toBe("readOnly");
    expect(newWorkspaceSafety("cloud").preset).toBe("guarded");
    expect(newWorkspaceSafety("shield").preset).toBe("readOnly");
  });
});

describe("relaxations", () => {
  it("lists every looser category and option with examples", () => {
    const changes = relaxations(presetSettings("guarded"), presetSettings("open"));
    expect(changes.map((change) => change.category)).toEqual(["write", "ddl", "destructive", "admin", null]);
    expect(changes[0].text).toBe("Write statements: confirm → allow");
    expect(riskExamples(changes)).toHaveLength(4);
    expect(riskExamples(changes)[2]).toContain("DELETE FROM orders");
  });

  it("tightening is not a relaxation", () => {
    expect(relaxations(presetSettings("open"), presetSettings("readOnly"))).toEqual([]);
  });

  it("removing the Production marking relaxes unless the rules stay Read-only", () => {
    expect(unmarksLooserThanReadOnly("shield", "laptop", presetSettings("guarded"))).toBe(true);
    expect(unmarksLooserThanReadOnly("shield", "laptop", presetSettings("readOnly"))).toBe(false);
    expect(unmarksLooserThanReadOnly("shield", "shield", presetSettings("open"))).toBe(false);
    expect(unmarksLooserThanReadOnly("laptop", "cloud", presetSettings("open"))).toBe(false);
    expect(unmarksLooserThanReadOnly(undefined, "laptop", presetSettings("open"))).toBe(false);
  });

  it("marks workspaces looser than their baseline", () => {
    expect(isRelaxed({ icon: "laptop", safety: defaultSafety("laptop") })).toBe(false);
    expect(isRelaxed({ icon: "shield", safety: presetSettings("guarded") })).toBe(true);
    expect(isRelaxed({ icon: "shield", safety: presetSettings("readOnly") })).toBe(false);
    expect(isRelaxed({ icon: "cloud", safety: { ...newWorkspaceSafety("cloud"), preset: "open" } })).toBe(true);
    const override = { ...defaultSafety("laptop"), preset: "open" as const };
    const profile = { id: "c", name: "Replica", driver: "mariadb" as const, host: "", port: 0, user: "", database: null, savePassword: false, options: {} };
    expect(isRelaxed({ icon: "shield", safety: presetSettings("readOnly"), connections: [{ ...profile, safety: override }] })).toBe(true);
  });
});

describe("gateway errors", () => {
  const payload = {
    code: "CONFIRMATION_REQUIRED",
    message: "Destructive statements need a typed confirmation",
    caller: "queryTab",
    workspace: "Production",
    connection: "Main",
    preset: "guarded",
    level: "typed",
    token: "abc",
    expected: "orders",
    categories: ["destructive"],
    tables: ["orders"],
    statements: [{ category: "destructive", kind: "DELETE", tables: ["orders"], excerpt: "DELETE FROM orders", reason: "DELETE without WHERE" }],
  };

  it("parses confirmation requests and ignores other errors", () => {
    const problem = gatewayProblem(payload);
    expect(problem?.token).toBe("abc");
    expect(gatewayProblem("plain error")).toBeNull();
    expect(gatewayProblem({ code: "PASSWORD_REQUIRED" })).toBeNull();
    expect(confirmationReason(problem!)).toBe("Destructive statements need a typed confirmation under the Guarded rules of workspace Production.");
  });

  it("checks the typed name exactly (trimmed)", () => {
    const problem = gatewayProblem(payload)!;
    expect(typedMatches(problem, " orders ")).toBe(true);
    expect(typedMatches(problem, "Orders")).toBe(false);
    expect(typedMatches({ ...problem, expected: null }, "")).toBe(true);
  });
});

describe("transactions", () => {
  it("labels the indicator honestly", () => {
    expect(transactionLabel({ supported: false, open: false, autoCommit: true })).toBe("Transactions: not supported");
    expect(transactionLabel({ supported: true, open: true, autoCommit: false })).toBe("Transaction open");
    expect(transactionLabel({ supported: true, open: false, autoCommit: true })).toBe("Auto-commit");
  });

  it("warns before DDL commits an open MySQL transaction", () => {
    const open = { supported: true, open: true, autoCommit: true };
    expect(commitsOpenTransaction("mariaDb", "ALTER TABLE t ADD c INT", open)).toBe(true);
    expect(commitsOpenTransaction("mySql", "UPDATE t SET a = 1;\n  create index i on t (a)", open)).toBe(true);
    expect(commitsOpenTransaction("mariaDb", "-- drop table t\nSELECT 1", open)).toBe(false);
    expect(commitsOpenTransaction("mariaDb", "UPDATE t SET note = 'drop'", open)).toBe(false);
    expect(commitsOpenTransaction("mariaDb", "DROP TABLE t", { ...open, open: false })).toBe(false);
    expect(commitsOpenTransaction("msSql", "DROP TABLE t", open)).toBe(false);
    expect(commitsOpenTransaction(null, "DROP TABLE t", open)).toBe(false);
    expect(commitsOpenTransaction("oracle", "BEGIN NULL; END", open)).toBe(false);
  });

  it("disables Begin while a transaction is open", () => {
    expect(transactionButtons({ supported: true, open: true, autoCommit: true })).toEqual({ begin: false, commit: true, rollback: true });
    expect(transactionButtons({ supported: true, open: false, autoCommit: false })).toEqual({ begin: true, commit: false, rollback: false });
    expect(transactionButtons(null)).toEqual({ begin: true, commit: false, rollback: false });
    expect(transactionButtons({ supported: false, open: false, autoCommit: true }).begin).toBe(false);
  });

  it("reminds about idle transactions every few minutes", () => {
    const minute = 60_000;
    expect(idleReminderDue(null, null, 10 * minute)).toBe(false);
    expect(idleReminderDue(0, null, 4 * minute)).toBe(false);
    expect(idleReminderDue(0, null, 5 * minute)).toBe(true);
    expect(idleReminderDue(0, 5 * minute, 7 * minute)).toBe(false);
    expect(idleReminderDue(0, 5 * minute, 10 * minute)).toBe(true);
  });
});

describe("read-only grids", () => {
  const profile = (id: string, safety?: SafetySettings): ConnectionProfile => ({
    id,
    name: id,
    driver: "mariadb",
    host: "h",
    port: 3306,
    user: "u",
    database: null,
    savePassword: false,
    options: {},
    safety,
  });
  const workspaces: Workspace[] = [
    {
      id: "prod",
      name: "Production",
      icon: "shield",
      color: "red",
      safety: presetSettings("readOnly"),
      connections: [profile("p"), profile("p-open", presetSettings("open"))],
      tasks: [],
    },
    { id: "dev", name: "Dev", icon: "laptop", color: "blue", safety: presetSettings("guarded"), connections: [profile("d")], tasks: [] },
  ];

  it("block editing where the workspace (or the connection) blocks writes", () => {
    expect(writesBlocked(connectionSafety(workspaces, "p"))).toBe(true);
    expect(writesBlocked(connectionSafety(workspaces, "p-open"))).toBe(false);
    expect(writesBlocked(connectionSafety(workspaces, "d"))).toBe(false);
    expect(writesBlocked(connectionSafety(workspaces, "unknown"))).toBe(false);
  });
});
