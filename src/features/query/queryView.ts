import type { EditorView } from "@codemirror/view";
import { api, errorText } from "../../core/api";
import { listDatabases, listObjects } from "../../core/connectionApi";
import type { ErrorPosition } from "../../core/contract";
import { renderError, showError } from "../../core/errorPanel";
import { positionOffset } from "../../core/errors";
import { confirmDialog, toast } from "../../core/dialogs";
import { clamp, formatNumber, h, icon, secondaryTool, separator, spacer, toolButton } from "../../core/dom";
import { driverInfo } from "../../core/drivers";
import { openColumnsPopover } from "../grid/columnsPopover";
import { DataGrid, gridSetup } from "../grid/grid";
import { legacyGridData } from "../grid/legacy";
import { makeSplitter } from "../../core/splitter";
import { createSqlEditor, selectedOrAllText, setSchemaTables } from "./sqlEditor";
import { HistoryPanel } from "./historyPanel";
import { AttemptTimer } from "./attemptTimer";
import { summarizeSets } from "./historyFormat";
import { sqlDialect, type SqlDialect } from "../../core/sql";
import type { TabContent } from "../../core/tabs";
import type { HistoryEntry, QueryOutcome, ResultSet, TransactionOp, TransactionState } from "../../core/types";
import { withConfirmation } from "../safety/confirmationDialog";
import { commitsOpenTransaction, IDLE_TRANSACTION_MINUTES, idleReminderDue, transactionButtons, transactionLabel } from "../safety/safety";

interface ResultPanel {
  title: string;
  element: HTMLElement;
  /** The grid of a result set; the Columns and Cell editor buttons act on it. */
  grid?: DataGrid;
}

/** What a query tab needs from the app shell. */
export interface QueryHost {
  connectionName(connectionId: string): string;
  /** The `queryHistory` setting. */
  historyEnabled(): boolean;
  openQuery(connectionId: string, database: string | null, sql: string): void;
}

const HISTORY_PANEL_KEY = "db-studio.queryHistoryPanel";
/** Reported once per session so a read-only disk does not flood the user with toasts. */
let historyFailureReported = false;

interface Message {
  text: string;
  kind: "info" | "error" | "muted";
}

const MIN_EDITOR_HEIGHT = 80;
const IDLE_CHECK_MS = 30_000;
const MIN_RESULTS_HEIGHT = 120;

export class QueryView implements TabContent {
  readonly element: HTMLElement;
  readonly connectionId: string;
  readonly driver: string;
  /** Message of the last failed run, cleared by a successful one. */
  lastError: string | null = null;
  /** Called when the database selection or the run outcome changes. */
  onChanged: (() => void) | null = null;
  private readonly sessionId = crypto.randomUUID();
  private readonly editorHost = h("div", { class: "editor-host" });
  private readonly editor: EditorView;
  private readonly databaseSelect = h("select", { class: "input database-select", title: "Database used by this query" });
  private readonly resultTabs = h("div", { class: "result-tabs" });
  private readonly resultBody = h("div", { class: "result-body" });
  private readonly runButton: HTMLButtonElement;
  private readonly stopButton: HTMLButtonElement;
  private readonly columnsButton: HTMLButtonElement;
  private readonly cellButton: HTMLButtonElement;
  private readonly historyButton: HTMLButtonElement;
  private readonly host: QueryHost;
  private readonly history: HistoryPanel;
  private activeGrid: DataGrid | null = null;
  private panelGrids: DataGrid[] = [];
  private sessionOpened = false;
  private running = false;
  private cancelRequested = false;
  /** Transaction state as last reported by the engine. */
  private transaction: TransactionState | null = null;
  private transactionOpenedAt: number | null = null;
  private transactionRemindedAt: number | null = null;
  private readonly idleTimer: number;
  private readonly transactionIndicator = h("span", { class: "tx-indicator", role: "status" });
  private readonly autoCommit = h("input", { type: "checkbox", checked: true, title: "Commit every statement on its own" });
  private readonly transactionButtons: HTMLButtonElement[];
  private readonly transactionsSupported: boolean;

