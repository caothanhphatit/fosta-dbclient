/**
 * The result area of a query tab: a tab per result set with its live row count (its grid is built when the tab is first
 * shown), or, above `RESULT_TABS_MAX` result sets, one list of them where a click opens that result; then Messages.
 * Updated incrementally while a run streams: each frame touches only the entries that changed (`RunResults.takeChanged`),
 * so a script of thousands of statements never rebuilds thousands of tabs, grids or message lines.
 */
import type { ColumnInfo, Value, ValueKind } from "../../core/contract";
import { formatNumber, h } from "../../core/dom";
import type { SqlDialect } from "../../core/sql";
import { DataGrid, gridSetup } from "../grid/grid";
import { describeResult, resultLayout, statementExcerpt, type RunResults, type StatementResult } from "./runResults";

export interface Message {
  text: string;
  kind: "info" | "error" | "muted" | "notice";
}

/** While rows stream in, a grid (and the list of result sets) is reloaded at most this often. */
const GRID_RELOAD_MS = 1000;

interface Panel {
  element: HTMLElement;
  button: HTMLButtonElement;
  titleLabel: HTMLElement;
  countLabel: HTMLElement;
  /** Built on first show (result tabs), or at once (the list). */
  grid: DataGrid | null;
  /** The result set shown; the list's detail tab: the one opened last. */
  entry: StatementResult | null;
  loadedRows: number;
  loadedAt: number;
  /** The list: its rows as last loaded (JSON), so an unchanged list is not loaded again. */
  loadedSignature?: string;
}

interface MessageGroup {
  line: HTMLElement;
  group: HTMLElement;
  notices: number;
}

export class ResultArea {
  readonly tabs = h("div", { class: "result-tabs" });
  readonly body = h("div", { class: "result-body" });
  /** The active tab changed (the Columns and Cell editor buttons follow its grid). */
  onActiveChange: (() => void) | null = null;
  private panels: Panel[] = [];
  private active: Panel | null = null;
  private messages: Panel;
  private messageGroups = new Map<StatementResult, MessageGroup>();
  private placeholder: HTMLElement | null = null;
  private resultPanels: Panel[] = [];
  private layout: "tabs" | "list" = "tabs";
  private list: Panel | null = null;
  private detail: Panel | null = null;
  private results: RunResults | null = null;
  /** Whether this run already brought a result forward (later ones wait behind their tabs). */
  private resultShown = false;

  constructor(private readonly dialect: () => SqlDialect) {
    this.messages = this.reset();
  }

  /** Only the Messages tab, with `lines` (the hint before a run, "Cancelled; nothing ran"). */
  showMessages(lines: Message[]): void {
    this.reset();
    this.messages.element.append(...messageLines(lines));
  }

  /** A new run: Messages shows "Running…" until the first statement reports. */
  start(results: RunResults): void {
    this.reset();
    this.results = results;
    this.placeholder = messageLines([{ text: "Running…", kind: "muted" }])[0];
    this.messages.element.append(this.placeholder);
  }

  /** `start`, unless the area already shows `results` (a run that failed before it started still clears the last one). */
  begin(results: RunResults): void {
    if (results !== this.results) this.start(results);
  }

  /** Brings the tabs (or the list), their counts and the Messages lines up to date with what arrived. */
  update(results: RunResults, final: boolean, unfinished = "running…"): void {
    if (results !== this.results) return;
    const now = performance.now();
    const changed = results.takeChanged();
    const sets = results.resultSets();
    if (this.layout === "tabs" && resultLayout(sets.length) === "list") this.switchToList();
    if (this.layout === "tabs") this.updateTabs(sets, final, now);
    else this.updateList(sets, changed, final, now, unfinished);
    this.updateMessages(changed, unfinished);
    if (final) {
      // The statement that failed (or was stopped) never got its `done`: say so on its line.
      for (const entry of results.entries) if (!entry.done) this.describe(entry, unfinished);
      this.placeholder?.remove();
      this.placeholder = null;
    }
  }

  /** The end of a run: its last line and the error block; `reveal` brings Messages forward (a failure). */
  finish(end: Message, error: HTMLElement | null, reveal: boolean): void {
    this.placeholder?.remove();
    this.placeholder = null;
    this.messages.element.append(...messageLines([end]));
    if (error) this.messages.element.append(error);
    if (reveal) this.activate(this.messages);
  }

  activeGrid(): DataGrid | null {
    return this.active?.grid ?? null;
  }

  dispose(): void {
    for (const panel of this.panels) panel.grid?.dispose();
    this.panels = [];
  }

