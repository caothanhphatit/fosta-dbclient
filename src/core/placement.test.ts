import { describe, expect, it } from "vitest";
import { nextFocusable, placeInViewport } from "./placement";

describe("placeInViewport", () => {
  it("keeps a menu that fits where it was asked", () => {
    expect(placeInViewport(100, 100, 200, 300, 1280, 800)).toEqual({ left: 100, top: 100, maxHeight: 792 });
  });

  it("moves a menu near the bottom-right corner up and left by its full size", () => {
    // The Settings menu: opened near the bottom, it must end 4 px above the edge (not 3 % short of it).
    const placed = placeInViewport(1200, 700, 220, 260, 1280, 800);
    expect(placed.left + 220).toBe(1276);
    expect(placed.top + 260).toBe(796);
  });

  it("caps a menu taller than the window and pins it to the top margin", () => {
    // 25 tabs in the tab list (814 px) in a 600 px window.
    expect(placeInViewport(300, 40, 240, 814, 900, 600)).toEqual({ left: 300, top: 4, maxHeight: 592 });
  });

  it("never goes past the left or top margin", () => {
    expect(placeInViewport(-50, -20, 2000, 100, 900, 600)).toMatchObject({ left: 4, top: 4 });
  });
});

describe("nextFocusable", () => {
  const all = () => true;

  it("starts at the first or last entry", () => {
    expect(nextFocusable(-1, 1, 3, all)).toBe(0);
    expect(nextFocusable(-1, -1, 3, all)).toBe(2);
    expect(nextFocusable(-1, "last", 3, all)).toBe(2);
  });

  it("wraps around and skips disabled entries", () => {
    expect(nextFocusable(2, 1, 3, all)).toBe(0);
    expect(nextFocusable(0, 1, 4, (index) => index !== 1)).toBe(2);
    expect(nextFocusable(0, -1, 4, (index) => index !== 3)).toBe(2);
    expect(nextFocusable(-1, "first", 3, (index) => index > 0)).toBe(1);
  });

  it("finds nothing when no entry can take focus", () => {
    expect(nextFocusable(0, 1, 3, () => false)).toBe(-1);
    expect(nextFocusable(0, 1, 0, all)).toBe(-1);
  });
});