  constructor(connectionId: string, driver: string, database: string | null, initialSql: string, host: QueryHost) {
    this.connectionId = connectionId;
    this.driver = driver;
    this.host = host;
    const info = driverInfo(driver);
    this.editor = createSqlEditor(this.editorHost, initialSql, () => void this.run());
    this.runButton = toolButton("play", "Run", () => void this.run(), "Run (Ctrl+Enter). Runs the selection if there is one.");
    this.stopButton = toolButton("stop", "Stop", () => void this.cancel(), "Cancel the running query");
    this.stopButton.disabled = true;
    this.stopButton.hidden = !info.supportsCancel;
    this.columnsButton = secondaryTool(
      toolButton("columns", "Columns", () => this.activeGrid && openColumnsPopover(this.activeGrid, this.columnsButton), "Show or hide columns of this result"),
    );
    this.cellButton = secondaryTool(
      toolButton("expand", "Cell editor", () => this.activeGrid?.openSelectedInEditor(), "Open the selected cell in a larger viewer (Shift+Enter)"),
    );
    this.historyButton = secondaryTool(toolButton("history", "History", () => this.toggleHistory(), "Show or hide the query history"));
    this.transactionsSupported = info.id !== "mongodb";
    this.transactionButtons = [
      toolButton("bolt", "Begin", () => void this.transactionControl("begin"), "Start a transaction in this tab"),
      toolButton("check", "Commit", () => void this.transactionControl("commit"), "Commit the open transaction"),
      toolButton("x", "Rollback", () => void this.transactionControl("rollback"), "Roll back the open transaction"),
    ];
    this.autoCommit.addEventListener("change", () => void this.toggleAutoCommit());
    this.databaseSelect.addEventListener("change", () => {
      void this.loadSchema();
      this.onChanged?.();
    });
    this.history = new HistoryPanel({
      connectionId,
      enabled: () => host.historyEnabled(),
      insert: (sql) => this.insertText(sql),
      openInNewTab: (entry) => host.openQuery(entry.connectionId, entry.database, entry.sql),
      close: () => this.toggleHistory(false),
    });

    const splitter = h("div", { class: "splitter-y" });
    makeSplitter(splitter, "y", (delta) => this.resizeEditor(delta));
    this.element = h(
      "div",
      { class: "view query-view" },
      h(
        "div",
        { class: "toolbar" },
        this.runButton,
        this.stopButton,
        separator(),
        icon(info.icon, `driver-icon ${info.id}`),
        this.databaseSelect,
        separator(),
        this.transactionsSupported ? h("label", { class: "checkbox tx-autocommit" }, this.autoCommit, h("span", {}, "Auto-commit")) : null,
        ...(this.transactionsSupported ? this.transactionButtons : []),
        this.transactionIndicator,
        spacer(),
        h("div", { class: "toolbar-group toolbar-end" }, this.columnsButton, this.cellButton, separator(), this.historyButton),
      ),
      h(
        "div",
        { class: "query-body" },
        h("div", { class: "query-main" }, this.editorHost, splitter, h("div", { class: "results" }, this.resultTabs, this.resultBody)),
        this.history.element,
      ),
    );
    this.showPanels([{ title: "Messages", element: messageList([{ text: info.queryHint, kind: "muted" }]) }]);
    this.toggleHistory(readHistoryPanelOpen(), false);
    this.showTransaction(this.transactionsSupported ? null : { supported: false, open: false, autoCommit: true });
    this.idleTimer = window.setInterval(() => this.remindIdleTransaction(), IDLE_CHECK_MS);
    void this.loadDatabases(database);
  }

  /** True while the engine reports an open transaction in this tab. */
  hasOpenTransaction(): boolean {
    return this.transaction?.open === true;
  }

  canClose(): boolean | Promise<boolean> {
    if (!this.hasOpenTransaction()) return true;
    return confirmDialog("This tab has an open transaction. Closing the tab rolls it back, so uncommitted changes are lost. Close anyway?", "Close tab");
  }

  private async transactionControl(op: TransactionOp): Promise<void> {
    // No double Begin while the first one is still on its way.
    if (op === "begin") this.transactionButtons[0].disabled = true;
    try {
      await this.ensureSession();
      const state = await api.sqlTransaction(this.sessionId, op);
      this.showTransaction(state);
      const done: Partial<Record<TransactionOp, string>> = { begin: "Transaction started", commit: "Committed", rollback: "Rolled back" };
      if (op !== "status") toast(done[op] ?? transactionLabel(state), "success");
    } catch (error) {
      showError(error, "Transaction");
      this.showTransaction(this.transaction);
      void this.refreshTransaction();
    }
  }

