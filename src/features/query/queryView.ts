/**
 * The query tab: an editor for one of the connection's query languages and a session on the engine contract.
 *
 * - `openSession(sessionId, { language, database })`; the `SessionInfo` decides whether Stop (`cancellable`) and the
 *   transaction controls (`transaction.supported`) are shown. No engine ids are compared here.
 * - The editor's language comes from the injected `languageSupport` (`LanguageSupport.editorExtensions`, `statementAt`
 *   for "Run statement" through `currentStatement`, `format` for "Format" through `formatInEditor`; SQL comes from the
 *   language library in `src/lang`).
 * - `run` streams `StreamEvent`s into `RunResults`; the result tabs and their row counts update while rows arrive.
 */
import type { EditorView } from "@codemirror/view";
import type { Extension } from "@codemirror/state";
import { api, errorText } from "../../core/api";
import { databaseName, databaseNodes, type ConnectionApi } from "../../core/connectionApi";
import type {
  ConnectInfo,
  EngineDescriptor,
  ErrorPosition,
  LanguageSpec,
  RunRequest,
  RunSummary,
  SessionInfo,
  SqlDialectId,
  TransactionOp,
  TransactionState,
} from "../../core/contract";
import { confirmDialog, toast } from "../../core/dialogs";
import { clamp, formatNumber, h, icon, iconOrDefault, secondaryTool, separator, spacer, toolButton } from "../../core/dom";
import { defaultLanguage, hasCapability } from "../../core/engines";
import { renderError, showError } from "../../core/errorPanel";
import { isCancelError } from "../../core/errors";
import { statementAt, type LanguageSupport } from "../../core/language";
import { currentStatement, formatInEditor } from "../../lang/editor";
import { makeSplitter } from "../../core/splitter";
import { dialectOf } from "../../core/sql";
import type { TabContent } from "../../core/tabs";
import type { HistoryEntry } from "../../core/types";
import { openColumnsPopover } from "../grid/columnsPopover";

import { withConfirmation } from "../safety/confirmationDialog";
import { commitsOpenTransaction, IDLE_TRANSACTION_MINUTES, idleReminderDue, transactionButtons, transactionLabel } from "../safety/safety";
import { AttemptTimer } from "./attemptTimer";
import { HistoryPanel } from "./historyPanel";
import { ResultArea, type Message } from "./resultArea";
import { editorOffset, RunResults } from "./runResults";
import { createSqlEditor, selectedOrAllText, setEditorLanguage } from "./sqlEditor";
import "./query-run.css";

/** What a query tab needs from the app shell. */
export interface QueryHost {
  connectionName(connectionId: string): string;
  /** The `queryHistory` setting. */
  historyEnabled(): boolean;
  openQuery(connectionId: string, database: string | null, sql: string): void;
}

/** Everything a query tab is built from (the view registry fills it from the `ViewContext`). */
export interface QueryViewOptions {
  connection: ConnectionApi;
  engine: EngineDescriptor;
  /** What the server reported on connect (languages, capabilities); null when unknown. */
  info: ConnectInfo | null;
  database: string | null;
  text: string;
  host: QueryHost;
  /** The editor support for one of the connection's languages (`src/engines/index.ts` `languageSupport`). */
  languageSupport(languageId: string): Promise<LanguageSupport>;
}

const HISTORY_PANEL_KEY = "db-studio.queryHistoryPanel";
/** Reported once per session so a read-only disk does not flood the user with toasts. */
let historyFailureReported = false;

/** The text a run sends and where it starts in the editor (`RunRequest.offset`). */
interface RunTarget {
  text: string;
  offset: number;
}

const MIN_EDITOR_HEIGHT = 80;
const IDLE_CHECK_MS = 30_000;
const MIN_RESULTS_HEIGHT = 120;