  // --- Tabs ---------------------------------------------------------------------------------------------------------------

  private updateTabs(sets: readonly StatementResult[], final: boolean, now: number): void {
    for (let index = this.resultPanels.length; index < sets.length; index += 1) {
      const panel = this.panel("Result", sets[index]);
      this.resultPanels.push(panel);
      this.insertBeforeMessages(panel);
    }
    const many = this.resultPanels.length > 1;
    this.resultPanels.forEach((panel, index) => {
      const entry = panel.entry as StatementResult;
      panel.titleLabel.textContent = many ? `Result ${index + 1}` : "Result";
      panel.countLabel.textContent = rowCount(entry);
      if (panel.grid) this.reloadGrid(panel, entry, final, now);
    });
    if (!this.resultShown && this.resultPanels.length > 0) {
      this.resultShown = true;
      this.activate(this.resultPanels[0]);
    }
  }

  /** Over the tab limit: the result tabs (and their grids) give way to one list and a tab for the result opened from it. */
  private switchToList(): void {
    const wasResult = this.active !== null && this.resultPanels.includes(this.active);
    for (const panel of this.resultPanels) {
      panel.grid?.dispose();
      panel.button.remove();
    }
    this.panels = this.panels.filter((panel) => !this.resultPanels.includes(panel));
    this.resultPanels = [];
    this.layout = "list";
    const list = this.panel("Result sets", null);
    list.grid = new DataGrid();
    list.element = h(
      "div",
      { class: "result-list" },
      h("div", { class: "result-list-hint" }, "Too many result sets for a tab each: click one to open it. Filter or sort the list by its headers."),
      list.grid.element,
    );
    list.grid.element.addEventListener("click", (event) => {
      if (event.target instanceof Element && event.target.closest("tbody tr")) this.openSelected();
    });
    this.list = list;
    this.insertBeforeMessages(list);
    const detail = this.panel("Result", null);
    detail.button.hidden = true;
    this.detail = detail;
    this.insertBeforeMessages(detail);
    this.resultShown = true;
    if (wasResult || this.active === null) this.activate(list);
  }

  private updateList(sets: readonly StatementResult[], changed: StatementResult[], final: boolean, now: number, unfinished: string): void {
    const list = this.list;
    if (!list?.grid) return;
    list.countLabel.textContent = formatNumber(sets.length);
    if (final || list.loadedRows === 0 || now - list.loadedAt > GRID_RELOAD_MS) {
      const rows = sets.map((entry) => listRow(entry, unfinished));
      // Reloaded only when a row changed, so the list's sort and selection are not redone every second for nothing.
      const signature = JSON.stringify(rows);
      if (signature !== list.loadedSignature) {
        list.grid.load({
          ...gridSetup(LIST_COLUMNS, rows, this.dialect()),
          clientFilter: true,
          clientSort: true,
          extraMenu: () => [{ label: "Open this result", icon: "expand", run: () => this.openSelected() }],
        });
        list.loadedSignature = signature;
      }
      list.loadedRows = sets.length;
      list.loadedAt = now;
    }
    const detail = this.detail;
    if (detail?.entry && detail.grid && (final || changed.includes(detail.entry))) {
      detail.countLabel.textContent = rowCount(detail.entry);
      this.reloadGrid(detail, detail.entry, final, now);
    }
  }

  /** The row picked in the list opens in the detail tab. */
  private openSelected(): void {
    const row = this.list?.grid?.selectedRow();
    const cell = row?.[0];
    const position = cell && cell.t === "int" ? Number(cell.v) - 1 : -1;
    const entry = this.results?.entries[position];
    const detail = this.detail;
    if (!entry || !detail) return;
    detail.entry = entry;
    detail.loadedRows = -1;
    detail.titleLabel.textContent = `Result #${position + 1}`;
    detail.countLabel.textContent = rowCount(entry);
    detail.button.title = statementExcerpt(entry.sql, 200);
    detail.button.hidden = false;
    detail.grid ??= new DataGrid();
    detail.element = detail.grid.element;
    this.reloadGrid(detail, entry, true, performance.now());
    this.activate(detail);
  }

  // --- Messages -----------------------------------------------------------------------------------------------------------

  private updateMessages(changed: StatementResult[], unfinished: string): void {
    for (const entry of changed) {
      let group = this.messageGroups.get(entry);
      if (!group) {
        this.placeholder?.remove();
        this.placeholder = null;
        const line = h("div", { class: "message info" });
        group = { line, group: h("div", { class: "message-group" }, line), notices: 0 };
        this.messageGroups.set(entry, group);
        this.messages.element.append(group.group);
      }
      this.describe(entry, unfinished);
      for (; group.notices < entry.notices.length; group.notices += 1) {
        group.group.append(h("div", { class: "message notice" }, entry.notices[group.notices]));
      }
    }
  }