  private async toggleAutoCommit(): Promise<void> {
    const on = this.autoCommit.checked;
    if (on && this.hasOpenTransaction()) {
      const proceed = await confirmDialog("Turning auto-commit on commits the open transaction. Continue?", "Commit and switch");
      if (!proceed) {
        this.autoCommit.checked = false;
        return;
      }
    }
    await this.transactionControl(on ? "autoCommitOn" : "autoCommitOff");
  }

  /** Asks the engine again (after a failed run the transaction may still be open). */
  private async refreshTransaction(): Promise<void> {
    if (!this.sessionOpened || !this.transactionsSupported) return;
    try {
      this.showTransaction(await api.sqlTransaction(this.sessionId, "status"));
    } catch {
      // The status is advisory; the next run reports it again.
    }
  }

  private showTransaction(state: TransactionState | null): void {
    const wasOpen = this.transaction?.open === true;
    this.transaction = state;
    const open = state?.open === true;
    if (open && !wasOpen) {
      this.transactionOpenedAt = Date.now();
      this.transactionRemindedAt = null;
    } else if (!open) {
      this.transactionOpenedAt = null;
      this.transactionRemindedAt = null;
    }
    this.transactionIndicator.textContent = transactionLabel(state);
    this.transactionIndicator.classList.toggle("open", open);
    this.transactionIndicator.classList.toggle("unsupported", state?.supported === false);
    this.transactionIndicator.title =
      state?.supported === false
        ? "MongoDB commands run on their own here: the app does not open server sessions, so there are no transactions to show."
        : "Reported by the database after each run";
    if (state?.supported) this.autoCommit.checked = state.autoCommit;
    const [begin, commit, rollback] = this.transactionButtons;
    const usable = transactionButtons(state);
    begin.disabled = !usable.begin;
    begin.title = open ? "A transaction is already open: commit or roll it back first" : "Start a transaction in this tab";
    commit.disabled = !usable.commit;
    rollback.disabled = !usable.rollback;
    this.element.classList.toggle("tx-open", open);
  }

  private remindIdleTransaction(): void {
    const now = Date.now();
    if (!idleReminderDue(this.transactionOpenedAt, this.transactionRemindedAt, now)) return;
    this.transactionRemindedAt = now;
    const minutes = Math.max(Math.round((now - (this.transactionOpenedAt ?? now)) / 60_000), IDLE_TRANSACTION_MINUTES);
    showError(
      {
        code: "TRANSACTION_IDLE",
        message: `A query tab on ${this.host.connectionName(this.connectionId)} has had a transaction open for ${minutes} minutes. Commit or roll back to release its locks.`,
      },
      "Open transaction",
    );
  }

  onActivate(): void {
    requestAnimationFrame(() => this.editor.focus());
  }

  currentDatabase(): string | null {
    return this.databaseSelect.value || null;
  }

  /** The selection if there is one, otherwise the whole editor. */
  currentText(): string {
    return selectedOrAllText(this.editor);
  }

  /** Appends text typed while the editor was loading, with the cursor after it. */
  appendTyped(text: string): void {
    const end = this.editor.state.doc.length;
    this.editor.dispatch({ changes: { from: end, insert: text }, selection: { anchor: end + text.length } });
  }

  /** Replaces the selection (or appends when nothing is selected) and focuses the editor. */
  insertText(text: string): void {
    const { state } = this.editor;
    const range = state.selection.main;
    const empty = range.empty;
    const from = empty ? state.doc.length : range.from;
    const to = empty ? state.doc.length : range.to;
    const prefix = empty && state.doc.length > 0 && !state.doc.toString().endsWith("\n") ? "\n\n" : empty && state.doc.length > 0 ? "\n" : "";
    this.editor.dispatch({
      changes: { from, to, insert: prefix + text },
      selection: { anchor: from + prefix.length, head: from + prefix.length + text.length },
    });
    this.editor.focus();
  }

  private toggleHistory(open = this.history.element.hidden, remember = true): void {
    this.history.element.hidden = !open;
    this.historyButton.classList.toggle("active", open);
    this.historyButton.setAttribute("aria-pressed", String(open));
    if (open) this.history.show();
    if (remember) writeHistoryPanelOpen(open);
  }

