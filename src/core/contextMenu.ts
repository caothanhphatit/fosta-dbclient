import { h, icon, type IconName } from "./dom";
import { nextFocusable, placeInViewport } from "./placement";

/** A small button at the end of a menu entry. */
export interface MenuAction {
  icon: IconName;
  title: string;
  run: () => void;
}

export type MenuItem =
  | {
      label: string;
      icon?: IconName;
      danger?: boolean;
      disabled?: boolean;
      run: () => void;
      /** A status dot instead of the icon (`state-<dot>` styles it, e.g. "running"). */
      dot?: string;
      /** A second, dimmer line under the label. */
      detail?: string;
      actions?: MenuAction[];
    }
  | "separator";

let activeMenu: HTMLElement | null = null;
/** The entry the arrow keys moved to (-1: none yet). */
let focusedIndex = -1;

export function closeContextMenu(): void {
  activeMenu?.remove();
  activeMenu = null;
  focusedIndex = -1;
}

export function showContextMenu(event: MouseEvent, items: MenuItem[]): void {
  event.preventDefault();
  event.stopPropagation();
  openMenu(items, event.clientX, event.clientY);
}

/** Opens `items` as a dropdown under `anchor` (a toolbar button). */
export function showMenuBelow(anchor: HTMLElement, items: MenuItem[]): void {
  const bounds = anchor.getBoundingClientRect();
  openMenu(items, bounds.left, bounds.bottom + 2);
}

function openMenu(items: MenuItem[], x: number, y: number): void {
  closeContextMenu();
  const menu = h("div", { class: "context-menu" });
  for (const item of items) menu.append(item === "separator" ? h("div", { class: "menu-separator" }) : menuEntry(item));
  document.body.append(menu);
  placeWithinViewport(menu, x, y);
  activeMenu = menu;
}

function menuEntry(item: Exclude<MenuItem, "separator">): HTMLElement {
  const classes = ["menu-item", item.danger ? "danger" : "", item.disabled ? "disabled" : "", item.dot ? `with-dot state-${item.dot}` : ""]
    .filter(Boolean)
    .join(" ");
  const lead = item.dot ? h("span", { class: "status-dot" }) : item.icon ? icon(item.icon) : h("span", { class: "icon" });
  const label = item.detail
    ? h("span", { class: "menu-text" }, h("span", {}, item.label), h("span", { class: "menu-detail" }, item.detail))
    : h("span", {}, item.label);
  const entry = h("div", { class: classes }, lead, label);
  if (item.actions?.length) {
    const actions = h("span", { class: "menu-actions" });
    for (const action of item.actions) {
      const button = h("button", { class: "icon-button", type: "button", title: action.title, "aria-label": action.title }, icon(action.icon));
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        closeContextMenu();
        action.run();
      });
      actions.append(button);
    }
    entry.append(actions);
  }
  entry.addEventListener("click", () => {
    if (item.disabled) return;
    closeContextMenu();
    item.run();
  });
  return entry;
}

/** Measured untransformed (the opening animation scales the menu), capped to the window height (it scrolls). */
function placeWithinViewport(menu: HTMLElement, x: number, y: number): void {
  const placed = placeInViewport(x, y, menu.offsetWidth, menu.offsetHeight, window.innerWidth, window.innerHeight);
  menu.style.maxHeight = `${placed.maxHeight}px`;
  menu.style.left = `${placed.left}px`;
  menu.style.top = `${placed.top}px`;
}

function menuEntries(menu: HTMLElement): HTMLElement[] {
  return Array.from(menu.querySelectorAll<HTMLElement>(":scope > .menu-item"));
}

/** Arrow keys, Home and End move between entries (kept in view when the menu scrolls); Enter runs the focused one. */
function moveFocus(menu: HTMLElement, step: 1 | -1 | "first" | "last"): void {
  const entries = menuEntries(menu);
  const next = nextFocusable(focusedIndex, step, entries.length, (index) => !entries[index].classList.contains("disabled"));
  entries.forEach((entry, index) => entry.classList.toggle("focused", index === next));
  focusedIndex = next;
  entries[next]?.scrollIntoView({ block: "nearest" });
}

const MOVES: Partial<Record<string, 1 | -1 | "first" | "last">> = { ArrowDown: 1, ArrowUp: -1, Home: "first", End: "last" };

document.addEventListener("mousedown", (event) => {
  if (activeMenu && !activeMenu.contains(event.target as Node)) closeContextMenu();
});
document.addEventListener(
  "keydown",
  (event) => {
    if (!activeMenu) return;
    const move = MOVES[event.key];
    if (event.key === "Escape") {
      closeContextMenu();
    } else if (move !== undefined) {
      moveFocus(activeMenu, move);
    } else if (event.key === "Enter" && focusedIndex >= 0) {
      menuEntries(activeMenu)[focusedIndex]?.click();
    } else {
      return;
    }
    // The grid, the tree and the editor underneath must not act on the same key.
    event.preventDefault();
    event.stopPropagation();
  },
  true,
);
window.addEventListener("blur", closeContextMenu);