  private describe(entry: StatementResult, unfinished: string): void {
    const group = this.messageGroups.get(entry);
    if (group) group.line.textContent = describeResult(entry, entry.position, unfinished);
  }

  // --- Panels -------------------------------------------------------------------------------------------------------------

  /** Drops every tab and starts over with an empty Messages tab. */
  private reset(): Panel {
    this.dispose();
    this.resultPanels = [];
    this.messageGroups = new Map();
    this.placeholder = null;
    this.layout = "tabs";
    this.list = null;
    this.detail = null;
    this.results = null;
    this.resultShown = false;
    this.active = null;
    const messages = this.panel("Messages", null);
    messages.element = h("div", { class: "messages" });
    this.messages = messages;
    this.tabs.replaceChildren(messages.button);
    this.panels = [messages];
    this.activate(messages);
    return messages;
  }

  private panel(title: string, entry: StatementResult | null): Panel {
    const titleLabel = h("span", {}, title);
    const countLabel = h("span", { class: "result-count" });
    const button = h("button", { class: "result-tab", type: "button", title: entry ? statementExcerpt(entry.sql, 200) : undefined }, titleLabel, countLabel);
    const panel: Panel = { element: h("div"), button, titleLabel, countLabel, grid: null, entry, loadedRows: 0, loadedAt: 0 };
    button.addEventListener("click", () => this.activate(panel));
    return panel;
  }

  private insertBeforeMessages(panel: Panel): void {
    this.tabs.insertBefore(panel.button, this.messages.button);
    this.panels.splice(this.panels.length - 1, 0, panel);
  }

  private activate(panel: Panel): void {
    if (panel.entry && !panel.grid && this.resultPanels.includes(panel)) {
      // A result tab builds its grid when it is first shown.
      panel.grid = new DataGrid();
      panel.element = panel.grid.element;
      this.reloadGrid(panel, panel.entry, true, performance.now());
    }
    for (const item of this.panels) item.button.classList.toggle("active", item === panel);
    if (this.active !== panel || this.body.firstChild !== panel.element) this.body.replaceChildren(panel.element);
    this.active = panel;
    this.onActiveChange?.();
  }

  private reloadGrid(panel: Panel, entry: StatementResult, force: boolean, now: number): void {
    const stale = entry.rows.length !== panel.loadedRows;
    if (!panel.grid || !stale || !(force || entry.done !== null || panel.loadedRows <= 0 || now - panel.loadedAt > GRID_RELOAD_MS)) return;
    panel.grid.load({ ...gridSetup(entry.columns ?? [], entry.rows.slice(), this.dialect()), clientFilter: true, clientSort: true });
    panel.loadedRows = entry.rows.length;
    panel.loadedAt = now;
  }
}

function rowCount(entry: StatementResult): string {
  const rows = entry.done?.rows ?? entry.rows.length;
  return `${formatNumber(rows)}${entry.done?.truncated ? "+" : ""}`;
}

function messageLines(messages: Message[]): HTMLElement[] {
  return messages.map((message) => h("div", { class: `message ${message.kind}` }, message.text));
}

function listColumn(name: string, kind: ValueKind["kind"]): ColumnInfo {
  return { name, typeName: "", kind: { kind } as ValueKind, nullable: true, default: null, comment: null, identity: null, generated: null, hints: {} };
}

/** The list of result sets: statement number, its first line, rows, time and status. */
const LIST_COLUMNS: ColumnInfo[] = [
  listColumn("#", "int"),
  listColumn("Statement", "text"),
  listColumn("Rows", "int"),
  listColumn("Time (ms)", "int"),
  listColumn("Status", "text"),
];

/** `unfinished`: the status of a result set without its `done` ("running…" while the run goes on, "failed", "stopped"). */
function listRow(entry: StatementResult, unfinished: string): Value[] {
  const done = entry.done;
  const status = !done ? unfinished.replace("…", "") : done.truncated ? "truncated" : "done";
  return [
    { t: "int", v: entry.position + 1 },
    { t: "text", v: statementExcerpt(entry.sql, 200) },
    { t: "int", v: done?.rows ?? entry.rows.length },
    done ? { t: "int", v: Math.round(done.elapsedMs) } : { t: "null", v: null },
    { t: "text", v: status },
  ];
}