  dispose(): void {
    window.clearInterval(this.idleTimer);
    this.editor.destroy();
    for (const grid of this.panelGrids) grid.dispose();
    if (this.sessionOpened) void api.closeSession(this.sessionId);
  }

  private async loadDatabases(preferred: string | null): Promise<void> {
    try {
      const databases = await listDatabases(this.connectionId);
      this.databaseSelect.replaceChildren(
        h("option", { value: "" }, "(no database)"),
        ...databases.map((name) => h("option", { value: name, selected: name === preferred }, name)),
      );
      this.onChanged?.();
      await this.loadSchema();
    } catch (error) {
      showError(error, "List databases");
    }
  }

  private async loadSchema(): Promise<void> {
    const database = this.databaseSelect.value;
    try {
      const tables = database ? await listObjects(this.connectionId, database) : [];
      setSchemaTables(
        this.editor,
        tables.map((table) => table.label),
      );
    } catch (error) {
      showError(error, "Load table names for completion");
    }
  }

  private async run(): Promise<void> {
    const raw = selectedOrAllText(this.editor);
    const sql = raw.trim();
    // Where the text that runs starts in the editor, so an error position can be shown there.
    const selection = this.editor.state.selection.main;
    const base = (selection.empty ? 0 : selection.from) + (raw.length - raw.trimStart().length);
    if (!sql || this.running) return;
    if (commitsOpenTransaction(this.driver, sql, this.transaction)) {
      const proceed = await confirmDialog(
        "This runs DDL (or a new BEGIN), which commits the open transaction first on MariaDB/MySQL; Rollback will not undo the earlier changes. Run anyway?",
        "Commit and run",
      );
      if (!proceed) return;
    }
    const database = this.databaseSelect.value || null;
    const startedAt = Date.now();
    // Time spent in the confirmation dialog is not query time, and Stop is only usable while a statement runs.
    const timer = new AttemptTimer();
    const inFlight = (running: boolean) => (this.stopButton.disabled = !running);
    this.cancelRequested = false;
    this.setRunning(true);
    try {
      await this.ensureSession();
      const outcome = await withConfirmation((confirmation) => timer.attempt(() => api.runSql(this.sessionId, database, sql, confirmation), inFlight));
      if (outcome === null) {
        this.showPanels([{ title: "Messages", element: messageList([{ text: "Cancelled; nothing ran.", kind: "muted" }]) }]);
        return;
      }
      this.lastError = null;
      this.showOutcome(outcome);
      if (this.transactionsSupported) this.showTransaction(outcome.transaction);
      this.record({ sql, database, startedAt, durationMs: outcome.elapsedMs, results: summarizeSets(outcome.sets), error: null });
    } catch (error) {
      this.lastError = errorText(error);
      const block = renderError(error, { onJump: (position) => this.jumpTo(sql, base, position) });
      this.showPanels([{ title: "Messages", element: h("div", { class: "messages" }, block) }]);
      showError(error, `Query on ${this.host.connectionName(this.connectionId)}`, { reveal: false });
      this.record({ sql, database, startedAt, durationMs: timer.elapsed(), results: [], error: this.lastError });
      void this.refreshTransaction();
    } finally {
      this.setRunning(false);
      this.onChanged?.();
    }
  }

  /** Adds a run to the query history in the background; never blocks or fails the run. */
  /** Moves the cursor to an error position reported for `text`, which started at `base` in the editor. */
  private jumpTo(text: string, base: number, position: ErrorPosition): void {
    const offset = positionOffset(text, position);
    if (offset === null) return;
    const anchor = Math.min(base + offset, this.editor.state.doc.length);
    this.editor.dispatch({ selection: { anchor }, scrollIntoView: true });
    this.editor.focus();
  }

  private record(run: Pick<HistoryEntry, "sql" | "database" | "startedAt" | "durationMs" | "results" | "error">): void {
    if (!this.host.historyEnabled()) return;
    const entry: HistoryEntry = {
      ...run,
      id: "",
      durationMs: Math.max(0, Math.round(run.durationMs)),
      connectionId: this.connectionId,
      connectionName: this.host.connectionName(this.connectionId),
      driver: this.driver,
      cancelled: this.cancelRequested && run.error !== null,
    };
    api
      .historyAppend(entry)
      .then((stored) => stored && this.history.added(stored))
      .catch((error: unknown) => {
        if (historyFailureReported) return;
        historyFailureReported = true;
        showError(error, "The query ran, but it could not be added to the history");
      });
  }

