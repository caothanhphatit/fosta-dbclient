/** Where a floating element (a menu, a popover) goes so it stays inside the window. Pure, so it is unit-tested. */

export interface Placement {
  left: number;
  top: number;
  /** The height it may take; a taller element scrolls. */
  maxHeight: number;
}

/**
 * Places an element of `width` × `height` at (`x`, `y`), moved left and up as far as needed to keep `margin` pixels from
 * the window edges. An element taller than the window gets the window's height (minus the margins) and scrolls.
 * Measure the size untransformed (`offsetWidth` / `offsetHeight`): an opening animation that scales the element makes
 * `getBoundingClientRect` a few percent short, and the element then ends up partly outside.
 */
export function placeInViewport(x: number, y: number, width: number, height: number, viewportWidth: number, viewportHeight: number, margin = 4): Placement {
  const maxHeight = Math.max(0, viewportHeight - 2 * margin);
  const shown = Math.min(height, maxHeight);
  const left = Math.max(margin, Math.min(x, viewportWidth - width - margin));
  const top = Math.max(margin, Math.min(y, viewportHeight - shown - margin));
  return { left, top, maxHeight };
}

/** The next index of a keyboard move over `count` entries (`enabled` says which can be focused); -1 when none can. */
export function nextFocusable(current: number, step: 1 | -1 | "first" | "last", count: number, enabled: (index: number) => boolean): number {
  if (count === 0) return -1;
  if (step === "first" || step === "last") {
    const from = step === "first" ? 0 : count - 1;
    const direction = step === "first" ? 1 : -1;
    for (let index = from; index >= 0 && index < count; index += direction) if (enabled(index)) return index;
    return -1;
  }
  for (let moved = 1; moved <= count; moved += 1) {
    const index = ((((current < 0 && step === -1 ? 0 : current) + step * moved) % count) + count) % count;
    if (enabled(index)) return index;
  }
  return -1;
}
