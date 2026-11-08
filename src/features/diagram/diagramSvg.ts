/** SVG drawing helpers and the theme palette of the ER diagram. */

export const SVG = "http://www.w3.org/2000/svg";

export interface Palette {
  canvas: string;
  dot: string;
  surface: string;
  border: string;
  fg: string;
  fg2: string;
  fg3: string;
  accent: string;
  accentFg: string;
  view: string;
  collection: string;
  key: string;
  line: string;
  highlight: string;
}

export function svg<K extends keyof SVGElementTagNameMap>(tag: K, attributes: Record<string, string | number>): SVGElementTagNameMap[K] {
  const element = document.createElementNS(SVG, tag);
  for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, String(value));
  return element;
}

export function text(value: string, x: number, y: number, attributes: Record<string, string | number>): SVGTextElement {
  const element = svg("text", { x, y, ...attributes });
  element.textContent = value;
  return element;
}

export function setDisplayed(element: SVGElement | null, visible: boolean): void {
  if (!element) return;
  const value = visible ? "" : "none";
  if (element.style.display !== value) element.style.display = value;
}

export function truncate(value: string, limit: number): string {
  return value.length > limit ? `${value.slice(0, limit - 1)}…` : value;
}

/** Resolves the theme's CSS variables into concrete colours, so exports look like the screen. */
export function readPalette(): Palette {
  const style = getComputedStyle(document.documentElement);
  const value = (name: string) => style.getPropertyValue(name).trim();
  return {
    canvas: value("--canvas"),
    dot: value("--border-strong"),
    surface: value("--surface"),
    border: value("--border-strong"),
    fg: value("--fg"),
    fg2: value("--fg-2"),
    fg3: value("--fg-3"),
    accent: value("--accent"),
    accentFg: value("--accent-fg"),
    view: value("--ws-purple"),
    collection: value("--ws-green"),
    key: value("--warning"),
    line: value("--fg-3"),
    highlight: value("--accent"),
  };
}
