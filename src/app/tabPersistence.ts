/**
 * Saves the open tabs of every workspace (backend `save_open_tabs`) a second after they change, and brings them back on
 * start (`restore`): each tab comes back as a `RestoredTab` that connects only when shown.
 */
import { api } from "../core/api";
import { capabilities } from "../core/capabilities";
import { toast } from "../core/dialogs";
import { showError } from "../core/errorPanel";
import type { TabManager } from "../core/tabs";
import type { AppConfig } from "../core/types";
import type { AppNavigation } from "./navigation";
import { parseSavedTabs, restorableTabs, serializeTabs, WORKSPACE_TEXT_BUDGET, type SavedTab } from "./tabState";

const SAVE_DELAY_MS = 1000;

export class TabPersistence {
  private timer: number | null = null;
  /** What was saved last per workspace (as JSON), so unchanged workspaces are not written again. */
  private readonly saved = new Map<string, string>();
  /** Off until `restore` is done, so an early save cannot wipe the tabs not restored yet. */
  private ready = false;
  private failureReported = false;

  constructor(
    private readonly tabs: TabManager,
    private readonly navigation: AppNavigation,
    private readonly workspaceIds: () => string[],
  ) {}

  /** Restores the saved tabs of every workspace in `config`; tabs of connections that are gone are dropped (one toast). */
  async restore(config: AppConfig): Promise<void> {
    try {
      const stored = await api.loadOpenTabs().catch((error: unknown) => {
        console.warn("Could not read the open tabs of the last session", error);
        return {} as Record<string, unknown>;
      });
      const connections = new Set(config.workspaces.flatMap((workspace) => workspace.connections.map((profile) => profile.id)));
      const tasks = new Set(config.workspaces.flatMap((workspace) => workspace.tasks.map((task) => task.id)));
      let dropped = 0;
      for (const workspace of config.workspaces) {
        const parsed = parseSavedTabs(stored[workspace.id]);
        if (!parsed) continue;
        this.saved.set(workspace.id, JSON.stringify(parsed));
        const usable = restorableTabs(
          parsed,
          (id) => connections.has(id),
          (id) => tasks.has(id),
        );
        dropped += usable.dropped;
        const keys = usable.tabs.map((tab) => this.navigation.addRestored(workspace.id, tab));
        const active = usable.active !== null ? keys[usable.active] : undefined;
        if (active) this.tabs.rememberActive(workspace.id, active);
      }
      if (dropped > 0) toast(`${dropped} tab(s) were not restored: their connection no longer exists`);
    } finally {
      this.ready = true;
      this.schedule();
    }
  }

  /** A tab opened, closed or changed, or a query text was edited: save in a second. */
  schedule(): void {
    if (!this.ready) return;
    if (this.timer !== null) window.clearTimeout(this.timer);
    this.timer = window.setTimeout(() => void this.flush(), SAVE_DELAY_MS);
  }

  /** Saves now (before quitting). */
  async flush(): Promise<void> {
    if (this.timer !== null) window.clearTimeout(this.timer);
    this.timer = null;
    if (!this.ready) return;
    this.navigation.pruneRecords();
    const budget = WORKSPACE_TEXT_BUDGET[capabilities().edition === "web" ? "web" : "desktop"];
    const writes: Promise<void>[] = [];
    for (const workspaceId of this.workspaceIds()) {
      const { keys, active } = this.tabs.layout(workspaceId);
      const entries = keys
        .map((key) => ({ key, tab: this.navigation.savedTab(key) }))
        .filter((entry): entry is { key: string; tab: SavedTab } => entry.tab !== null);
      const activeIndex = entries.findIndex((entry) => entry.key === active);
      const state = serializeTabs(
        entries.map((entry) => entry.tab),
        activeIndex >= 0 ? activeIndex : null,
        budget,
      );
      const json = JSON.stringify(state);
      if (this.saved.get(workspaceId) === json || (state.tabs.length === 0 && !this.saved.has(workspaceId))) continue;
      writes.push(
        api.saveOpenTabs(workspaceId, state).then(
          () => void this.saved.set(workspaceId, json),
          (error: unknown) => this.reportFailure(error),
        ),
      );
    }
    await Promise.all(writes);
  }

  /** A deleted workspace: its saved tabs go too. */
  forget(workspaceId: string): void {
    this.saved.delete(workspaceId);
    void api.saveOpenTabs(workspaceId, null).catch(() => undefined);
  }

  private reportFailure(error: unknown): void {
    if (this.failureReported) return;
    this.failureReported = true;
    showError(error, "Save the open tabs (they will not come back after a restart)");
  }
}
