export type Child = Node | string | number | null | undefined | false;

const PROPERTIES = new Set(["value", "checked", "selected", "tabIndex", "hidden"]);

const ICONS = {
  server: '<rect x="2" y="2.5" width="12" height="4.5" rx="1"/><rect x="2" y="9" width="12" height="4.5" rx="1"/><path d="M4.5 4.75h.01M4.5 11.25h.01"/>',
  database: '<ellipse cx="8" cy="3.8" rx="5" ry="1.8"/><path d="M3 3.8v8.4c0 1 2.2 1.8 5 1.8s5-.8 5-1.8V3.8"/><path d="M3 8c0 1 2.2 1.8 5 1.8s5-.8 5-1.8"/>',
  table: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M2 6.5h12M2 9.8h12M6.5 6.5V13"/>',
  view: '<path d="M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="2"/>',
  query: '<path d="M4 1.5h5.5L12.5 4.5V14.5H4z"/><path d="M9.5 1.5v3h3M6 8h4.5M6 11h3"/>',
  play: '<path d="M5 3l8 5-8 5z" fill="currentColor"/>',
  stop: '<rect x="4" y="4" width="8" height="8" rx="1" fill="currentColor"/>',
  refresh: '<path d="M13 8a5 5 0 1 1-1.46-3.54"/><path d="M13 2.5V5h-2.5"/>',
  plus: '<path d="M8 3v10M3 8h10"/>',
  minus: '<path d="M3 8h10"/>',
  check: '<path d="M3 8.5l3 3 7-7"/>',
  x: '<path d="M4 4l8 8M12 4l-8 8"/>',
  filter: '<path d="M2 3h12l-4.5 5.5V13l-3 1.5V8.5z"/>',
  key: '<circle cx="5" cy="8" r="2.8"/><path d="M7.8 8h6.2M12 8v2.2M10 8v1.6"/>',
  task: '<path d="M1.5 8.5h3l2-5 3 9 2-4h3"/>',
  edit: '<path d="M3 13l.8-3.2L10.5 3l2.5 2.5-6.8 6.7z"/>',
  trash: '<path d="M2.5 4h11M6 4V2.5h4V4M4 4l.8 10h6.4L12 4"/>',
  chevron: '<path d="M6 4l4 4-4 4"/>',
  chevronDown: '<path d="M4 6l4 4 4-4"/>',
  prev: '<path d="M10 4L6 8l4 4"/>',
  first: '<path d="M11 4L7 8l4 4M4.5 4v8"/>',
  last: '<path d="M5 4l4 4-4 4M11.5 4v8"/>',
  design: '<path d="M2 3.5h12M2 6.5h12M2 9.5h12M2 12.5h7"/>',
  diagram: '<rect x="1.5" y="2" width="5" height="4.5" rx="0.8"/><rect x="9.5" y="9.5" width="5" height="4.5" rx="0.8"/><path d="M6.5 4.25h2.5v7.5h0.5"/>',
  fit: '<path d="M2 5.5V2h3.5M10.5 2H14v3.5M14 10.5V14h-3.5M5.5 14H2v-3.5"/>',
  download: '<path d="M8 2v8.5M4.5 7L8 10.5 11.5 7M2.5 13.5h11"/>',
  upload: '<path d="M8 11V2.5M4.5 6L8 2.5 11.5 6M2.5 13.5h11"/>',
  plug: '<path d="M6 1.5v4M10 1.5v4M3.5 5.5h9V8a4.5 4.5 0 0 1-9 0zM8 12.5v2"/>',
  logs: '<path d="M3 3h10M3 6h10M3 9h7M3 12h5"/>',
  copy: '<rect x="5" y="5" width="8.5" height="9" rx="1"/><path d="M3 11V2.5a1 1 0 0 1 1-1h6.5"/>',
  vector: '<circle cx="3.5" cy="12.5" r="1.5"/><circle cx="12.5" cy="3.5" r="1.5"/><circle cx="11" cy="11" r="1.5"/><path d="M4.6 11.4l6.8-6.8M5 12.4h4.5"/>',
  health: '<path d="M8 13.5S2 10 2 5.8A3 3 0 0 1 8 4.5a3 3 0 0 1 6 1.3C14 10 8 13.5 8 13.5z"/><path d="M4.5 8h2l1-1.5 1.5 3 1-1.5h1.5"/>',
  sparkle: '<path d="M8 1.5l1.4 4.1 4.1 1.4-4.1 1.4L8 12.5 6.6 8.4 2.5 7l4.1-1.4z"/><path d="M13 11.5v3M11.5 13h3"/>',
  settings:
    '<path d="M2 4.5h12M2 8h12M2 11.5h12"/><circle cx="6" cy="4.5" r="1.6" fill="var(--rail, #fff)"/><circle cx="10.5" cy="8" r="1.6" fill="var(--rail, #fff)"/><circle cx="5" cy="11.5" r="1.6" fill="var(--rail, #fff)"/>',
  sun: '<circle cx="8" cy="8" r="3"/><path d="M8 1.5v1.8M8 12.7v1.8M1.5 8h1.8M12.7 8h1.8M3.4 3.4l1.3 1.3M11.3 11.3l1.3 1.3M3.4 12.6l1.3-1.3M11.3 4.7l1.3-1.3"/>',
  moon: '<path d="M13.5 9.8A5.8 5.8 0 0 1 6.2 2.5a5.8 5.8 0 1 0 7.3 7.3z"/>',
  search: '<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/>',
  more: '<circle cx="3.5" cy="8" r="1.1" fill="currentColor"/><circle cx="8" cy="8" r="1.1" fill="currentColor"/><circle cx="12.5" cy="8" r="1.1" fill="currentColor"/>',
  layers: '<path d="M8 2.5l6 3-6 3-6-3z"/><path d="M2 8.5l6 3 6-3M2 11l6 3 6-3"/>',
  terminal: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M4.5 6.5l2 1.5-2 1.5M8 10h3.5"/>',
  bolt: '<path d="M9 1.5L3.5 9h4l-.5 5.5L12.5 7h-4z"/>',
  panel: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M10 3v10"/>',
  bookmark: '<path d="M4 2.5h8v11l-4-3-4 3z"/>',
  laptop: '<rect x="3" y="3" width="10" height="7" rx="1"/><path d="M1.5 12.5h13M6.5 12.5v-2.5h3v2.5"/>',
  cloud: '<path d="M5 12.5h6.5a2.75 2.75 0 0 0 .4-5.47A4 4 0 0 0 4.2 8.1 2.2 2.2 0 0 0 5 12.5z"/>',
  shield: '<path d="M8 1.5l5.5 2v4.2c0 3.2-2.3 5.6-5.5 6.8-3.2-1.2-5.5-3.6-5.5-6.8V3.5z"/><path d="M5.5 8l1.8 1.8L10.5 6.5"/>',
  flask: '<path d="M6 1.5h4M6.5 1.5v4.5L2.8 12.3a1 1 0 0 0 .9 1.5h8.6a1 1 0 0 0 .9-1.5L9.5 6V1.5"/><path d="M4.5 10h7"/>',
  briefcase: '<rect x="1.5" y="4.5" width="13" height="9" rx="1.5"/><path d="M5.5 4.5v-2h5v2M1.5 8.5h13"/>',
  rocket: '<path d="M8 1.5c2.5 1.5 3.5 4.5 3 8l-3 3-3-3c-.5-3.5.5-6.5 3-8z"/><circle cx="8" cy="6.5" r="1.2"/><path d="M5 9.5l-2 1 1 2M11 9.5l2 1-1 2"/>',
  globe: '<circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12"/>',
  mariadb:
    '<path d="M14.5 2.5c-2 0-2.6 1.2-3.6 2.6S8.8 7 7 7.3 3.6 8.2 2.5 10c.8-.3 1.6-.4 2.3-.3-.6.9-1.3 2-1.3 3.3 1.5-.6 2.2-1.9 3.4-2.6s2.9-.4 4.4-1.5c1.4-1 1.6-3 2.1-4.3.3-.9.9-1.4 1.1-2.1z"/>',
  mysql:
    '<path d="M2 12.5c.3-3.3 1.4-6 4-7.5 1.3-.8 2.4-.4 3.3.5l1.4 1.5c.8.9 2 1 3.3.6-.4 1.2-1.4 1.9-2.6 1.9l1.1 2.2c-1.6-.3-2.6-1.3-3.4-2.6-.9.7-1.9 1-3.1 1.1-1.2 1-2.3 1.6-4 2.3z"/>',
  mssql:
    '<rect x="2" y="2" width="5.2" height="5.2" rx="0.8"/><rect x="8.8" y="2" width="5.2" height="5.2" rx="0.8"/><rect x="2" y="8.8" width="5.2" height="5.2" rx="0.8"/><rect x="8.8" y="8.8" width="5.2" height="5.2" rx="0.8"/>',
  mongodb: '<path d="M8 1.5c3.3 2.6 4.8 5.4 4.3 8.4-.4 2.4-2.1 4-4.3 4.6-2.2-.6-3.9-2.2-4.3-4.6C3.2 6.9 4.7 4.1 8 1.5z"/><path d="M8 4.5v10"/>',
  send: '<path d="M2.5 8L13.5 2.5 10 13.5 7.5 9z"/><path d="M7.5 9l6-6.5"/>',
  columns: '<rect x="2" y="2.5" width="12" height="11" rx="1"/><path d="M6 2.5v11M10 2.5v11"/>',
  expand: '<path d="M9.5 2.5h4v4M13.5 2.5L9 7M6.5 13.5h-4v-4M2.5 13.5L7 9"/>',
  history: '<path d="M2.5 8a5.5 5.5 0 1 0 1.6-3.9"/><path d="M2.5 2.5V5H5"/><path d="M8 5v3l2 1.5"/>',
} as const;

