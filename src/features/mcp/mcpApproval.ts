/**
 * The dialog for an MCP client's `run_query`: the backend asks every open window (`mcp-approval`), the first answer
 * wins, and the others close (`mcp-approval-closed`). Closing the dialog any other way denies. The one-time token
 * never reaches the UI: the answer goes back by approval id, and the backend checks the typed name again.
 */
import { api, onMcpApproval, onMcpApprovalClosed } from "../../core/api";
import { field, openModal, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { McpApproval } from "../../core/types";
import { CATEGORY_LABELS, confirmationReason, typedMatches } from "../safety/safety";
import { approvalTitle, secondsLeft } from "./mcpLogic";

const open = new Map<string, () => void>();

export function showApproval(approval: McpApproval): void {
  if (open.has(approval.id)) return;
  const problem = approval.problem;
  const typed = problem.level === "typed";
  const startedAt = Date.now();
  let answered = false;
  const input = textInput("", { class: "input mono", placeholder: problem.expected ?? "", "aria-label": "Confirmation text" });
  const countdown = h("span", { class: "field-hint" });
  const tick = () => {
    countdown.textContent = `Denied automatically in ${secondsLeft(startedAt, approval.timeoutMs, Date.now())} s.`;
  };
  tick();
  const timer = window.setInterval(tick, 1000);
  const where = [approval.connection, approval.database].filter(Boolean).join(" / ");
  const body = h(
    "div",
    { class: "form-grid confirm-statements" },
    h(
      "p",
      { class: "form-intro" },
      "An MCP client identifying as ",
      h("strong", {}, approval.clientLabel),
      " wants to run this on ",
      h("strong", {}, where),
      " in workspace ",
      h("strong", {}, approval.workspace),
      ":",
    ),
    h("pre", { class: "mcp-statement mono" }, approval.statement),
    h(
      "ul",
      { class: "statement-list" },
      ...problem.statements
        .slice(0, 1)
        .map((statement) =>
          h(
            "li",
            { class: `statement-item category-${statement.category}` },
            h("span", { class: `category-badge category-${statement.category}` }, CATEGORY_LABELS[statement.category]),
            statement.reason ? h("span", { class: "statement-reason" }, statement.reason) : null,
          ),
        ),
    ),
    h("p", { class: "confirm-reason" }, typed ? confirmationReason(problem) : "Fosta Studio asks before it runs any statement from an MCP client."),
    typed && problem.expected ? field(`Type ${problem.expected} to confirm`, input, "Used once, for exactly this statement.") : null,
    countdown,
  );
  const close = openModal(
    approvalTitle(approval.clientLabel),
    body,
    [
      { label: "Deny", run: () => undefined },
      {
        label: typed ? "Run anyway" : "Run",
        primary: true,
        danger: true,
        run: async () => {
          if (typed && !typedMatches(problem, input.value)) throw new Error(`Type ${problem.expected ?? ""} exactly to confirm`);
          answered = true;
          await api.mcpAnswer(approval.id, true, typed ? input.value.trim() : null);
        },
      },
    ],
    {
      width: 620,
      onClose: () => {
        window.clearInterval(timer);
        open.delete(approval.id);
        // Deny, Escape, or the dialog closed itself: a denial (ignored when it was already answered elsewhere).
        if (!answered) void api.mcpAnswer(approval.id, false, null).catch(() => undefined);
      },
    },
  );
  open.set(approval.id, () => {
    answered = true;
    close();
  });
  if (typed) requestAnimationFrame(() => input.focus());
}

/** Closes the dialog of an approval answered elsewhere, withdrawn or timed out. */
export function closeApproval(id: string): void {
  open.get(id)?.();
}

/** Listens for approvals and shows the ones already waiting (a window opened later). */
export async function initMcpApprovals(): Promise<void> {
  await onMcpApproval(showApproval);
  await onMcpApprovalClosed(({ id }) => closeApproval(id));
  try {
    const status = await api.mcpStatus();
    for (const approval of status.pending) showApproval(approval);
  } catch {
    // Older backends without MCP: nothing to show.
  }
}
