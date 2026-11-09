/**
 * Relaxing the rules of a Production workspace needs its name typed. The backend refuses such a save with
 * `RELAX_CONFIRMATION_REQUIRED` (detail: `expected`, the name, and `changes`, what gets looser); the caller asks for the
 * name and saves again with it. Used where the dialog cannot tell in advance (connection saves and moves).
 */
import { errorCode, errorText } from "../../core/errors";

export const RELAX_CONFIRMATION_REQUIRED = "RELAX_CONFIRMATION_REQUIRED";

export interface RelaxProblem {
  /** The workspace name to type. */
  expected: string;
  /** What gets looser, as short sentences. */
  changes: string[];
  message: string;
}

/** The refusal of a save that needs the Production workspace name, or null for any other error. */
export function relaxProblem(error: unknown): RelaxProblem | null {
  if (errorCode(error) !== RELAX_CONFIRMATION_REQUIRED) return null;
  const detail = error as { expected?: unknown; changes?: unknown };
  return {
    expected: typeof detail.expected === "string" ? detail.expected : "",
    changes: Array.isArray(detail.changes) ? detail.changes.filter((change): change is string => typeof change === "string") : [],
    message: errorText(error),
  };
}

/**
 * Runs `save` without a confirmation; when the backend asks for the Production workspace name, asks `prompt` for it and
 * runs `save` again with the typed name (until it succeeds, fails otherwise, or the prompt is cancelled: then null).
 */
export async function withRelaxConfirmation<T>(
  save: (relaxConfirmation: string | null) => Promise<T>,
  prompt: (problem: RelaxProblem) => Promise<string | null>,
): Promise<T | null> {
  let typed: string | null = null;
  for (;;) {
    try {
      return await save(typed);
    } catch (error) {
      const problem = relaxProblem(error);
      if (!problem) throw error;
      typed = await prompt(problem);
      if (typed === null) return null;
    }
  }
}
