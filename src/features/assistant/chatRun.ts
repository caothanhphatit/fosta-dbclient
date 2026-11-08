/**
 * What runs from the assistant panel, and how it is shown there:
 * - the Run button of a code block: the user's own run, through the gateway like a query tab (`run` on a session of its
 *   own, so the workspace rules apply: confirm, typed confirm, block), at most 200 rows, shown as a compact grid under
 *   the block with "Open in tab";
 * - a read-only query the assistant ran itself (`run_read_query`), shown as an expandable tool call.
 */
import { api } from "../../core/api";
import type { LanguageSpec } from "../../core/contract";
import { h, icon, spacer } from "../../core/dom";
import { renderError } from "../../core/errorPanel";
import type { ChatQuery } from "../../core/types";
import { displayValue } from "../../core/values";
import { describeResult, RunResults } from "../query/runResults";
import { withConfirmation } from "../safety/confirmationDialog";

/** Rows a Run from the chat keeps (the rest stay on the server: "Open in tab" runs it in full). */
export const CHAT_RUN_ROWS = 200;

export interface ChatRunTarget {
  connectionId: string;
  database: string | null;
  language: LanguageSpec;
}

export interface ChatRunHost {
  /** Opens `code` in a new query tab of the connection. */
  openQuery(code: string): void;
  /** The result changed size (keeps the chat scrolled). */
  resized(): void;
}

/** The result area under one code block; `run()` (re)runs the block into it. */
export class ChatRun {
  readonly element = h("div", { class: "chat-run" });
  private sessionId: string | null = null;
  private running = false;

  constructor(
    private readonly code: string,
    private readonly target: ChatRunTarget,
    private readonly host: ChatRunHost,
  ) {
    this.element.hidden = true;
  }

  async run(): Promise<void> {
    if (this.running) return;
    this.running = true;
    this.element.hidden = false;
    const stop = h("button", { class: "link-button", type: "button" }, "Stop");
    stop.addEventListener("click", () => void this.stop());
    this.element.replaceChildren(h("div", { class: "chat-run-status" }, h("span", { class: "chat-run-spinner" }), "Running…", spacer(), stop));
    this.host.resized();
    const sessionId = `chat-${crypto.randomUUID()}`;
    let results = new RunResults();
    try {
      await api.openSession(this.target.connectionId, sessionId, { language: this.target.language.id, database: this.target.database });
      this.sessionId = sessionId;
      const request = { text: this.code, database: this.target.database, offset: 0 };
      const summary = await withConfirmation((confirmation) => {
        // A confirmed second attempt starts over (the first one ran nothing).
        results = new RunResults();
        return api.run(sessionId, request, (event) => results.apply(event), confirmation, CHAT_RUN_ROWS);
      });
      if (summary === null) this.element.replaceChildren(h("div", { class: "chat-run-status muted" }, "Not run."));
      else this.element.replaceChildren(...this.renderResults(results, summary.stopped));
    } catch (error) {
      this.element.replaceChildren(...this.partial(results), renderError(error, { compact: true }));
    } finally {
      this.sessionId = null;
      this.running = false;
      void api.closeSession(sessionId).catch(() => undefined);
      this.host.resized();
    }
  }

  private async stop(): Promise<void> {
    if (this.sessionId) await api.cancelRun(this.sessionId).catch(() => undefined);
  }

  /** What arrived before an error (the statements of a script that ran before the failing one). */
  private partial(results: RunResults): HTMLElement[] {
    return results.entries.some((entry) => entry.done !== null) ? this.renderResults(results, false) : [];
  }

  private renderResults(results: RunResults, stopped: boolean): HTMLElement[] {
    const nodes: HTMLElement[] = [];
    const sets = results.resultSets();
    for (const set of sets) {
      const columns = (set.columns ?? []).map((column) => column.name);
      const rows = set.rows.map((row) => row.map((value) => displayValue(value)));
      nodes.push(resultTable(columns, rows));
    }
    const lines = results.entries.map((entry, index) => describeResult(entry, index, "not finished"));
    if (stopped) lines.push("Stopped.");
    const open = h(
      "button",
      { class: "chat-code-button", type: "button", title: "Open it in a new query tab, to run it there with every row" },
      icon("query"),
      "Open in tab",
    );
    open.addEventListener("click", () => this.host.openQuery(this.code));
    nodes.push(h("div", { class: "chat-run-footer" }, h("span", { class: "chat-run-summary" }, lines.join(" · ") || "Done."), spacer(), open));
    return nodes;
  }
}

/** A small read-only grid: header, rows, NULL shown muted. */
export function resultTable(columns: string[], rows: (string | null)[][]): HTMLElement {
  const head = h("tr", {}, ...columns.map((column) => h("th", {}, column)));
  const body = rows.map((row) => h("tr", {}, ...row.map((cell) => (cell === null ? h("td", { class: "null" }, "NULL") : h("td", { title: cell }, cell)))));
  return h("div", { class: "chat-grid" }, h("table", {}, h("thead", {}, head), h("tbody", {}, ...body)));
}

/** A read-only query the assistant ran: the statement and row count, the rows it was sent when expanded. */
export function queryToolCall(query: ChatQuery): HTMLElement {
  const outcome = query.error
    ? "refused or failed"
    : `${query.rows.length.toLocaleString("en-US")} row(s)${query.truncated ? " (more not read)" : ""} · ${query.elapsedMs.toLocaleString("en-US")} ms`;
  const summary = h(
    "summary",
    {},
    icon(query.error ? "x" : "query"),
    h("span", { class: "chat-tool-title" }, "Ran a read-only query"),
    h("span", { class: "chat-tool-outcome" }, outcome),
  );
  const body: HTMLElement[] = [h("pre", { class: "chat-tool-statement" }, h("code", {}, query.statement))];
  if (query.error) body.push(h("div", { class: "chat-error" }, query.error));
  else {
    if (query.columns.length > 0) body.push(resultTable(query.columns, query.rows));
    body.push(h("div", { class: "chat-tool-note muted" }, "These rows were sent to your AI provider."));
  }
  return h("details", { class: `chat-tool chat-tool-query${query.error ? " failed" : ""}` }, summary, ...body);
}
