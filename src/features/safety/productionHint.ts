/**
 * Spots a connection that looks like a production database (its name, host, database or URL says prod / production / prd / live)
 * saved outside a Production workspace, so the app can offer to move it where it is read-only by default. Pure functions,
 * unit-tested; the dialog lives in main.ts.
 */
import type { ConnectionProfile, Workspace } from "../../core/types";
import { PRODUCTION_ICON } from "./safety";

const PRODUCTION_WORD = /(^|[^a-z0-9])(prod|production|prd|live)([^a-z0-9]|$)/i;
const DISMISSED_KEY = "fosta-studio-production-hint-dismissed";

export function looksLikeProduction(profile: Pick<ConnectionProfile, "name" | "host" | "database" | "url">): boolean {
  return [profile.name, profile.host, profile.database, profile.url].some((value) => !!value && PRODUCTION_WORD.test(value));
}

/** The workspace a production-looking connection would move to: the first one marked Production. */
export function productionWorkspace(workspaces: readonly Workspace[]): Workspace | undefined {
  return workspaces.find((workspace) => workspace.icon === PRODUCTION_ICON);
}

/** Whether to offer the move: the connection looks like production, is not in a Production workspace, and was not dismissed. */
export function shouldOfferProductionMove(profile: ConnectionProfile, current: Workspace, dismissed: ReadonlySet<string>): boolean {
  return current.icon !== PRODUCTION_ICON && !dismissed.has(profile.id) && looksLikeProduction(profile);
}

export function dismissedProductionHints(): Set<string> {
  try {
    const stored: unknown = JSON.parse(window.localStorage.getItem(DISMISSED_KEY) ?? "[]");
    return new Set(Array.isArray(stored) ? stored.filter((id): id is string => typeof id === "string") : []);
  } catch {
    return new Set();
  }
}

export function dismissProductionHint(connectionId: string): void {
  try {
    const ids = dismissedProductionHints();
    ids.add(connectionId);
    window.localStorage.setItem(DISMISSED_KEY, JSON.stringify([...ids]));
  } catch {
    // Storage may be unavailable (private window); the hint then simply shows again next time.
  }
}
