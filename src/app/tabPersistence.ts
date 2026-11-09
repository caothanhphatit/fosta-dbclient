/**
 * Saves the open tabs of every workspace (backend `save_open_tabs`) a second after they change, and brings them back on
 * start (`restore`): each tab comes back as a `RestoredTab` that connects only when shown.
 *
 * - **One save at a time.** Every save (debounced, before quitting, before an update restart) is chained on one promise,
 *   so saves land in order and quitting waits for a save still in flight.
 * - **Several windows** (the web edition open in two browser tabs): each save names the revision it is based on. When
 *   another window saved in between, the backend writes nothing and answers with its state; this window then keeps the
 *   other window's tabs that it does not show (`mergeTabs`, by tab id) and saves again. A tab one window closed but
 *   another still shows comes back with the other window's next save.
 * - **Leaving the page** (web): `saveOnLeave` sends what changed at once with `keepalive`, so the request outlives the
 *   page; browsers allow 64 KB of such bodies, so a larger state is saved by an ordinary save instead (usually when the
 *   page is hidden, before it unloads). The page's hide, unload and close events each call it; only changed state is
 *   sent, so they do not send the same save three times.
 */
import { api, type OpenTabsSaveOutcome, type StoredOpenTabs } from "../core/api";
import { capabilities } from "../core/capabilities";
import { toast } from "../core/dialogs";
import { showError } from "../core/errorPanel";
import type { TabManager } from "../core/tabs";
import type { AppConfig } from "../core/types";
import type { AppNavigation } from "./navigation";
import {
  fitToBytes,
  mergeTabs,
  parseSavedTabs,
  restorableTabs,
  serializeTabs,
  WORKSPACE_MAX_BYTES,
  WORKSPACE_TEXT_BUDGET,
  type SavedTab,
  type SavedWorkspaceTabs,
} from "./tabState";

const SAVE_DELAY_MS = 1000;
/** What `keepalive` requests may carry in all (browsers allow 64 KB), with room for the request around the body. */
const KEEPALIVE_BYTES = 60_000;
/** A conflict is merged and saved again this many times before the save gives up until the next change. */
const SAVE_ATTEMPTS = 3;

/** What this window knows of one workspace's stored tabs. */
interface WorkspaceRecord {
  /** The revision the stored tabs have (0: none stored). */
  revision: number;
  /** The stored tabs as last read or written (null: none). */
  stored: SavedWorkspaceTabs | null;
  /** The ids of the tabs this window showed when it last saved (or restored): closing one of them keeps it closed. */
  shown: Set<string>;
  /** The JSON last written (or restored), so an unchanged workspace is not written again. */
  json: string | null;
}

interface Plan {
  /** This window's tabs of the workspace, bounded. */
  local: SavedWorkspaceTabs;
  /** The tab key of each of `local.tabs`. */
  keys: string[];
}

