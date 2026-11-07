/**
 * The confirmation protocol of the statement gateway on the UI side: a call that needs a
 * confirmation fails with `CONFIRMATION_REQUIRED` (what would run, why, and a one-time
 * token); the user sees this dialog, and the same call is made again with the token (and,
 * for a typed confirmation, the table or workspace name the user typed).
 */
import { field, openModal, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { Confirmation, GatewayProblem } from "../../core/types";
import { CATEGORY_LABELS, confirmationReason, gatewayProblem, typedMatches } from "./safety";

const MAX_LISTED = 12;

/** Shows what will run and why it needs a confirmation; resolves with the answer, or null when cancelled. */
export function askConfirmation(problem: GatewayProblem): Promise<Confirmation | null> {
  return new Promise((resolve) => {
    let answer: Confirmation | null = null;
    const typed = problem.level === "typed";
    const input = textInput("", { class: "input mono", placeholder: problem.expected ?? "", "aria-label": "Confirmation text" });
    const listed = problem.statements.slice(0, MAX_LISTED);
    const more = problem.statements.length - listed.length;
    const body = h(
      "div",
      { class: "form-grid confirm-statements" },
      h("p", { class: "confirm-reason" }, confirmationReason(problem)),
      h(
        "p",
        { class: "form-intro" },
        "Connection ",
        h("strong", {}, problem.connection),
        " in workspace ",
        h("strong", {}, problem.workspace),
        ". This will run:",
      ),
      h(
        "ul",
        { class: "statement-list" },
        ...listed.map((statement) =>
          h(
            "li",
            { class: `statement-item category-${statement.category}` },
            h("span", { class: `category-badge category-${statement.category}` }, CATEGORY_LABELS[statement.category]),
            h("code", { class: "statement-excerpt" }, statement.excerpt),
            statement.reason ? h("span", { class: "statement-reason" }, statement.reason) : null,
          ),
        ),
        more > 0 ? h("li", { class: "statement-item muted" }, `…and ${more} more statement(s)`) : null,
      ),
      typed && problem.expected ? field(`Type ${problem.expected} to confirm`, input, "The confirmation is used once, for exactly these statements.") : null,
    );
    openModal(
      typed ? "Type to confirm" : "Confirm statements",
      body,
      [
        { label: "Cancel", run: () => undefined },
        {
          label: typed ? "Run anyway" : "Run",
          primary: true,
          danger: typed,
          run: () => {
            if (!problem.token) return;
            if (typed && !typedMatches(problem, input.value)) throw new Error(`Type ${problem.expected ?? ""} exactly to confirm`);
            answer = { token: problem.token, typed: typed ? input.value.trim() : null };
          },
        },
      ],
      { width: 620, onClose: () => resolve(answer) },
    );
    if (typed) requestAnimationFrame(() => input.focus());
  });
}

/**
 * Runs a gateway call, asking for a confirmation when the rules require one. A confirmation
 * given once is kept for later calls of the same session object (one import run sends its
 * batches with the same token). Resolves with null when the user cancels.
 */
export class ConfirmationSession {
  private confirmation: Confirmation | null = null;

  async run<T>(call: (confirmation: Confirmation | null) => Promise<T>): Promise<T | null> {
    try {
      return await call(this.confirmation);
    } catch (error) {
      const problem = gatewayProblem(error);
      if (!problem || problem.code !== "CONFIRMATION_REQUIRED") throw error;
      const answer = await askConfirmation(problem);
      if (!answer) return null;
      this.confirmation = answer;
      return call(answer);
    }
  }
}

/** One gateway call with a confirmation dialog when needed; null when cancelled. */
export function withConfirmation<T>(call: (confirmation: Confirmation | null) => Promise<T>): Promise<T | null> {
  return new ConfirmationSession().run(call);
}
