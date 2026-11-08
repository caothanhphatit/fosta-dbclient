import { showContextMenu, type MenuItem } from "./contextMenu";
import { h, icon, type IconName } from "./dom";
import { horizontalWheelDelta } from "./wheelScroll";

export interface TabContent {
  readonly element: HTMLElement;
  onActivate?(): void;
  dispose?(): void;
  canClose?(): boolean | Promise<boolean>;
}

export interface TabSpec {
  key: string;
  title: string;
  icon: IconName;
  subtitle?: string;
  /** Tabs are grouped by scope (one per workspace); only the current scope is shown. */
  scope?: string;
  create: () => TabContent;
}

interface OpenTab {
  key: string;
  scope: string;
  header: HTMLElement;
  content: TabContent;
}

const DEFAULT_SCOPE = "";

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

  open(spec: TabSpec): void {
    if (!this.tabs.some((tab) => tab.key === spec.key)) {
      const content = spec.create();
      const header = this.createHeader(spec);
      const scope = spec.scope ?? this.scope;
      header.hidden = scope !== this.scope;
      this.tabs.push({ key: spec.key, scope, header, content });
      this.strip.append(header);
      this.body.append(content.element);
    }
    this.activate(spec.key);
  }

  /** Every open tab's content, in all workspaces. */
  contents(): TabContent[] {
    return this.tabs.map((tab) => tab.content);
  }

  contentOf(key: string): TabContent | undefined {
    return this.tabs.find((tab) => tab.key === key)?.content;
  }

  async close(key: string): Promise<void> {
    const index = this.tabs.findIndex((tab) => tab.key === key);
    const tab = this.tabs[index];
    if (!tab || (tab.content.canClose && !(await tab.content.canClose()))) return;
    const wasActive = this.activeKey === key;
    const siblings = this.tabs.filter((item) => item.scope === tab.scope);
    const position = siblings.indexOf(tab);
    const neighbour = siblings[position + 1] ?? siblings[position - 1];
    tab.content.dispose?.();
    tab.header.remove();
    tab.content.element.remove();
    this.tabs.splice(index, 1);
    if (wasActive) this.activate(neighbour && neighbour.scope === this.scope ? neighbour.key : null);
    else this.refreshPlaceholder();
  }

  closeWhere(matches: (key: string) => boolean): void {
    for (const tab of [...this.tabs]) {
      if (matches(tab.key)) void this.close(tab.key);
    }
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
  }

  private refreshPlaceholder(): void {
    this.placeholder.hidden = this.visibleTabs().length > 0;
    this.refreshOverflow();
  }

  private createHeader(spec: TabSpec): HTMLElement {
    const closeButton = h("span", { class: "tab-close", title: "Close" }, icon("x"));
    closeButton.addEventListener("mousedown", (event) => event.stopPropagation());
    closeButton.addEventListener("click", () => void this.close(spec.key));
    const header = h(
      "div",
      { class: "tab", role: "tab", title: spec.subtitle ?? spec.title },
      icon(spec.icon, `tab-icon ${spec.icon}`),
      h("span", { class: "tab-title" }, spec.title),
      closeButton,
    );
    header.addEventListener("mousedown", (event) => {
      if (event.button === 1) {
        event.preventDefault();
        void this.close(spec.key);
      } else if (event.button === 0) {
        this.activate(spec.key);
      }
    });
    header.addEventListener("contextmenu", (event) => showContextMenu(event, this.tabMenu(spec.key)));
    return header;
  }

  private tabMenu(key: string): MenuItem[] {
    const scope = this.tabs.find((tab) => tab.key === key)?.scope ?? this.scope;
    const siblings = this.tabs.filter((tab) => tab.scope === scope);
    return [
      { label: "Close", icon: "x", run: () => void this.close(key) },
      { label: "Close others", disabled: siblings.length < 2, run: () => this.closeWhere((item) => item !== key && siblings.some((tab) => tab.key === item)) },
      { label: "Close all", run: () => this.closeWhere((item) => siblings.some((tab) => tab.key === item)) },
    ];
  }
}