/** Live state a query tab reports to the status bar (read from what the tab already shows). */
export interface QueryStatus {
  /** "2 statement(s) · 14 row(s) · 31 ms", or empty before the first run. */
  lastRun: string;
  /** "Auto-commit", "Transaction open"; null when the engine has no transactions. */
  transaction: string | null;
  transactionOpen: boolean;
}

export class QueryView implements TabContent {
  readonly element: HTMLElement;
  readonly connectionId: string;
  readonly engineId: string;
  /** Message of the last failed run, cleared by a successful one. */
  lastError: string | null = null;
  /** Called when the database selection or the run outcome changes. */
  onChanged: (() => void) | null = null;
  private readonly connection: ConnectionApi;
  private readonly engine: EngineDescriptor;
  private readonly host: QueryHost;
  private readonly loadSupport: (languageId: string) => Promise<LanguageSupport>;
  private readonly languages: LanguageSpec[];
  private language: LanguageSpec;
  private support: LanguageSupport | null = null;
  private languageGeneration = 0;
  private extensionGeneration = 0;
  private sessionId = crypto.randomUUID();
  private session: SessionInfo | null = null;
  private sessionOpening: Promise<SessionInfo> | null = null;
  private readonly editorHost = h("div", { class: "editor-host" });
  private readonly editor: EditorView;
  private readonly databaseSelect = h("select", { class: "input database-select", title: "Database used by this query" });
  private readonly languageSelect = h("select", { class: "input language-select", title: "Query language of this tab" });
  private readonly runStatus = h("span", { class: "run-status", role: "status" });
  private readonly resultArea = new ResultArea(() => dialectOf(this.dialect()));
  private readonly runButton: HTMLButtonElement;
  private readonly statementButton: HTMLButtonElement;
  private readonly stopButton: HTMLButtonElement;
  private readonly formatButton: HTMLButtonElement;
  private readonly columnsButton: HTMLButtonElement;
  private readonly cellButton: HTMLButtonElement;
  private readonly historyButton: HTMLButtonElement;
  private readonly history: HistoryPanel;
  private renderFrame: number | null = null;
  private running = false;
  private cancelRequested = false;
  private disposed = false;
  /** Transaction state as last reported by the engine. */
  private transaction: TransactionState | null = null;
  private transactionsSupported: boolean;
  private transactionOpenedAt: number | null = null;
  private transactionRemindedAt: number | null = null;
  private readonly idleTimer: number;
  private readonly transactionIndicator = h("span", { class: "tx-indicator", role: "status" });
  private readonly autoCommit = h("input", { type: "checkbox", checked: true, title: "Commit every statement on its own" });
  private readonly autoCommitLabel: HTMLElement;
  private readonly transactionButtons: HTMLButtonElement[];

