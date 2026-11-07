export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "db-studio.theme";
const media = window.matchMedia("(prefers-color-scheme: dark)");
const listeners = new Set<() => void>();
let preference: ThemePreference = readPreference();

export function themePreference(): ThemePreference {
  return preference;
}

export function resolvedTheme(): ResolvedTheme {
  if (preference === "system") return media.matches ? "dark" : "light";
  return preference;
}

export function setTheme(next: ThemePreference): void {
  preference = next;
  try {
    localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // Storage can be unavailable; the choice then lasts for this session only.
  }
  apply();
}

/** Flips between light and dark (leaving "system" mode). Returns the new preference. */
export function toggleTheme(): ThemePreference {
  const next: ThemePreference = resolvedTheme() === "dark" ? "light" : "dark";
  setTheme(next);
  return next;
}

export function describeTheme(): string {
  const resolved = resolvedTheme() === "dark" ? "Dark" : "Light";
  return preference === "system" ? `Theme: ${resolved} (follows system)` : `Theme: ${resolved}`;
}

/** Tooltip for the toggle button: what one click will do. */
export function describeToggle(): string {
  const action = resolvedTheme() === "dark" ? "Switch to light theme" : "Switch to dark theme";
  return `${action} (right-click: follow system)`;
}

export function onThemeChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export function initTheme(): void {
  apply();
  media.addEventListener("change", apply);
}

function apply(): void {
  document.documentElement.dataset.theme = resolvedTheme();
  for (const listener of listeners) listener();
}

function readPreference(): ThemePreference {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === "light" || stored === "dark" || stored === "system") return stored;
  } catch {
    // Fall through to the default.
  }
  return "system";
}
