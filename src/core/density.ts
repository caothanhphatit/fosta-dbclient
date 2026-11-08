export type Density = "compact" | "comfortable";

const STORAGE_KEY = "db-studio.density";
const listeners = new Set<() => void>();
let current: Density = read();

function read(): Density {
  try {
    return localStorage.getItem(STORAGE_KEY) === "comfortable" ? "comfortable" : "compact";
  } catch {
    return "compact";
  }
}

export function density(): Density {
  return current;
}

/** Switches the spacing of grid rows, tree rows, tabs and toolbar (CSS variables under `html[data-density]`). */
export function setDensity(next: Density): void {
  current = next;
  try {
    localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // Storage can be unavailable; the choice then lasts for this session only.
  }
  apply();
}

export function onDensityChange(listener: () => void): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

export function initDensity(): void {
  apply();
}

function apply(): void {
  document.documentElement.dataset.density = current;
  for (const listener of listeners) listener();
}
