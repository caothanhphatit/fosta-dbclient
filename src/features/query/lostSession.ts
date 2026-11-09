/**
 * What a query tab does when a run fails because the backend lost its session (`SESSION_LOST`) or its connection
 * (`NOT_CONNECTED`), for example after the app or the server restarted. Kept apart from the view (no DOM), so every path
 * is unit-tested.
 *
 * A new session starts clean: what the lost one had set (SET ROLE, search_path, sql_safe_updates, a session READ ONLY
 * setting, temporary tables) is gone. So the run goes again on its own only when every statement reads, as the engine
 * classifies it in the new session; anything else reopens the session and waits for the user to press Run again. A lost
 * open transaction is said so and nothing runs either, and nothing reopens once rows had arrived (the statements may
 * have run).
 */
import { errorCode, NOT_CONNECTED, SESSION_LOST } from "../../core/errors";
import type { StatementCategory } from "../../core/types";

export type LostSessionOutcome =
  /** Not a lost session, or results had already arrived: the error stands. */
  | "rethrow"
  /** The session was reopened and every statement reads: the run goes again. */
  | "retry"
  /** The session was reopened; the lost one had an open transaction (rolled back with it): nothing runs. */
  | "transactionLost"
  /** The session was reopened; the run does more than read (or could not be classified): nothing runs. */
  | "sessionReset";

export interface LostSessionSteps {
  /** Results of this run already arrived (some statements may have run). */
  received: boolean;
  /** The lost session had an open transaction. */
  hadTransaction: boolean;
  /** Opens a new session (connecting again first when needed); rejects when that fails. */
  reopen(): Promise<void>;
  /** The categories of the run's statements, classified in the new session. */
  classify(): Promise<StatementCategory[]>;
}

/** The backend no longer has the tab's session, or its connection: nothing ran. */
export function sessionGone(error: unknown): boolean {
  const code = errorCode(error);
  return code === SESSION_LOST || code === NOT_CONNECTED;
}

/** At least one statement, and every one of them reads. */
export function onlyReads(categories: readonly StatementCategory[]): boolean {
  return categories.length > 0 && categories.every((category) => category === "read");
}

/** Reopens the session when `error` says it was lost before anything arrived, and decides whether the run goes again. */
export async function recoverLostSession(error: unknown, steps: LostSessionSteps): Promise<LostSessionOutcome> {
  if (!sessionGone(error) || steps.received) return "rethrow";
  await steps.reopen();
  if (steps.hadTransaction) return "transactionLost";
  let categories: StatementCategory[];
  try {
    categories = await steps.classify();
  } catch {
    return "sessionReset";
  }
  return onlyReads(categories) ? "retry" : "sessionReset";
}

/** What Messages says when nothing ran after the session was reopened. */
export function lostSessionMessage(outcome: "transactionLost" | "sessionReset"): string {
  return outcome === "transactionLost"
    ? "This tab lost its session (its connection closed, or the app or the server restarted), and with it the open transaction: its uncommitted changes were rolled back. Nothing ran; run again to continue in a new session."
    : "This tab lost its session (its connection closed, or the app or the server restarted), so it was reset: settings made in it, such as SET ROLE, search_path or a session's read-only mode, and its temporary tables are gone. Nothing ran, because the query does more than read; check those settings, then press Run again.";
}
