/**
 * A rough stand-in for the backend's statement gateway (`src-tauri/src/gateway`): applies the workspace's safety rules
 * to classified statements and throws `BLOCKED` or `CONFIRMATION_REQUIRED` like the backend (any token is accepted back).
 */
import type { AppConfig, Caller, Confirmation, GatewayProblem, SafetyAction, StatementCategory, StatementInfo } from "../../core/types";
import { effectiveRules, safetyOf } from "../../features/safety/safety";

const STRICTNESS: Record<SafetyAction, number> = { allow: 0, confirm: 1, typed: 2, block: 3 };

export function statementInfo(category: StatementCategory, kind: string, tables: string[], excerpt: string, reason: string | null = null): StatementInfo {
  return { category, kind, tables, excerpt, reason };
}

function problemOf(
  config: AppConfig,
  connectionId: string,
  caller: Caller,
  level: SafetyAction,
  statements: StatementInfo[],
  message: string,
): GatewayProblem | null {
  const workspace = config.workspaces.find((item) => item.connections.some((profile) => profile.id === connectionId));
  if (!workspace) return null;
  const settings = safetyOf(workspace);
  const tables = [...new Set(statements.flatMap((item) => item.tables))];
  return {
    code: level === "block" ? "BLOCKED" : "CONFIRMATION_REQUIRED",
    message,
    caller,
    workspace: workspace.name,
    connection: workspace.connections.find((profile) => profile.id === connectionId)?.name ?? connectionId,
    preset: settings.preset,
    level,
    token: level === "block" ? null : `mock-${Date.now().toString(16)}`,
    expected: level === "typed" ? (tables.length === 1 ? (tables[0].split(".").pop() ?? workspace.name) : workspace.name) : null,
    categories: [...new Set(statements.map((item) => item.category))],
    tables,
    statements,
  };
}

/** Throws the gateway's structured error when the rules need a confirmation (none given) or block the statements. */
export function gate(
  config: AppConfig,
  connectionId: string,
  caller: Caller,
  statements: StatementInfo[],
  confirmation: Confirmation | null | undefined,
): void {
  const workspace = config.workspaces.find((item) => item.connections.some((profile) => profile.id === connectionId));
  if (!workspace) return;
  const rules = effectiveRules(safetyOf(workspace)).rules;
  const actionOf = (category: StatementCategory): SafetyAction =>
    caller === "transaction" || category === "transaction" ? "allow" : category === "unknown" ? rules.write : rules[category as keyof typeof rules];
  const level = statements
    .map((item) => actionOf(item.category))
    .reduce<SafetyAction>((worst, next) => (STRICTNESS[next] > STRICTNESS[worst] ? next : worst), "allow");
  if (level === "allow") return;
  if (level !== "block" && confirmation) return;
  const message =
    level === "block"
      ? `Blocked: these statements are not allowed by the rules of workspace ${workspace.name}.`
      : `These statements need a ${level === "typed" ? "typed " : ""}confirmation in workspace ${workspace.name}.`;
  throw problemOf(config, connectionId, caller, level, statements, message);
}

/** An engine operation the engine does not classify: refused whatever the rules (like `gateway::invoke`). */
export function refuseOperation(config: AppConfig, connectionId: string, name: string): never {
  const statements = [statementInfo("unknown", name, [], name, "unknown operation")];
  throw (
    problemOf(config, connectionId, "engineView" as Caller, "block", statements, "Refused: unknown operation.") ?? {
      code: "BLOCKED",
      message: "Refused: unknown operation.",
    }
  );
}
