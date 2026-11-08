/**
 * Safety rules on the UI side: preset tables that mirror `gateway/rules.rs`, the
 * "relaxed" check behind the warning badges, the warnings shown when rules are relaxed,
 * and parsing of the gateway's structured errors. Pure functions, unit-tested.
 */
import type {
  CategoryRules,
  GatewayProblem,
  SafetyAction,
  SafetyPreset,
  SafetySettings,
  StatementCategory,
  TransactionState,
  Workspace,
  WorkspaceMeta,
} from "../../core/types";

export const PRODUCTION_ICON = "shield";
const DEFAULT_MAX_ROWS = 1000;
const GUARDED_TIMEOUT_SECS = 60;

export const PRESETS: SafetyPreset[] = ["open", "guarded", "readOnly", "custom"];
export const RULE_CATEGORIES: (keyof CategoryRules)[] = ["read", "write", "ddl", "destructive", "admin"];
export const ACTIONS: SafetyAction[] = ["allow", "confirm", "typed", "block"];

export const PRESET_LABELS: Record<SafetyPreset, string> = { open: "Open", guarded: "Guarded", readOnly: "Read-only", custom: "Custom" };

export const PRESET_DESCRIPTIONS: Record<SafetyPreset, string> = {
  open: "Everything runs without asking.",
  guarded: "Writes and DDL need a confirmation; destructive and administrative statements need the table or workspace name typed. 60 s statement timeout.",
  readOnly: "Only reads run. Sessions are read-only where the engine supports it. 60 s statement timeout.",
  custom: "Choose per category, plus multi-statement runs, timeout and row limit.",
};

export const CATEGORY_LABELS: Record<StatementCategory, string> = {
  read: "Read",
  transaction: "Transaction",
  write: "Write",
  unknown: "Unparsed",
  ddl: "DDL",
  destructive: "Destructive",
  admin: "Administrative",
};

export const CATEGORY_HINTS: Record<keyof CategoryRules, string> = {
  read: "SELECT, SHOW, EXPLAIN, find, aggregate",
  write: "INSERT, UPDATE, DELETE with WHERE, MERGE, REPLACE, grid saves, imports",
  ddl: "CREATE, ALTER, RENAME, createIndexes",
  destructive: "DROP, TRUNCATE, UPDATE/DELETE without WHERE, ALTER … DROP",
  admin: "GRANT, REVOKE, users, KILL, SET GLOBAL",
};

export const ACTION_LABELS: Record<SafetyAction, string> = { allow: "Allow", confirm: "Confirm", typed: "Typed confirm", block: "Block" };

const STRICTNESS: Record<SafetyAction, number> = { allow: 0, confirm: 1, typed: 2, block: 3 };

const PRESET_RULES: Record<Exclude<SafetyPreset, "custom">, CategoryRules> = {
  open: { read: "allow", write: "allow", ddl: "allow", destructive: "allow", admin: "allow" },
  guarded: { read: "allow", write: "confirm", ddl: "confirm", destructive: "typed", admin: "typed" },
  readOnly: { read: "allow", write: "block", ddl: "block", destructive: "block", admin: "block" },
};

/** What a concrete statement in each category could do when it runs unchecked. */
const RISK_EXAMPLES: Record<keyof CategoryRules, string> = {
  read: "SELECT * FROM a large table can load the server and hold locks while it streams.",
  write: "UPDATE orders SET status = 'paid' WHERE id = 7 — a typo in the id changes the wrong order without anyone being asked.",
  ddl: "ALTER TABLE orders ADD INDEX … can lock a busy table for minutes; CREATE TABLE … AS SELECT copies data around.",
  destructive: "DELETE FROM orders (no WHERE) or DROP TABLE customers removes every row at once, and nothing asks you to type a name first.",
  admin: "GRANT ALL ON *.* TO 'app'@'%', KILL <thread> or SET GLOBAL … change the whole server for everyone.",
};

export interface EffectiveRules {
  rules: CategoryRules;
  allowMultiStatement: boolean;
  statementTimeoutSecs: number;
  maxRows: number;
}

