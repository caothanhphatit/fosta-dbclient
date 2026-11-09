import { showContextMenu, type MenuItem } from "./contextMenu";
import { confirmDialog } from "./dialogs";
import { h, icon, type IconName } from "./dom";
import { disambiguateTitles, tabTooltip } from "./tabTitles";
import { horizontalWheelDelta } from "./wheelScroll";

export interface TabContent {
  readonly element: HTMLElement;
  onActivate?(): void;
  dispose?(): void;
  canClose?(): boolean | Promise<boolean>;
  /** What closing loses ("query text not run"), null for nothing: "Close all" asks once for every tab (no `canClose`). */
  closeWarning?(): string | null;
  /** Edits not saved yet; quitting (or restarting to update) asks first. */
  hasUnsavedChanges?(): boolean;
}

export interface TabSpec {
  key: string;
  title: string;
  icon: IconName;
  subtitle?: string;
  /** Tabs are grouped by scope (one per workspace); only the current scope is shown. */
  scope?: string;
  /** The connection's name: in the tooltip, and in the title when another tab has the same title. */
  connection?: string;
  /** The database, added to the title when it is what tells two tabs apart. */
  database?: string | null;
  create: () => TabContent;
}

interface OpenTab {
  key: string;
  scope: string;
  header: HTMLElement;
  titleLabel: HTMLElement;
  spec: TabSpec;
  content: TabContent;
}

const DEFAULT_SCOPE = "";
/** The most tabs one "Close all" question names. */
const MAX_LISTED = 12;

export class TabManager {
  readonly element: HTMLElement;
  private readonly strip = h("div", { class: "tab-strip", role: "tablist" });
  private readonly body = h("div", { class: "tab-body" });
  private readonly placeholder: HTMLElement;
  private readonly lastActive = new Map<string, string>();
  private tabs: OpenTab[] = [];
  private scope = DEFAULT_SCOPE;
  private activeKey: string | null = null;
  /** Called after the active tab changes (also when the last tab closes). */
  onActiveChange: ((content: TabContent | null) => void) | null = null;
  /** Called after a tab opens, closes or is replaced, or another one becomes active (the open tabs are saved). */
  onTabsChanged: (() => void) | null = null;

  private readonly listButton: HTMLButtonElement;
  private readonly stripObserver: ResizeObserver;

  constructor(placeholder: HTMLElement) {
    this.placeholder = placeholder;
    this.body.append(placeholder);
    // The strip has no visible scrollbar: the wheel scrolls it sideways, the active tab is kept in view,
    // and the list button (shown only when tabs overflow) reaches every tab.
    this.strip.addEventListener(
      "wheel",
      (event) => {
        const delta = horizontalWheelDelta(event);
        if (delta === 0 || this.strip.scrollWidth <= this.strip.clientWidth) return;
        event.preventDefault();
        this.strip.scrollLeft += delta;
      },
      { passive: false },
    );
    this.listButton = h("button", { class: "icon-button tab-list-button", type: "button", title: "All tabs", "aria-label": "All tabs" }, icon("chevronDown"));
    this.listButton.hidden = true;
    this.listButton.addEventListener("click", (event) => showContextMenu(event, this.listMenu()));
    this.stripObserver = new ResizeObserver(() => this.refreshOverflow());
    this.stripObserver.observe(this.strip);
    this.element = h("div", { class: "tabs" }, h("div", { class: "tab-bar" }, this.strip, this.listButton), this.body);
  }

  private refreshOverflow(): void {
    this.listButton.hidden = this.strip.scrollWidth <= this.strip.clientWidth + 1;
  }

  private listMenu(): MenuItem[] {
    return this.visibleTabs().map((tab) => ({
      label: tab.header.querySelector(".tab-title")?.textContent ?? tab.key,
      icon: tab.key === this.activeKey ? "check" : undefined,
      run: () => this.activate(tab.key),
    }));
  }

  activeContent(): TabContent | null {
    return this.tabs.find((tab) => tab.key === this.activeKey)?.content ?? null;
  }

  /** Shows only the tabs of `scope`, restoring the tab that was last active there. */
  setScope(scope: string): void {
    if (scope === this.scope) return;
    if (this.activeKey) this.lastActive.set(this.scope, this.activeKey);
    this.scope = scope;
    for (const tab of this.tabs) tab.header.hidden = tab.scope !== scope;
    const remembered = this.lastActive.get(scope);
    const next = this.visibleTabs().find((tab) => tab.key === remembered) ?? this.visibleTabs()[0];
    this.activate(next?.key ?? null);
  }