  constructor(options: QueryViewOptions) {
    this.connection = options.connection;
    this.connectionId = options.connection.connectionId;
    this.engine = options.engine;
    this.engineId = options.engine.id;
    this.host = options.host;
    this.loadSupport = options.languageSupport;
    const source = options.info && options.info.languages.length > 0 ? options.info : options.engine;
    this.languages = source.languages.length > 0 ? source.languages : [fallbackLanguage(options.engine)];
    this.language = defaultLanguage(source) ?? this.languages[0];
    const capabilities = options.info ?? options.engine;
    this.transactionsSupported = hasCapability(capabilities, "transactions");

    this.editor = createSqlEditor(this.editorHost, options.text, {
      run: () => void this.run("selection"),
      runStatement: () => void this.run("statement"),
      format: () => void this.format(),
    });
    this.runButton = toolButton("play", "Run", () => void this.run("selection"), "Run the selection, or everything when nothing is selected (Ctrl+Enter)");
    this.statementButton = secondaryTool(
      toolButton("chevron", "Run statement", () => void this.run("statement"), "Run the selection, or the statement under the cursor (Ctrl+Shift+Enter)"),
    );
    this.stopButton = toolButton("stop", "Stop", () => void this.cancel(), "Cancel the running query");
    this.stopButton.disabled = true;
    this.stopButton.hidden = !hasCapability(capabilities, "cancel");
    this.formatButton = secondaryTool(toolButton("design", "Format", () => void this.format(), "Format the selection, or everything (Shift+Alt+F)"));
    this.columnsButton = secondaryTool(
      toolButton(
        "columns",
        "Columns",
        () => {
          const grid = this.resultArea.activeGrid();
          if (grid) openColumnsPopover(grid, this.columnsButton);
        },
        "Show or hide columns of this result",
      ),
    );
    this.cellButton = secondaryTool(
      toolButton(
        "expand",
        "Cell editor",
        () => this.resultArea.activeGrid()?.openSelectedInEditor(),
        "Open the selected cell in a larger viewer (Shift+Enter)",
      ),
    );
    this.historyButton = secondaryTool(toolButton("history", "History", () => this.toggleHistory(), "Show or hide the query history"));
    this.transactionButtons = [
      toolButton("bolt", "Begin", () => void this.transactionControl("begin"), "Start a transaction in this tab"),
      toolButton("check", "Commit", () => void this.transactionControl("commit"), "Commit the open transaction"),
      toolButton("x", "Rollback", () => void this.transactionControl("rollback"), "Roll back the open transaction"),
    ];
    this.autoCommitLabel = h("label", { class: "checkbox tx-autocommit" }, this.autoCommit, h("span", {}, "Auto-commit"));
    this.autoCommit.addEventListener("change", () => void this.toggleAutoCommit());
    this.databaseSelect.addEventListener("change", () => {
      void this.applyEditorLanguage();
      this.onChanged?.();
    });
    this.languageSelect.replaceChildren(...this.languages.map((spec) => h("option", { value: spec.id, selected: spec.id === this.language.id }, spec.label)));
    this.languageSelect.hidden = this.languages.length < 2;
    this.languageSelect.addEventListener("change", () => void this.switchLanguage(this.languageSelect.value));
    this.history = new HistoryPanel({
      connectionId: this.connectionId,
      enabled: () => this.host.historyEnabled(),
      insert: (sql) => this.insertText(sql),
      openInNewTab: (entry) => this.host.openQuery(entry.connectionId, entry.database, entry.sql),
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
        this.statementButton,
        this.stopButton,
        this.formatButton,
        separator(),
        icon(iconOrDefault(this.engine.icon, "database"), `driver-icon ${this.engine.id}`),
        this.languageSelect,
        this.databaseSelect,
        separator(),
        this.autoCommitLabel,
        ...this.transactionButtons,
        this.transactionIndicator,
        spacer(),
        h("div", { class: "toolbar-group toolbar-end" }, this.columnsButton, this.cellButton, separator(), this.historyButton),
      ),
      h(
        "div",
        { class: "query-body" },
        h(
          "div",
          { class: "query-main" },
          this.editorHost,
          splitter,
          h("div", { class: "results" }, h("div", { class: "result-bar" }, this.resultArea.tabs, this.runStatus), this.resultArea.body),
        ),
        this.history.element,
      ),
    );
    this.resultArea.onActiveChange = () => this.syncResultTools();
    this.showHint();
    this.toggleHistory(readHistoryPanelOpen(), false);
    this.showTransactionControls(this.transactionsSupported);
    this.idleTimer = window.setInterval(() => this.remindIdleTransaction(), IDLE_CHECK_MS);
    void this.loadLanguage();
    void this.loadDatabases(options.database);
  }

  /** True while the engine reports an open transaction in this tab. */
  hasOpenTransaction(): boolean {
    return this.transaction?.open === true;
  }