export function presetSettings(preset: SafetyPreset, base?: SafetySettings): SafetySettings {
  return {
    preset,
    rules: base?.rules ?? { ...PRESET_RULES.guarded },
    allowMultiStatement: base?.allowMultiStatement ?? true,
    statementTimeoutSecs: base?.statementTimeoutSecs ?? 0,
    maxRows: base?.maxRows ?? DEFAULT_MAX_ROWS,
    baseline: base?.baseline ?? preset,
  };
}

/** The default for a workspace without saved rules: Production is Read-only, others keep working as before (Open). */
export function defaultSafety(icon: string): SafetySettings {
  return presetSettings(icon === PRODUCTION_ICON ? "readOnly" : "open");
}

/** New workspaces start Guarded (Read-only when created as Production). */
export function newWorkspaceSafety(icon: string): SafetySettings {
  return presetSettings(icon === PRODUCTION_ICON ? "readOnly" : "guarded");
}

export function safetyOf(workspace: Pick<WorkspaceMeta, "icon" | "safety">): SafetySettings {
  return workspace.safety ?? defaultSafety(workspace.icon);
}

/** The rules a connection runs under: its own override, else its workspace's. Null when it is not in any workspace. */
export function connectionSafety(workspaces: readonly Workspace[], connectionId: string): SafetySettings | null {
  for (const workspace of workspaces) {
    const profile = workspace.connections.find((item) => item.id === connectionId);
    if (profile) return profile.safety ?? safetyOf(workspace);
  }
  return null;
}

/** True when the rules block every write (Read-only), so grids should not offer editing at all. */
export function writesBlocked(settings: SafetySettings | null): boolean {
  return settings !== null && effectiveRules(settings).rules.write === "block";
}

export function effectiveRules(settings: SafetySettings): EffectiveRules {
  if (settings.preset === "custom") {
    return {
      rules: settings.rules,
      allowMultiStatement: settings.allowMultiStatement,
      statementTimeoutSecs: settings.statementTimeoutSecs,
      maxRows: Math.min(Math.max(settings.maxRows, 1), 100_000),
    };
  }
  return {
    rules: PRESET_RULES[settings.preset],
    allowMultiStatement: true,
    statementTimeoutSecs: settings.preset === "open" ? 0 : GUARDED_TIMEOUT_SECS,
    maxRows: DEFAULT_MAX_ROWS,
  };
}

export interface Relaxation {
  /** A category whose action got looser, or null for an option. */
  category: keyof CategoryRules | null;
  text: string;
}

/** Every way `next` is less safe than `previous` (mirrors `relaxations` in gateway/rules.rs). */
export function relaxations(previous: SafetySettings, next: SafetySettings): Relaxation[] {
  const before = effectiveRules(previous);
  const after = effectiveRules(next);
  const changes: Relaxation[] = [];
  for (const category of RULE_CATEGORIES) {
    if (STRICTNESS[after.rules[category]] < STRICTNESS[before.rules[category]]) {
      changes.push({
        category,
        text: `${CATEGORY_LABELS[category]} statements: ${ACTION_LABELS[before.rules[category]].toLowerCase()} → ${ACTION_LABELS[after.rules[category]].toLowerCase()}`,
      });
    }
  }
  if (after.allowMultiStatement && !before.allowMultiStatement) changes.push({ category: null, text: "Several statements per run allowed" });
  const looserTimeout = before.statementTimeoutSecs > 0 && (after.statementTimeoutSecs === 0 || after.statementTimeoutSecs > before.statementTimeoutSecs);
  if (looserTimeout)
    changes.push({
      category: null,
      text: `Statement timeout: ${timeoutLabel(before.statementTimeoutSecs)} → ${timeoutLabel(after.statementTimeoutSecs)} (a runaway query keeps running longer)`,
    });
  if (after.maxRows > before.maxRows) changes.push({ category: null, text: `Row limit: ${before.maxRows} → ${after.maxRows}` });
  return changes;
}

/** A concrete example of what could go wrong for each relaxed category. */
export function riskExamples(changes: Relaxation[]): string[] {
  return changes.flatMap((change) => (change.category ? [RISK_EXAMPLES[change.category]] : []));
}

function timeoutLabel(seconds: number): string {
  return seconds === 0 ? "none" : `${seconds} s`;
}