  /** Opens (or selects) a tab; `select: false` adds it behind the others (restoring the tabs of a workspace). */
  open(spec: TabSpec, select = true): void {
    if (!this.tabs.some((tab) => tab.key === spec.key)) {
      const content = spec.create();
      const { header, titleLabel } = this.createHeader(spec);
      const scope = spec.scope ?? this.scope;
      header.hidden = scope !== this.scope;
      content.element.hidden = true;
      this.tabs.push({ key: spec.key, scope, header, titleLabel, spec, content });
      this.strip.append(header);
      this.body.append(content.element);
      this.refreshTitles();
    }
    if (select) this.activate(spec.key);
    else this.refreshPlaceholder();
    this.onTabsChanged?.();
  }

  /**
   * Puts a new tab in the place of `key` (same position; the scope of `spec`, else the old tab's; selected when that one
   * was): a restored tab turning into its real view once it is shown. The old content is disposed. False when `key` is
   * no longer open (it closed meanwhile): nothing is created.
   */
  replace(key: string, spec: TabSpec): boolean {
    const tab = this.tabs.find((item) => item.key === key);
    if (!tab) return false;
    const scope = spec.scope ?? tab.scope;
    const content = spec.create();
    const { header, titleLabel } = this.createHeader(spec);
    header.hidden = scope !== this.scope;
    content.element.hidden = true;
    tab.header.replaceWith(header);
    tab.content.element.replaceWith(content.element);
    tab.content.dispose?.();
    const wasActive = this.activeKey === key;
    for (const [remembered, active] of this.lastActive) if (active === key) this.lastActive.set(remembered, spec.key);
    Object.assign(tab, { key: spec.key, scope, header, titleLabel, spec, content });
    this.refreshTitles();
    if (wasActive) this.activate(scope === this.scope ? spec.key : null);
    this.onTabsChanged?.();
    return true;
  }

  /** The tab keys of `scope` in strip order, and the one active there (or last active, for another workspace). */
  layout(scope: string): { keys: string[]; active: string | null } {
    const keys = this.tabs.filter((tab) => tab.scope === scope).map((tab) => tab.key);
    const active = scope === this.scope ? this.activeKey : (this.lastActive.get(scope) ?? null);
    return { keys, active: active && keys.includes(active) ? active : null };
  }

  /** The tab `setScope(scope)` selects (restoring a workspace that is not shown). */
  rememberActive(scope: string, key: string): void {
    if (scope === this.scope) this.activate(key);
    else this.lastActive.set(scope, key);
  }

  /** The title shown for `key` (with its connection when another tab has the same title). */
  titleOf(key: string): string {
    return this.tabs.find((tab) => tab.key === key)?.titleLabel.textContent ?? "";
  }

  /** Changes a tab's title and description (a query tab switched to another database). */
  describe(key: string, change: Pick<TabSpec, "subtitle" | "database">): void {
    const tab = this.tabs.find((item) => item.key === key);
    if (!tab) return;
    tab.spec = { ...tab.spec, ...change };
    this.refreshTitles();
  }

  /** Every open tab's content, in all workspaces. */
  contents(): TabContent[] {
    return this.tabs.map((tab) => tab.content);
  }

  keyOf(content: TabContent): string | null {
    return this.tabs.find((tab) => tab.content === content)?.key ?? null;
  }

  contentOf(key: string): TabContent | undefined {
    return this.tabs.find((tab) => tab.key === key)?.content;
  }

  /** Closes the tab `key` once its content agrees (`canClose`); resolves false when it stays open. */
  async close(key: string): Promise<boolean> {
    const tab = this.tabs.find((item) => item.key === key);
    if (!tab) return true;
    if (tab.content.canClose && !(await tab.content.canClose())) return false;
    if (this.tabs.includes(tab)) this.remove(tab);
    return true;
  }

  /** Takes a tab out without asking. */
  private remove(tab: OpenTab): void {
    const index = this.tabs.indexOf(tab);
    if (index < 0) return;
    const wasActive = this.activeKey === tab.key;
    const siblings = this.tabs.filter((item) => item.scope === tab.scope);
    const position = siblings.indexOf(tab);
    const neighbour = siblings[position + 1] ?? siblings[position - 1];
    tab.content.dispose?.();
    tab.header.remove();
    tab.content.element.remove();
    this.tabs.splice(index, 1);
    this.refreshTitles();
    if (wasActive) this.activate(neighbour && neighbour.scope === this.scope ? neighbour.key : null);
    else this.refreshPlaceholder();
    this.onTabsChanged?.();
  }

