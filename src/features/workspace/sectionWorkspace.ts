/**
 * An engine workspace: one tab for an object (a collection, an index, a key space) with a strip of sections under the
 * tab bar ("Documents | Aggregations | Schema | Indexes"). An engine registers it as the `workspace` role of a kind
 * (`core/views.ts`); a default open then shows it instead of the grid, which stays in the node menu ("Open as grid").
 *
 * Sections are loaded on first show (each can be a chunk of its own) and stay alive while the tab is open, so switching
 * back keeps their state. The tab forwards `canClose`, `closeWarning` and `hasUnsavedChanges` to every loaded section,
 * saves the shown section plus each section's `saveState()` with the open tabs (bounded, `workspaceState.ts`), and gives
 * the assistant every section's `assistantContext()`.
 */
import type { CapabilityName, ObjectRef } from "../../core/contract";
import { h, icon, type IconName } from "../../core/dom";
import { showError } from "../../core/errorPanel";
import type { TabContent } from "../../core/tabs";
import type { ViewContext } from "../../core/views";
import { WorkspaceStore } from "./state";
import { boundViewState, parseViewState, workspaceContextText, type WorkspaceViewState } from "./workspaceState";

/**
 * What a section gives the workspace: a tab content (its element fills the area under the strip) and more. A section
 * with `canClose` also gives `closeWarning`: the workspace tab has one, so "Close all" asks through it, not `canClose`.
 */
export interface SectionContent extends TabContent {
  /** Plain JSON to keep across restarts (a filter, a sort, the pipeline); given back as `SectionContext.savedState`. */
  saveState?(): unknown;
  /** What the assistant should know about this section now (the filter, the active stage, sampled fields, the last error). */
  assistantContext?(): string | null;
}

export interface SectionContext {
  /** The workspace tab's own view context (connection, engine, object, navigator). */
  view: ViewContext;
  /** Values the sections of this tab share. */
  store: WorkspaceStore;
  /** What this section saved last session (`saveState`), undefined when there is nothing. */
  savedState: unknown;
  /** The section's saved state changed: the open tabs are saved again (debounced). */
  stateChanged(): void;
  /** Shows another section of the same tab (loading it first). */
  show(sectionId: string): Promise<void>;
}

export type SectionFactory = (ctx: SectionContext) => SectionContent;

export interface WorkspaceSection {
  /** Stable id: it is saved with the tab ("documents", "aggregations"). */
  id: string;
  title: string;
  icon: IconName;
  /** The section is shown only when the connection (or, before it reports, the engine) has this capability. */
  requires?: CapabilityName;
  load: () => Promise<SectionFactory>;
}

/** The tab content `createSectionWorkspace` returns; `app/` duck-types it (`isWorkspaceTab`). */
export interface WorkspaceTab extends TabContent {
  readonly connectionId: string;
  readonly object: ObjectRef | null;
  /** The id of the section shown. */
  activeSection(): string;
  show(sectionId: string): Promise<void>;
  /** What is saved with the open tabs. */
  viewState(): WorkspaceViewState;
  /** The tab's context for the assistant (bounded). */
  workspaceContext(): string;
  /** The saved view changed (another section shown, a section's state): set by navigation to save the open tabs. */
  onViewChanged: (() => void) | null;
}

export function isWorkspaceTab(content: TabContent | null | undefined): content is WorkspaceTab {
  return !!content && typeof (content as WorkspaceTab).viewState === "function" && typeof (content as WorkspaceTab).workspaceContext === "function";
}

interface Loaded {
  content: SectionContent | null;
  /** The section's slot under the strip (its content, or the loading / failure message). */
  slot: HTMLElement;
  pending: Promise<void> | null;
}

/**
 * The workspace tab for `ctx.object` with `sections` (those whose `requires` the connection lacks are left out).
 * `ctx.params.view` (a saved `WorkspaceViewState`) picks the section and gives sections their saved state;
 * `ctx.params.section` (a section id) opens on that section.
 */
export function createSectionWorkspace(ctx: ViewContext, sections: WorkspaceSection[]): WorkspaceTab {
  return new SectionWorkspace(ctx, sections);
}

class SectionWorkspace implements WorkspaceTab {
  readonly element: HTMLElement;
  readonly connectionId: string;
  readonly object: ObjectRef | null;
  onViewChanged: (() => void) | null = null;
  private readonly sections: WorkspaceSection[];
  private readonly store = new WorkspaceStore();
  private readonly strip = h("div", { class: "section-strip", role: "tablist", "aria-label": "Sections" });
  private readonly body = h("div", { class: "section-body" });
  private readonly buttons = new Map<string, HTMLButtonElement>();
  private readonly loaded = new Map<string, Loaded>();
  /** Saved state of sections not loaded yet (kept so saving again does not lose it). */
  private readonly restored: Record<string, unknown>;
  private active: string;
  private disposed = false;

  constructor(
    private readonly ctx: ViewContext,
    all: WorkspaceSection[],
  ) {
    this.connectionId = ctx.profile.id;
    this.object = ctx.object;
    const capabilities = ctx.info.capabilities;
    this.sections = all.filter((section) => !section.requires || capabilities.includes(section.requires));
    const saved = parseViewState(ctx.params?.view);
    this.restored = { ...(saved?.state ?? {}) };
    const wanted = typeof ctx.params?.section === "string" ? ctx.params.section : saved?.section;
    this.active = this.sections.find((section) => section.id === wanted)?.id ?? this.sections[0]?.id ?? "";
    for (const section of this.sections) {
      const button = h(
        "button",
        { class: "section-tab", type: "button", role: "tab", tabindex: "-1", "aria-selected": "false", "data-section": section.id },
        icon(section.icon, "section-tab-icon"),
        h("span", {}, section.title),
      );
      button.addEventListener("click", () => void this.show(section.id));
      this.buttons.set(section.id, button);
      this.strip.append(button);
    }
    this.strip.addEventListener("keydown", (event) => this.onStripKey(event));
    this.strip.hidden = this.sections.length < 2;
    this.element = h("div", { class: "view section-workspace" }, this.strip, this.body);
    if (this.sections.length === 0) this.body.append(h("div", { class: "section-message" }, "Nothing to show for this connection."));
    else void this.show(this.active, false);
  }