/** The setting a workspace is compared against for the "relaxed" badge. */
export function baselineOf(workspace: Pick<WorkspaceMeta, "icon" | "safety">): SafetyPreset {
  if (workspace.icon === PRODUCTION_ICON) return "readOnly";
  return workspace.safety?.baseline ?? "open";
}

/** Ways a workspace (or one of its connections) is looser than its baseline; empty when it is not relaxed. */
export function relaxedFromBaseline(workspace: Pick<Workspace, "icon" | "safety"> & { connections?: Workspace["connections"] }): Relaxation[] {
  const baseline = presetSettings(baselineOf(workspace));
  const own = relaxations(baseline, safetyOf(workspace));
  const overrides = (workspace.connections ?? []).flatMap((profile) =>
    profile.safety ? relaxations(baseline, profile.safety).map((change) => ({ ...change, text: `${profile.name}: ${change.text}` })) : [],
  );
  return [...own, ...overrides];
}

export function isRelaxed(workspace: Pick<Workspace, "icon" | "safety"> & { connections?: Workspace["connections"] }): boolean {
  return relaxedFromBaseline(workspace).length > 0;
}

/** Short status text such as "Guarded" or "Read-only". */
export function safetyLabel(settings: SafetySettings): string {
  return PRESET_LABELS[settings.preset];
}

/** The gateway's structured error, when `error` is one. */
export function gatewayProblem(error: unknown): GatewayProblem | null {
  if (typeof error !== "object" || error === null) return null;
  const code = (error as { code?: unknown }).code;
  if (code !== "CONFIRMATION_REQUIRED" && code !== "BLOCKED") return null;
  const problem = error as Partial<GatewayProblem>;
  return {
    code,
    message: problem.message ?? "",
    caller: problem.caller ?? "queryTab",
    workspace: problem.workspace ?? "",
    connection: problem.connection ?? "",
    preset: problem.preset ?? "custom",
    level: problem.level ?? (code === "BLOCKED" ? "block" : "confirm"),
    token: problem.token ?? null,
    expected: problem.expected ?? null,
    categories: problem.categories ?? [],
    tables: problem.tables ?? [],
    statements: problem.statements ?? [],
  };
}

/** Whether the typed text satisfies a typed confirmation (same rule as the backend: trimmed, exact). */
export function typedMatches(problem: GatewayProblem, typed: string): boolean {
  return problem.expected === null || typed.trim() === problem.expected;
}

/** Why the dialog asks, in one sentence. */
export function confirmationReason(problem: GatewayProblem): string {
  const categories = problem.categories.filter((category) => category !== "read" && category !== "transaction").map((category) => CATEGORY_LABELS[category]);
  const what = categories.length > 0 ? categories.join(", ") : "These";
  const rules = `${PRESET_LABELS[problem.preset]} rules of workspace ${problem.workspace}`;
  return problem.level === "typed"
    ? `${what} statements need a typed confirmation under the ${rules}.`
    : `${what} statements need a confirmation under the ${rules}.`;
}

/** Text of the query toolbar's transaction indicator. */
export function transactionLabel(state: TransactionState | null): string {
  if (!state) return "No transaction";
  if (!state.supported) return "Transactions: not supported";
  if (state.open) return "Transaction open";
  return state.autoCommit ? "Auto-commit" : "No transaction (manual commit)";
}

/**
 * Which transaction buttons are usable. Begin is disabled while a transaction is open: on MySQL/MariaDB a second
 * START TRANSACTION silently commits the open one, and on SQL Server it nests.
 */
export function transactionButtons(state: TransactionState | null): { begin: boolean; commit: boolean; rollback: boolean } {
  const open = state?.open === true;
  const supported = state?.supported !== false;
  return { begin: supported && !open, commit: open, rollback: open };
}

/** Minutes after which an open transaction triggers a reminder. */
export const IDLE_TRANSACTION_MINUTES = 5;

/** Whether an idle reminder is due for a transaction open since `openedAt` (ms) and last reminded at `remindedAt`. */
export function idleReminderDue(openedAt: number | null, remindedAt: number | null, now: number, minutes = IDLE_TRANSACTION_MINUTES): boolean {
  if (openedAt === null) return false;
  const period = minutes * 60_000;
  const since = remindedAt ?? openedAt;
  return now - since >= period;
}
