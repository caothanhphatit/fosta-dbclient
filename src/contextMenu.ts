import { h, icon, type IconName } from "./dom";

export type MenuItem =
  | { label: string; icon?: IconName; danger?: boolean; disabled?: boolean; run: () => void }
  | "separator";

let activeMenu: HTMLElement | null = null;

export function closeContextMenu(): void {
  activeMenu?.remove();
  activeMenu = null;
}

export function showContextMenu(event: MouseEvent, items: MenuItem[]): void {
  event.preventDefault();
  event.stopPropagation();
  closeContextMenu();
  const menu = h("div", { class: "context-menu" });
  for (const item of items) menu.append(item === "separator" ? h("div", { class: "menu-separator" }) : menuEntry(item));
  document.body.append(menu);
  placeWithinViewport(menu, event.clientX, event.clientY);
  activeMenu = menu;
}

function menuEntry(item: Exclude<MenuItem, "separator">): HTMLElement {
  const classes = ["menu-item", item.danger ? "danger" : "", item.disabled ? "disabled" : ""].filter(Boolean).join(" ");
  const entry = h("div", { class: classes }, item.icon ? icon(item.icon) : h("span", { class: "icon" }), h("span", {}, item.label));
  entry.addEventListener("click", () => {
    if (item.disabled) return;
    closeContextMenu();
    item.run();
  });
  return entry;
}

function placeWithinViewport(menu: HTMLElement, x: number, y: number): void {
  const bounds = menu.getBoundingClientRect();
  menu.style.left = `${Math.max(4, Math.min(x, window.innerWidth - bounds.width - 4))}px`;
  menu.style.top = `${Math.max(4, Math.min(y, window.innerHeight - bounds.height - 4))}px`;
}

document.addEventListener("mousedown", (event) => {
  if (activeMenu && !activeMenu.contains(event.target as Node)) closeContextMenu();
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") closeContextMenu();
});
window.addEventListener("blur", closeContextMenu);