  activeSection(): string {
    return this.active;
  }

  async show(sectionId: string, notify = true): Promise<void> {
    const section = this.sections.find((item) => item.id === sectionId);
    if (!section || this.disposed) return;
    const changed = this.active !== sectionId;
    this.active = sectionId;
    for (const [id, button] of this.buttons) {
      const selected = id === sectionId;
      button.classList.toggle("active", selected);
      button.setAttribute("aria-selected", String(selected));
      button.tabIndex = selected ? 0 : -1;
    }
    let entry = this.loaded.get(sectionId);
    if (entry && !entry.content && !entry.pending) {
      // Loading it failed before: try again.
      entry.slot.remove();
      this.loaded.delete(sectionId);
      entry = undefined;
    }
    entry ??= this.load(section);
    for (const [id, other] of this.loaded) other.slot.hidden = id !== sectionId;
    if (changed && notify) this.onViewChanged?.();
    if (entry.pending) await entry.pending;
    if (this.active === sectionId && !this.element.hidden) entry.content?.onActivate?.();
  }

  onActivate(): void {
    this.loaded.get(this.active)?.content?.onActivate?.();
  }

  dispose(): void {
    this.disposed = true;
    for (const entry of this.loaded.values()) entry.content?.dispose?.();
    this.loaded.clear();
    this.store.clear();
  }

  async canClose(): Promise<boolean> {
    // Asked one by one: the first section that says no keeps the tab open.
    for (const entry of this.loaded.values()) if (entry.content?.canClose && !(await entry.content.canClose())) return false;
    return true;
  }

  closeWarning(): string | null {
    const warnings = [...this.loaded.values()].map((entry) => entry.content?.closeWarning?.() ?? null).filter((item): item is string => !!item);
    return warnings.length > 0 ? warnings.join("; ") : null;
  }

  hasUnsavedChanges(): boolean {
    return [...this.loaded.values()].some((entry) => entry.content?.hasUnsavedChanges?.() === true);
  }

  viewState(): WorkspaceViewState {
    const state: Record<string, unknown> = { ...this.restored };
    for (const [id, entry] of this.loaded) {
      if (!entry.content?.saveState) continue;
      try {
        const value = entry.content.saveState();
        if (value === undefined) delete state[id];
        else state[id] = value;
      } catch {
        // A section that cannot say its state keeps what it had.
      }
    }
    return boundViewState({ section: this.active, state });
  }

  workspaceContext(): string {
    const object = this.object;
    const name = object ? [object.database, object.schema, object.name].filter(Boolean).join(".") : this.ctx.profile.name;
    const shown = this.sections.find((section) => section.id === this.active);
    const header = `${this.ctx.engine.label} workspace of ${object?.kind ?? "connection"} ${name}; section shown: ${shown?.title ?? "none"}.`;
    const parts = this.sections
      .filter((section) => this.loaded.get(section.id)?.content)
      .map((section) => {
        let text: string | null = null;
        try {
          text = this.loaded.get(section.id)?.content?.assistantContext?.() ?? null;
        } catch {
          text = null;
        }
        return { title: section.title, text, active: section.id === this.active };
      });
    return workspaceContextText(header, parts);
  }

  private load(section: WorkspaceSection): Loaded {
    const slot = h("div", { class: "section-slot" }, h("div", { class: "section-message" }, `Loading ${section.title}…`));
    this.body.append(slot);
    const entry: Loaded = { content: null, slot, pending: null };
    this.loaded.set(section.id, entry);
    const run = async (): Promise<void> => {
      try {
        const factory = await section.load();
        if (this.disposed) return;
        const sectionCtx: SectionContext = {
          view: this.ctx,
          store: this.store,
          savedState: this.restored[section.id],
          stateChanged: () => this.onViewChanged?.(),
          show: (id) => this.show(id),
        };
        const content = factory(sectionCtx);
        delete this.restored[section.id];
        entry.content = content;
        slot.replaceChildren(content.element);
      } catch (error) {
        // The message stays until the section is shown again, which tries again (`show`).
        slot.replaceChildren(h("div", { class: "section-message" }, `${section.title} could not be loaded.`));
        showError(error, `Open ${section.title}`);
      }
    };
    // Cleared asynchronously even when `load` throws at once, so `pending` is never left set.
    entry.pending = run().finally(() => {
      entry.pending = null;
    });
    return entry;
  }

  /** ArrowLeft / ArrowRight / Home / End move between sections (the strip is one tab stop). */
  private onStripKey(event: KeyboardEvent): void {
    const ids = this.sections.map((section) => section.id);
    const index = ids.indexOf(this.active);
    let next = -1;
    if (event.key === "ArrowRight") next = (index + 1) % ids.length;
    else if (event.key === "ArrowLeft") next = (index - 1 + ids.length) % ids.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = ids.length - 1;
    if (next < 0) return;
    event.preventDefault();
    void this.show(ids[next]);
    this.buttons.get(ids[next])?.focus();
  }
}
