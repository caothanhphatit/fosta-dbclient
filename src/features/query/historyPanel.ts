/**
 * Side panel of a query tab listing earlier runs (from the backend's history file): search, an
 * "All connections" filter, status, relative time, duration and the first line of SQL. Selecting an
 * entry shows its full text; double-click or Insert puts it into the editor without running it. An entry its engine
 * can reopen elsewhere (a MongoDB query-bar run: its collection workspace) gets that action too.
 */
import { showError } from "../../core/errorPanel";
import { api } from "../../core/api";
import { confirmDialog, toast } from "../../core/dialogs";
import { copyText, h, icon, iconButton, type IconName } from "../../core/dom";
import type { HistoryEntry } from "../../core/types";
import { historyTargetOf, type HistoryTarget } from "../../core/views";
import { firstLine, formatDuration, historyStatus, relativeTime, resultSummary, type HistoryStatus } from "./historyFormat";
import "./query-history.css";

export interface HistoryPanelHost {
  connectionId: string;
  enabled(): boolean;
  insert(sql: string): void;
  openInNewTab(entry: HistoryEntry): void;
  /** Opens an entry where its engine reopens it (a workspace section); absent: entries reopen as text only. */
  openTarget?(target: HistoryTarget): void;
  close(): void;
}

const PAGE_SIZE = 200;
const SEARCH_DEBOUNCE_MS = 250;
const STATUS_ICONS: Record<HistoryStatus, IconName> = { ok: "check", error: "x", cancelled: "stop" };
const STATUS_LABELS: Record<HistoryStatus, string> = { ok: "Succeeded", error: "Failed", cancelled: "Cancelled" };