export class TabPersistence {
  private timer: number | null = null;
  private readonly records = new Map<string, WorkspaceRecord>();
  /** The JSON of a save on its way, per workspace (a `keepalive` save while leaving the page, or a chained one). */
  private readonly pending = new Map<string, string>();
  /** Every save runs after the one before. */
  private chain: Promise<void> = Promise.resolve();
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
        return { workspaces: {}, revisions: {} } as StoredOpenTabs;
      });
      const connections = new Set(config.workspaces.flatMap((workspace) => workspace.connections.map((profile) => profile.id)));
      const tasks = new Set(config.workspaces.flatMap((workspace) => workspace.tasks.map((task) => task.id)));
      let dropped = 0;
      for (const workspace of config.workspaces) {
        const parsed = parseSavedTabs(stored.workspaces[workspace.id]);
        if (!parsed) continue;
        // Dropped tabs count as shown and closed here, so they are not kept as another window's.
        this.records.set(workspace.id, {
          revision: stored.revisions[workspace.id] ?? 0,
          stored: parsed,
          shown: new Set(parsed.tabs.map((tab) => tab.id)),
          json: JSON.stringify(parsed),
        });
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

  /** Saves now, after any save still on its way (before quitting); resolves once this save is done. */
  flush(): Promise<void> {
    this.clearTimer();
    if (!this.ready) return this.chain;
    this.chain = this.chain.then(() => this.saveAll());
    return this.chain;
  }

  /**
   * The web page is being hidden or left: sends each changed workspace at once with `keepalive` (a request that outlives
   * the page) while the bodies fit the browser's 64 KB; the rest is saved by an ordinary (chained) save.
   */
  saveOnLeave(): void {
    if (!this.ready) return;
    this.clearTimer();
    this.navigation.pruneRecords();
    let budget = KEEPALIVE_BYTES;
    let rest = false;
    for (const [workspaceId, plan] of this.plans()) {
      const state = this.payload(workspaceId, plan.local);
      const json = JSON.stringify(state);
      if (!this.needsSave(workspaceId, state, json)) continue;
      // A save of this window on its way would be based on the same revision: this one replaces it whatever lands first.
      const revision = this.pending.has(workspaceId) ? null : (this.records.get(workspaceId)?.revision ?? 0);
      const bytes = new TextEncoder().encode(JSON.stringify({ workspaceId, tabs: state, revision })).length;
      if (bytes > budget) {
        rest = true;
        continue;
      }
      budget -= bytes;
      this.pending.set(workspaceId, json);
      void api
        .saveOpenTabsOnUnload(workspaceId, state, revision)
        .then((outcome) => this.settled(workspaceId, plan, state, json, outcome))
        .catch((error: unknown) => this.reportFailure(error))
        .finally(() => {
          if (this.pending.get(workspaceId) === json) this.pending.delete(workspaceId);
        });
    }
    if (rest) void this.flush();
  }

  /** Tabs whose query text is too large to keep (they would come back empty), so quitting can say so. */
  textLostKeys(): Set<string> {
    const lost = new Set<string>();
    for (const [workspaceId, { local, keys }] of this.plans()) {
      const state = this.payload(workspaceId, local);
      keys.forEach((key, index) => {
        const tab = state.tabs[index];
        if (tab.kind === "query" && tab.textOmitted) lost.add(key);
      });
    }
    return lost;
  }

  /** A deleted workspace: its saved tabs go too (after any save on its way). */
  forget(workspaceId: string): void {
    this.records.delete(workspaceId);
    this.chain = this.chain.then(() =>
      api.saveOpenTabs(workspaceId, null).then(
        () => undefined,
        () => undefined,
      ),
    );
  }

  private clearTimer(): void {
    if (this.timer !== null) window.clearTimeout(this.timer);
    this.timer = null;
  }

  /** One save of every workspace that changed; never rejects (a failure is reported once). */
  private async saveAll(): Promise<void> {
    this.navigation.pruneRecords();
    for (const [workspaceId, plan] of this.plans()) {
      try {
        await this.saveWorkspace(workspaceId, plan);
      } catch (error) {
        this.reportFailure(error);
      }
    }
  }

  private async saveWorkspace(workspaceId: string, plan: Plan): Promise<void> {
    for (let attempt = 0; attempt < SAVE_ATTEMPTS; attempt += 1) {
      const state = this.payload(workspaceId, plan.local);
      const json = JSON.stringify(state);
      if (!this.needsSave(workspaceId, state, json)) return;
      this.pending.set(workspaceId, json);
      let outcome: OpenTabsSaveOutcome;
      try {
        outcome = await api.saveOpenTabs(workspaceId, state, this.records.get(workspaceId)?.revision ?? 0);
      } finally {
        if (this.pending.get(workspaceId) === json) this.pending.delete(workspaceId);
      }
      if (this.settled(workspaceId, plan, state, json, outcome)) return;
    }
  }

  /** Records a save's answer; false on a conflict (the stored state is now the other window's, to merge with). */
  private settled(workspaceId: string, plan: Plan, state: SavedWorkspaceTabs, json: string, outcome: OpenTabsSaveOutcome): boolean {
    if (outcome.saved) {
      this.records.set(workspaceId, { revision: outcome.revision, stored: state, shown: new Set(plan.local.tabs.map((tab) => tab.id)), json });
      return true;
    }
    const record = this.records.get(workspaceId);
    this.records.set(workspaceId, {
      revision: outcome.revision,
      stored: parseSavedTabs(outcome.current),
      shown: record?.shown ?? new Set(),
      json: null,
    });
    // A conflict seen by a save while leaving the page is merged by the next ordinary save.
    this.schedule();
    return false;
  }

  /** What to write for a workspace: this window's tabs merged into the stored ones, within the size limit. */
  private payload(workspaceId: string, local: SavedWorkspaceTabs): SavedWorkspaceTabs {
    const record = this.records.get(workspaceId);
    const merged = mergeTabs(local, record?.stored ?? null, record?.shown ?? new Set());
    return merged === local ? local : fitToBytes(merged, this.maxBytes());
  }

  /** Whether `state` differs from what was written and from what is on its way (an empty workspace never saved: no). */
  private needsSave(workspaceId: string, state: SavedWorkspaceTabs, json: string): boolean {
    const record = this.records.get(workspaceId);
    if (!record && state.tabs.length === 0) return false;
    return record?.json !== json && this.pending.get(workspaceId) !== json;
  }

  private edition(): "web" | "desktop" {
    return capabilities().edition === "web" ? "web" : "desktop";
  }

  private maxBytes(): number {
    return WORKSPACE_MAX_BYTES[this.edition()];
  }

  /** Each workspace's tabs in this window now, with the tab key of each saved tab. */
  private plans(): Map<string, Plan> {
    const edition = this.edition();
    const plans = new Map<string, Plan>();
    for (const workspaceId of this.workspaceIds()) {
      const { keys, active } = this.tabs.layout(workspaceId);
      const entries = keys
        .map((key) => ({ key, tab: this.navigation.savedTab(key) }))
        .filter((entry): entry is { key: string; tab: SavedTab } => entry.tab !== null);
      const activeIndex = entries.findIndex((entry) => entry.key === active);
      const bounded = serializeTabs(
        entries.map((entry) => entry.tab),
        activeIndex >= 0 ? activeIndex : null,
        WORKSPACE_TEXT_BUDGET[edition],
      );
      plans.set(workspaceId, { local: fitToBytes(bounded, WORKSPACE_MAX_BYTES[edition]), keys: entries.map((entry) => entry.key) });
    }
    return plans;
  }

  private reportFailure(error: unknown): void {
    if (this.failureReported) return;
    this.failureReported = true;
    showError(error, "Save the open tabs (they will not come back after a restart)");
  }
}