export type IconName = keyof typeof ICONS;

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attributes: Record<string, unknown> = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  for (const [name, value] of Object.entries(attributes)) setAttribute(element, name, value);
  for (const child of children) {
    if (child === null || child === undefined || child === false) continue;
    element.append(child instanceof Node ? child : String(child));
  }
  return element;
}

function setAttribute(element: HTMLElement, name: string, value: unknown): void {
  if (value === undefined || value === null || value === false) return;
  if (name === "class") element.className = String(value);
  else if (name.startsWith("on") && typeof value === "function") {
    element.addEventListener(name.slice(2).toLowerCase(), value as EventListener);
  } else if (PROPERTIES.has(name)) Reflect.set(element, name, value);
  else element.setAttribute(name, value === true ? "" : String(value));
}

export function icon(name: IconName, extraClass = ""): HTMLElement {
  const element = h("span", { class: `icon ${extraClass}`.trim(), "aria-hidden": "true" });
  element.innerHTML = `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">${ICONS[name]}</svg>`;
  return element;
}

export function setIcon(host: HTMLElement, name: IconName): void {
  host.querySelector(".icon")?.replaceWith(icon(name));
}

export function toolButton(iconName: IconName, label: string, onClick: () => void, title = label): HTMLButtonElement {
  const button = h("button", { class: "tool-button", title, type: "button" }, icon(iconName), h("span", {}, label));
  button.addEventListener("click", onClick);
  return button;
}

export function iconButton(iconName: IconName, title: string, onClick: () => void): HTMLButtonElement {
  const button = h("button", { class: "icon-button", title, "aria-label": title, type: "button" }, icon(iconName));
  button.addEventListener("click", (event) => {
    event.stopPropagation();
    onClick();
  });
  return button;
}

export function bigButton(iconName: IconName, label: string, onClick: () => void, title = label): HTMLButtonElement {
  const button = h("button", { class: "big-button", type: "button", title }, icon(iconName), h("span", {}, label));
  button.addEventListener("click", onClick);
  return button;
}

export function separator(): HTMLElement {
  return h("div", { class: "toolbar-separator" });
}

export function spacer(): HTMLElement {
  return h("div", { class: "spacer" });
}

export async function copyText(text: string): Promise<void> {
  await navigator.clipboard.writeText(text);
}

export function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

const numberFormat = new Intl.NumberFormat();

export function formatNumber(value: number): string {
  return numberFormat.format(value);
}

export function formatCompact(value: number): string {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}k`;
  return String(value);
}