  /** Titles that tell same-named tabs of a workspace apart (by connection, then database), and the tooltips. */
  private refreshTitles(): void {
    const scopes = new Set(this.tabs.map((tab) => tab.scope));
    for (const scope of scopes) {
      const tabs = this.tabs.filter((tab) => tab.scope === scope);
      const titles = disambiguateTitles(tabs.map((tab) => tab.spec));
      tabs.forEach((tab, index) => {
        tab.titleLabel.textContent = titles[index];
        tab.header.title = tabTooltip(tab.spec);
      });
    }
  }

  /**
   * Closes every tab whose key `matches`. One question lists the tabs that would lose something (`closeWarning`, or
   * `canClose` for a tab without one); declined, nothing closes. Resolves true when every matching tab closed.
   */
  async closeWhere(matches: (key: string) => boolean): Promise<boolean> {
    const targets = this.tabs.filter((tab) => matches(tab.key));
    const warned = targets.flatMap((tab) => {
      const warning = tab.content.closeWarning?.();
      return warning ? [`${tab.titleLabel.textContent ?? tab.key}: ${warning}`] : [];
    });
    if (warned.length > 0) {
      const lines = warned.slice(0, MAX_LISTED).map((line) => `• ${line}`);
      if (warned.length > MAX_LISTED) lines.push(`• …and ${warned.length - MAX_LISTED} more`);
      const question = `${warned.length} tab(s) would lose something when closed:\n${lines.join("\n")}\nClose them anyway?`;
      if (!(await confirmDialog(question, "Close tabs"))) return false;
    }
    let all = true;
    for (const tab of targets) {
      if (!this.tabs.includes(tab)) continue;
      // Tabs that can say what they lose were asked about above; others ask on their own.
      const asked = typeof tab.content.closeWarning === "function";
      if (asked) this.remove(tab);
      else all = (await this.close(tab.key)) && all;
    }
    return all;
  }

  private visibleTabs(): OpenTab[] {
    return this.tabs.filter((tab) => tab.scope === this.scope);
  }

  private activate(key: string | null): void {
    this.activeKey = key;
    for (const tab of this.tabs) {
      const active = tab.key === key;
      tab.header.classList.toggle("active", active);
      tab.header.setAttribute("aria-selected", String(active));
      tab.content.element.hidden = !active;
      if (active) {
        tab.header.scrollIntoView({ block: "nearest", inline: "nearest" });
        tab.content.onActivate?.();
      }
    }
    this.refreshPlaceholder();
    this.onActiveChange?.(this.activeContent());
    this.onTabsChanged?.();
  }

  private refreshPlaceholder(): void {
    this.placeholder.hidden = this.visibleTabs().length > 0;
    this.refreshOverflow();
  }

  private createHeader(spec: TabSpec): { header: HTMLElement; titleLabel: HTMLElement } {
    const closeButton = h("span", { class: "tab-close", title: "Close" }, icon("x"));
    closeButton.addEventListener("mousedown", (event) => event.stopPropagation());
    closeButton.addEventListener("click", () => void this.close(spec.key));
    const titleLabel = h("span", { class: "tab-title" }, spec.title);
    const header = h("div", { class: "tab", role: "tab", title: tabTooltip(spec) }, icon(spec.icon, `tab-icon ${spec.icon}`), titleLabel, closeButton);
    header.addEventListener("mousedown", (event) => {
      if (event.button === 1) {
        event.preventDefault();
        void this.close(spec.key);
      } else if (event.button === 0) {
        this.activate(spec.key);
      }
    });
    header.addEventListener("contextmenu", (event) => showContextMenu(event, this.tabMenu(spec.key)));
    return { header, titleLabel };
  }

  private tabMenu(key: string): MenuItem[] {
    const scope = this.tabs.find((tab) => tab.key === key)?.scope ?? this.scope;
    const siblings = this.tabs.filter((tab) => tab.scope === scope);
    return [
      { label: "Close", icon: "x", run: () => void this.close(key) },
      {
        label: "Close others",
        disabled: siblings.length < 2,
        run: () => void this.closeWhere((item) => item !== key && siblings.some((tab) => tab.key === item)),
      },
      { label: "Close all", run: () => void this.closeWhere((item) => siblings.some((tab) => tab.key === item)) },
    ];
  }
}