export class HistoryPanel {
  readonly element: HTMLElement;
  private readonly host: HistoryPanelHost;
  private readonly search = h("input", { class: "input history-search", placeholder: "Search history", spellcheck: "false", type: "search" });
  private readonly allConnections = h("input", { type: "checkbox" });
  private readonly notice = h("div", { class: "history-notice" });
  private readonly list = h("div", { class: "history-list", role: "listbox", tabIndex: 0, "aria-label": "Query history" });
  private readonly detail = h("div", { class: "history-detail" });
  private entries: HistoryEntry[] = [];
  private selectedId: string | null = null;
  private hasMore = false;
  private loaded = false;
  private request = 0;
  private searchTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(host: HistoryPanelHost) {
    this.host = host;
    this.search.addEventListener("input", () => {
      if (this.searchTimer) clearTimeout(this.searchTimer);
      this.searchTimer = setTimeout(() => void this.refresh(), SEARCH_DEBOUNCE_MS);
    });
    this.search.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && this.search.value) {
        event.stopPropagation();
        this.search.value = "";
        void this.refresh();
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        this.moveSelection(1);
        this.list.focus();
      }
    });
    this.allConnections.addEventListener("change", () => void this.refresh());
    this.list.addEventListener("keydown", (event) => this.handleKey(event));
    this.element = h(
      "aside",
      { class: "history-panel" },
      h(
        "div",
        { class: "panel-header" },
        h("span", {}, "History"),
        h("span", { class: "spacer" }),
        iconButton("refresh", "Reload history", () => void this.refresh()),
        iconButton("trash", "Clear all history…", () => void this.clearAll()),
        iconButton("x", "Close history", () => host.close()),
      ),
      h(
        "div",
        { class: "history-filters" },
        h("div", { class: "search-box" }, icon("search"), this.search),
        h("label", { class: "checkbox" }, this.allConnections, h("span", {}, "All connections")),
      ),
      this.notice,
      this.list,
      this.detail,
    );
  }

  /** Loads the first page on first show; later calls reload. */
  show(): void {
    this.updateNotice();
    if (!this.loaded) void this.refresh();
    else this.renderList();
  }

  focusSearch(): void {
    this.search.focus();
  }

  /** A run of this tab was just recorded. */
  added(entry: HistoryEntry): void {
    if (!this.loaded || !this.matches(entry)) return;
    this.entries = [entry, ...this.entries.filter((item) => item.id !== entry.id)];
    this.renderList();
  }

  async refresh(): Promise<void> {
    const request = ++this.request;
    this.updateNotice();
    try {
      const entries = await api.historyList(this.query());
      if (request !== this.request) return;
      this.entries = entries;
      this.hasMore = entries.length >= PAGE_SIZE;
      this.loaded = true;
      if (!entries.some((entry) => entry.id === this.selectedId)) this.selectedId = null;
      this.renderList();
    } catch (error) {
      showError(error, "Load the query history");
    }
  }

  private async loadMore(): Promise<void> {
    const last = this.entries[this.entries.length - 1];
    if (!last) return;
    const request = ++this.request;
    try {
      const more = await api.historyList({ ...this.query(), before: last.startedAt });
      if (request !== this.request) return;
      const known = new Set(this.entries.map((entry) => entry.id));
      this.entries = [...this.entries, ...more.filter((entry) => !known.has(entry.id))];
      this.hasMore = more.length >= PAGE_SIZE;
      this.renderList();
    } catch (error) {
      showError(error, "Load more history");
    }
  }

  private query() {
    const search = this.search.value.trim();
    return {
      connectionId: this.allConnections.checked ? undefined : this.host.connectionId,
      search: search || undefined,
      limit: PAGE_SIZE,
    };
  }

  private matches(entry: HistoryEntry): boolean {
    if (!this.allConnections.checked && entry.connectionId !== this.host.connectionId) return false;
    const search = this.search.value.trim().toLowerCase();
    if (!search) return true;
    return [entry.sql, entry.connectionName, entry.database ?? ""].some((text) => text.toLowerCase().includes(search));
  }

  private updateNotice(): void {
    const enabled = this.host.enabled();
    this.notice.hidden = enabled;
    this.notice.textContent = enabled ? "" : "History is turned off, so new runs are not recorded. Turn it on in Settings → Record query history.";
  }

  private renderList(): void {
    const now = Date.now();
    const rows = this.entries.map((entry) => this.row(entry, now));
    if (rows.length === 0) {
      const searching = this.search.value.trim() !== "";
      rows.push(h("div", { class: "empty-hint" }, searching ? "No run matches the search" : "No runs"));
    }
    if (this.hasMore) {
      const more = h("button", { class: "link-button history-more", type: "button" }, "Load older runs");
      more.addEventListener("click", () => void this.loadMore());
      rows.push(more);
    }
    this.list.replaceChildren(...rows);
    this.renderDetail();
  }

  private row(entry: HistoryEntry, now: number): HTMLElement {
    const status = historyStatus(entry);
    const meta = [relativeTime(entry.startedAt, now), formatDuration(entry.durationMs), resultSummary(entry)];
    if (this.allConnections.checked) meta.unshift(entry.connectionName || entry.connectionId);
    const row = h(
      "div",
      {
        class: `history-row${entry.id === this.selectedId ? " selected" : ""}`,
        role: "option",
        "aria-selected": entry.id === this.selectedId ? "true" : "false",
        "data-id": entry.id,
        title: `${new Date(entry.startedAt).toLocaleString()}\nDouble-click to insert into the editor`,
      },
      h("span", { class: `history-status ${status}`, title: STATUS_LABELS[status] }, icon(STATUS_ICONS[status])),
      h(
        "div",
        { class: "history-text" },
        h("div", { class: "history-sql" }, firstLine(entry.sql) || "(empty)"),
        h("div", { class: `history-meta ${status}` }, meta.join(" · ")),
      ),
    );
    row.addEventListener("click", () => this.select(entry.id));
    row.addEventListener("dblclick", () => this.host.insert(entry.sql));
    return row;
  }

  private select(id: string | null): void {
    this.selectedId = id;
    for (const row of this.list.querySelectorAll<HTMLElement>(".history-row")) {
      const selected = row.dataset.id === id;
      row.classList.toggle("selected", selected);
      row.setAttribute("aria-selected", selected ? "true" : "false");
      if (selected) row.scrollIntoView({ block: "nearest" });
    }
    this.renderDetail();
  }

  private moveSelection(delta: number): void {
    if (this.entries.length === 0) return;
    const index = this.entries.findIndex((entry) => entry.id === this.selectedId);
    const next = index < 0 ? 0 : Math.min(Math.max(index + delta, 0), this.entries.length - 1);
    this.select(this.entries[next].id);
  }

  private handleKey(event: KeyboardEvent): void {
    const entry = this.selected();
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      this.moveSelection(event.key === "ArrowDown" ? 1 : -1);
    } else if (event.key === "Enter" && entry) {
      event.preventDefault();
      this.host.insert(entry.sql);
    } else if (event.key === "Delete" && entry) {
      event.preventDefault();
      void this.remove(entry);
    }
  }

  private selected(): HistoryEntry | null {
    return this.entries.find((entry) => entry.id === this.selectedId) ?? null;
  }

  private renderDetail(): void {
    const entry = this.selected();
    this.detail.hidden = entry === null;
    if (!entry) {
      this.detail.replaceChildren();
      return;
    }
    const status = historyStatus(entry);
    const facts = [
      new Date(entry.startedAt).toLocaleString(),
      formatDuration(entry.durationMs),
      entry.connectionName || entry.connectionId,
      entry.database ?? "(no database)",
    ];
    const outcome = entry.error ?? resultSummary(entry);
    const action = (label: string, iconName: IconName, run: () => void, title = label) => {
      const button = h("button", { class: "btn small", type: "button", title }, icon(iconName), h("span", {}, label));
      button.addEventListener("click", run);
      return button;
    };
    this.detail.replaceChildren(
      h("pre", { class: "history-full" }, entry.sql),
      h("div", { class: "history-facts" }, facts.join(" · ")),
      h("div", { class: `history-outcome ${status}` }, status === "cancelled" ? `Cancelled${entry.error ? `: ${entry.error}` : ""}` : outcome),
      h(
        "div",
        { class: "history-actions" },
        action("Insert", "plus", () => this.host.insert(entry.sql), "Put the text into the editor (does not run it)"),
        action("New tab", "query", () => this.host.openInNewTab(entry), "Open the text in a new query tab"),
        action("Copy", "copy", () => void copyText(entry.sql).then(() => toast("Copied", "success"))),
        action("Delete", "trash", () => void this.remove(entry), "Delete this entry from the history"),
      ),
    );
    void this.addTargetAction(entry, action);
  }

  /** Adds the engine's "Open in …" action to the detail of `entry` once it is known (while it is still selected). */
  private async addTargetAction(
    entry: HistoryEntry,
    action: (label: string, iconName: IconName, run: () => void, title?: string) => HTMLElement,
  ): Promise<void> {
    const open = this.host.openTarget;
    if (!open) return;
    const target = await historyTargetOf(entry);
    if (!target || this.selectedId !== entry.id) return;
    const actions = this.detail.querySelector(".history-actions");
    actions?.prepend(action(target.label, "table", () => open(target), "Open the query where it ran, filled in (does not edit it here)"));
  }

  private async remove(entry: HistoryEntry): Promise<void> {
    try {
      await api.historyDelete([entry.id]);
      const index = this.entries.findIndex((item) => item.id === entry.id);
      this.entries = this.entries.filter((item) => item.id !== entry.id);
      this.selectedId = this.entries[Math.min(index, this.entries.length - 1)]?.id ?? null;
      this.renderList();
    } catch (error) {
      showError(error, "Delete history entries");
    }
  }

  private async clearAll(): Promise<void> {
    if (!(await confirmDialog("Delete the whole query history of every connection on this machine?", "Delete"))) return;
    try {
      await api.historyClear();
      this.entries = [];
      this.selectedId = null;
      this.hasMore = false;
      this.renderList();
      toast("Query history cleared", "success");
    } catch (error) {
      showError(error, "Clear the query history");
    }
  }
}
