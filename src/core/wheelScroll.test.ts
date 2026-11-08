import { describe, expect, it } from "vitest";
import { horizontalWheelDelta } from "./wheelScroll";

describe("horizontalWheelDelta", () => {
  it("turns a vertical wheel into horizontal scrolling", () => {
    expect(horizontalWheelDelta({ deltaX: 0, deltaY: 100, deltaMode: 0 })).toBe(100);
    expect(horizontalWheelDelta({ deltaX: 0, deltaY: -100, deltaMode: 0 })).toBe(-100);
  });

  it("keeps horizontal swipes", () => {
    expect(horizontalWheelDelta({ deltaX: 40, deltaY: 3, deltaMode: 0 })).toBe(40);
  });

  it("converts line-based deltas to pixels", () => {
    expect(horizontalWheelDelta({ deltaX: 0, deltaY: 3, deltaMode: 1 })).toBe(48);
  });

  it("ignores Ctrl+wheel (zoom)", () => {
    expect(horizontalWheelDelta({ deltaX: 0, deltaY: 100, deltaMode: 0, ctrlKey: true })).toBe(0);
  });
});