  canClose(): boolean | Promise<boolean> {
    if (!this.hasOpenTransaction()) return true;
    return confirmDialog("This tab has an open transaction. Closing the tab rolls it back, so uncommitted changes are lost. Close anyway?", "Close tab");
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

  dispose(): void {
    this.disposed = true;
    window.clearInterval(this.idleTimer);
    if (this.renderFrame !== null) cancelAnimationFrame(this.renderFrame);
    this.editor.destroy();
    this.resultArea.dispose();
    this.closeSession();
  }

  // --- Language and editor ---------------------------------------------------------------------------------------------

  /** The SQL dialect of the current language (quoting in the grid's "Copy as INSERT", the formatter, DDL warnings). */
  private dialect(): SqlDialectId | null {
    return this.language.dialect ?? this.engine.sqlDialect;
  }

  private showHint(): void {
    this.resultArea.showMessages([{ text: this.language.hint || "Ctrl+Enter runs the query.", kind: "muted" }]);
    this.runStatus.textContent = "";
  }

  private async loadLanguage(): Promise<void> {
    const generation = ++this.languageGeneration;
    this.support = null;
    try {
      const support = await this.loadSupport(this.language.id);
      if (generation !== this.languageGeneration || this.disposed) return;
      this.support = support;
      await this.applyEditorLanguage();
    } catch (error) {
      showError(error, `Load the ${this.language.label} editor`);
    }
  }

  /** Asks the language for its editor extensions for the current database (completion names) and installs them. */
  private async applyEditorLanguage(): Promise<void> {
    const support = this.support;
    if (!support) return;
    const generation = ++this.extensionGeneration;
    let extensions: Extension[];
    try {
      extensions = await support.editorExtensions({ language: this.language, connection: this.connection, database: this.currentDatabase() });
    } catch (error) {
      showError(error, "Load names for completion");
      extensions = await support.editorExtensions({ language: this.language }).catch(() => []);
    }
    if (generation !== this.extensionGeneration || this.disposed || support !== this.support) return;
    setEditorLanguage(this.editor, extensions);
  }

  private async switchLanguage(id: string): Promise<void> {
    const next = this.languages.find((spec) => spec.id === id);
    if (!next || next.id === this.language.id) return;
    const proceed =
      !this.running &&
      (!this.hasOpenTransaction() ||
        (await confirmDialog("Switching the language opens a new session for this tab; the open transaction is rolled back. Continue?", "Switch language")));
    if (!proceed) {
      this.languageSelect.value = this.language.id;
      return;
    }
    this.closeSession();
    this.sessionId = crypto.randomUUID();
    this.language = next;
    this.showTransactionControls(hasCapability(this.engine, "transactions"));
    this.showTransaction(null);
    this.showHint();
    setEditorLanguage(this.editor, []);
    await this.loadLanguage();
  }

  /** Formats the selection, or everything, with the language's formatter (else the backend `format_sql`). */
  private async format(): Promise<void> {
    const { state } = this.editor;
    const range = state.selection.main;
    const text = range.empty ? state.doc.toString() : state.sliceDoc(range.from, range.to);
    if (!text.trim()) return;
    const support = this.support?.format ? this.support : { format: (input: string) => api.formatSql(this.dialect(), input) };
    try {
      const changed = await formatInEditor(this.editor, support);
      // Unchanged, or the text changed while the formatter ran (the edit is kept).
      if (!changed && !this.disposed && this.editor.state.doc.toString() === state.doc.toString()) {
        toast(`The ${this.language.label} formatter left the text unchanged`);
      }
    } catch (error) {
      showError(error, "Format the query");
    }
  }

  // --- Databases --------------------------------------------------------------------------------------------------------

  private async loadDatabases(preferred: string | null): Promise<void> {
    try {
      const databases = (await databaseNodes(this.connection)).map(databaseName);
      if (preferred && !databases.includes(preferred)) databases.unshift(preferred);
      this.databaseSelect.replaceChildren(
        h("option", { value: "" }, "(no database)"),
        ...databases.map((name) => h("option", { value: name, selected: name === preferred }, name)),
      );
      this.onChanged?.();
      await this.applyEditorLanguage();
    } catch (error) {
      showError(error, "List databases");
    }
  }

  // --- Session and transactions -----------------------------------------------------------------------------------------

  private async ensureSession(): Promise<SessionInfo> {
    if (this.session) return this.session;
    const sessionId = this.sessionId;
    this.sessionOpening ??= this.connection.openSession(sessionId, { language: this.language.id, database: this.currentDatabase() });
    try {
      const info = await this.sessionOpening;
      if (sessionId !== this.sessionId) throw new Error("The tab's language changed while its session was opening; run again.");
      this.session = info;
      this.stopButton.hidden = !info.cancellable;
      this.showTransactionControls(info.transaction.supported);
      this.showTransaction(info.transaction);
      return info;
    } finally {
      this.sessionOpening = null;
    }
  }

  private closeSession(): void {
    if (!this.session && !this.sessionOpening) return;
    const sessionId = this.sessionId;
    const opening = this.sessionOpening;
    this.session = null;
    this.sessionOpening = null;
    // An opening session is closed once it exists; closing never fails the caller.
    void (opening ?? Promise.resolve(null))
      .catch(() => null)
      .then(() => this.connection.closeSession(sessionId))
      .catch(() => undefined);
  }

  private showTransactionControls(supported: boolean): void {
    this.transactionsSupported = supported;
    this.autoCommitLabel.hidden = !supported;
    for (const button of this.transactionButtons) button.hidden = !supported;
    this.showTransaction(supported ? this.transaction : { supported: false, open: false, autoCommit: true, failed: false });
  }

  private async transactionControl(op: TransactionOp): Promise<void> {
    // No double Begin while the first one is still on its way.
    if (op === "begin") this.transactionButtons[0].disabled = true;
    try {
      await this.ensureSession();
      const state = await this.connection.transaction(this.sessionId, op);
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
    if (!this.session || !this.transactionsSupported) return;
    try {
      this.showTransaction(await this.connection.transaction(this.sessionId, "status"));
    } catch {
      // The status is advisory; the next run reports it again.
    }
  }

  /** What the status bar shows for this tab: the last run's summary and the transaction state. */
  statusInfo(): QueryStatus {
    const lastRun = this.running ? "" : (this.runStatus.textContent?.trim() ?? "");
    return {
      lastRun: lastRun.startsWith("Running") ? "" : lastRun,
      transaction: this.transaction?.supported ? transactionLabel(this.transaction) : null,
      transactionOpen: this.transaction?.open === true,
    };
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
    this.transactionIndicator.classList.toggle("failed", state?.failed === true);
    this.transactionIndicator.classList.toggle("unsupported", state?.supported === false);
    this.transactionIndicator.title =
      state?.supported === false
        ? `${this.language.label} runs each command on its own here: this session has no transactions to show.`
        : state?.failed
          ? "The transaction failed: roll it back before running more statements."
          : "Reported by the database after each run";
    if (state?.supported) this.autoCommit.checked = state.autoCommit;
    const [begin, commit, rollback] = this.transactionButtons;
    const usable = transactionButtons(state);
    begin.disabled = !usable.begin;
    begin.title = open ? "A transaction is already open: commit or roll it back first" : "Start a transaction in this tab";
    commit.disabled = !usable.commit;
    rollback.disabled = !usable.rollback;
    this.element?.classList.toggle("tx-open", open);
    this.onChanged?.();
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

  // --- Running ----------------------------------------------------------------------------------------------------------

  /** The selection (or the whole editor), trimmed, and where it starts in the editor. */
  private selectionTarget(): RunTarget | null {
    const raw = selectedOrAllText(this.editor);
    const selection = this.editor.state.selection.main;
    const text = raw.trim();
    return text ? { text, offset: (selection.empty ? 0 : selection.from) + (raw.length - raw.trimStart().length) } : null;
  }

  /** The selection, else the statement under the cursor as the language splits statements. */
  private statementTarget(): RunTarget | null {
    const split = (text: string, offset: number) => (this.support ? this.support.statementAt(text, offset) : statementAt(text, offset));
    const current = currentStatement(this.editor.state, split);
    const text = current?.text.trim() ?? "";
    if (!current || !text) {
      toast("There is no statement at the cursor");
      return null;
    }
    return { text, offset: current.from + (current.text.length - current.text.trimStart().length) };
  }

  private async run(mode: "selection" | "statement"): Promise<void> {
    if (this.running) return;
    const target = mode === "statement" ? this.statementTarget() : this.selectionTarget();
    if (!target) return;
    const { text, offset } = target;
    if (commitsOpenTransaction(this.dialect(), text, this.transaction)) {
      const proceed = await confirmDialog(
        `This runs DDL (or a new BEGIN), which commits the open transaction first on ${this.engine.label}; Rollback will not undo the earlier changes. Run anyway?`,
        "Commit and run",
      );
      if (!proceed) return;
    }
    const database = this.currentDatabase();
    const request: RunRequest = { text, database, offset };
    const startedAt = Date.now();
    // Time spent in the confirmation dialog is not query time, and Stop is only usable while a statement runs.
    const timer = new AttemptTimer();
    const inFlight = (running: boolean) => (this.stopButton.disabled = !running);
    let results = new RunResults();
    this.cancelRequested = false;
    this.setRunning(true);
    try {
      await this.ensureSession();
      const summary = await withConfirmation((confirmation) => {
        // A retry after a confirmation starts over.
        results = this.startResults();
        const current = results;
        return timer.attempt(
          () =>
            this.connection.run(
              this.sessionId,
              request,
              (event) => {
                current.apply(event);
                this.scheduleRender(current);
              },
              confirmation,
            ),
          inFlight,
        );
      });
      if (summary === null) {
        this.resultArea.showMessages([{ text: "Cancelled; nothing ran.", kind: "muted" }]);
        this.runStatus.textContent = "Cancelled";
        return;
      }
      this.lastError = null;
      this.finishResults(results, summary, null, timer.elapsed());
      if (this.transactionsSupported) this.showTransaction(summary.transaction);
      this.record({ sql: text, database, startedAt, durationMs: summary.elapsedMs, results: results.history(), error: null });
    } catch (error) {
      if (this.cancelRequested || isCancelError(error)) {
        // Stopped by the user: not an error. The rows received so far stay in front.
        this.lastError = null;
        this.finishResults(results, null, null, timer.elapsed(), true);
        this.record({ sql: text, database, startedAt, durationMs: timer.elapsed(), results: results.history(), error: errorText(error) });
        void this.refreshTransaction();
        return;
      }
      this.lastError = errorText(error);
      const block = renderError(error, { onJump: (position) => this.jumpTo(text, offset, position) });
      this.finishResults(results, null, block, timer.elapsed());
      showError(error, `Query on ${this.host.connectionName(this.connectionId)}`, { reveal: false });
      this.record({ sql: text, database, startedAt, durationMs: timer.elapsed(), results: results.history(), error: this.lastError });
      void this.refreshTransaction();
    } finally {
      this.setRunning(false);
      this.onChanged?.();
    }
  }

  /** Moves the cursor to an error position reported for `text`, which started at `offset` in the editor. */
  private jumpTo(text: string, offset: number, position: ErrorPosition): void {
    const target = editorOffset(text, offset, position);
    if (target === null) return;
    const anchor = Math.min(target, this.editor.state.doc.length);
    this.editor.dispatch({ selection: { anchor }, scrollIntoView: true });
    this.editor.focus();
  }

  /** Adds a run to the query history in the background; never blocks or fails the run. */
  private record(run: Pick<HistoryEntry, "sql" | "database" | "startedAt" | "durationMs" | "results" | "error">): void {
    if (!this.host.historyEnabled()) return;
    const entry: HistoryEntry = {
      ...run,
      id: "",
      durationMs: Math.max(0, Math.round(run.durationMs)),
      connectionId: this.connectionId,
      connectionName: this.host.connectionName(this.connectionId),
      driver: this.engineId,
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
      await this.connection.cancelRun(this.sessionId);
      toast("Cancel requested");
    } catch (error) {
      showError(error, "Cancel the query");
    }
  }

  private setRunning(running: boolean): void {
    this.running = running;
    this.runButton.disabled = running;
    this.statementButton.disabled = running;
    this.languageSelect.disabled = running;
    // Enabled by the attempt itself (see run), so it stays off while a confirmation dialog is open.
    if (!running) this.stopButton.disabled = true;
    this.element.classList.toggle("busy", running);
  }

  // --- Results ----------------------------------------------------------------------------------------------------------

  /** Clears the previous results for a new attempt. */
  private startResults(): RunResults {
    const results = new RunResults();
    if (this.renderFrame !== null) cancelAnimationFrame(this.renderFrame);
    this.renderFrame = null;
    this.resultArea.start(results);
    this.runStatus.textContent = "Running…";
    return results;
  }

  private scheduleRender(results: RunResults): void {
    if (this.renderFrame !== null) return;
    this.renderFrame = requestAnimationFrame(() => {
      this.renderFrame = null;
      if (this.disposed || !this.running) return;
      this.resultArea.update(results, false);
      this.runStatus.textContent = `Running… ${formatNumber(results.rowCount())} row(s)`;
    });
  }

  /**
   * The end of a run: the last rows, the summary line, and the error block when it failed. `stopped`: the user stopped
   * it (Stop, or a stopped summary), which is not an error: the rows received so far stay in front.
   */
  private finishResults(results: RunResults, summary: RunSummary | null, error: HTMLElement | null, elapsed: number, stopped = false): void {
    if (this.renderFrame !== null) {
      cancelAnimationFrame(this.renderFrame);
      this.renderFrame = null;
    }
    const halted = stopped || summary?.stopped === true;
    this.resultArea.update(results, true, halted ? "stopped" : "failed");
    const ms = Math.round(summary?.elapsedMs ?? elapsed);
    const statements = summary?.statements ?? results.entries.length;
    const rows = results.rowCount();
    const end: Message = halted
      ? { text: `Stopped after ${formatNumber(ms)} ms · ${formatNumber(rows)} row(s) received`, kind: "info" }
      : summary
        ? { text: `Finished in ${formatNumber(ms)} ms · ${formatNumber(statements)} statement(s)`, kind: "muted" }
        : { text: `Failed after ${formatNumber(ms)} ms`, kind: "error" };
    // A failure shows Messages (the error and how far the script got); the result sets that arrived stay in their tabs.
    this.resultArea.finish(end, error, error !== null);
    this.runStatus.textContent = halted
      ? "Stopped"
      : summary
        ? `${formatNumber(statements)} statement(s) · ${formatNumber(rows)} row(s) · ${formatNumber(ms)} ms`
        : "Failed";
  }

  /** The Columns and Cell editor buttons act on the grid of the active result tab. */
  private syncResultTools(): void {
    const grid = this.resultArea.activeGrid();
    this.columnsButton.disabled = !grid;
    this.cellButton.disabled = !grid;
  }

  private toggleHistory(open = this.history.element.hidden, remember = true): void {
    this.history.element.hidden = !open;
    this.historyButton.classList.toggle("active", open);
    this.historyButton.setAttribute("aria-pressed", String(open));
    if (open) this.history.show();
    if (remember) writeHistoryPanelOpen(open);
  }

  private resizeEditor(delta: number): void {
    const maximum = this.element.clientHeight - MIN_RESULTS_HEIGHT;
    const height = clamp(this.editorHost.offsetHeight + delta, MIN_EDITOR_HEIGHT, maximum);
    this.editorHost.style.height = `${height}px`;
  }
}

/** A language for an engine whose descriptor lists none (an unknown engine id): plain text. */
function fallbackLanguage(engine: EngineDescriptor): LanguageSpec {
  return { id: "text", label: "Text", editor: "text", dialect: engine.sqlDialect, default: true, hint: "Ctrl+Enter runs the query.", fences: [] };
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