  private async cancel(): Promise<void> {
    this.cancelRequested = true;
    try {
      await api.cancelSql(this.sessionId);
      toast("Cancel requested");
    } catch (error) {
      showError(error, "Cancel the query");
    }
  }

  private async ensureSession(): Promise<void> {
    if (this.sessionOpened) return;
    await api.openSession(this.connectionId, this.sessionId);
    this.sessionOpened = true;
  }

  private setRunning(running: boolean): void {
    this.running = running;
    this.runButton.disabled = running;
    // Enabled by the attempt itself (see run), so it stays off while a confirmation dialog is open.
    if (!running) this.stopButton.disabled = true;
    this.element.classList.toggle("busy", running);
  }

  private showOutcome(outcome: QueryOutcome): void {
    const rowSets = outcome.sets.filter((set) => set.columns.length > 0);
    const panels: ResultPanel[] = rowSets.map((set, index) => {
      const grid = resultGrid(set, sqlDialect(this.driver));
      return { title: rowSets.length > 1 ? `Result ${index + 1}` : "Result", element: grid.element, grid };
    });
    const messages: Message[] = outcome.sets.map((set, index) => ({ text: describeSet(set, index), kind: "info" }));
    messages.push({ text: `Finished in ${formatNumber(outcome.elapsedMs)} ms`, kind: "muted" });
    panels.push({ title: "Messages", element: messageList(messages) });
    this.showPanels(panels);
  }

  private showPanels(panels: ResultPanel[]): void {
    // The previous results' grids stop observing their size.
    for (const grid of this.panelGrids) grid.dispose();
    this.panelGrids = panels.flatMap((panel) => (panel.grid ? [panel.grid] : []));
    const buttons = panels.map((panel) => h("button", { class: "result-tab", type: "button" }, panel.title));
    const activate = (active: number) => {
      buttons.forEach((button, index) => button.classList.toggle("active", index === active));
      this.resultBody.replaceChildren(panels[active].element);
      this.activeGrid = panels[active].grid ?? null;
      this.columnsButton.disabled = this.activeGrid === null;
      this.cellButton.disabled = this.activeGrid === null;
    };
    buttons.forEach((button, index) => button.addEventListener("click", () => activate(index)));
    this.resultTabs.replaceChildren(...buttons);
    activate(0);
  }

  private resizeEditor(delta: number): void {
    const maximum = this.element.clientHeight - MIN_RESULTS_HEIGHT;
    const height = clamp(this.editorHost.offsetHeight + delta, MIN_EDITOR_HEIGHT, maximum);
    this.editorHost.style.height = `${height}px`;
  }
}

function resultGrid(set: ResultSet, dialect: SqlDialect): DataGrid {
  const grid = new DataGrid();
  const data = legacyGridData(set.columns, set.rows);
  grid.load({ ...gridSetup(data.columns, data.rows, dialect), clientFilter: true });
  return grid;
}

function readHistoryPanelOpen(): boolean {
  try {
    return localStorage.getItem(HISTORY_PANEL_KEY) === "open";
  } catch {
    return false;
  }
}

function writeHistoryPanelOpen(open: boolean): void {
  try {
    localStorage.setItem(HISTORY_PANEL_KEY, open ? "open" : "closed");
  } catch {
    // Storage can be unavailable; the panel then starts closed in new tabs.
  }
}

function describeSet(set: ResultSet, index: number): string {
  const label = `#${index + 1}`;
  if (set.columns.length === 0) return `${label}: ${formatNumber(set.affectedRows)} row(s) affected`;
  const limited = set.truncated ? ` (showing the first ${formatNumber(set.rows.length)})` : "";
  return `${label}: ${formatNumber(set.rows.length)} row(s)${limited}`;
}

function messageList(messages: Message[]): HTMLElement {
  return h("div", { class: "messages" }, ...messages.map((message) => h("div", { class: `message ${message.kind}` }, message.text)));
}
