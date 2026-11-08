import { describe, expect, it } from "vitest";
import { TypingBuffer } from "./typingBuffer";

describe("TypingBuffer", () => {
  it("keeps printable keys, line breaks and backspaces typed while the editor loads", () => {
    const buffer = new TypingBuffer();
    for (const key of [..."SELECT 1", "Enter", ..."FROM t", "Backspace", "x", "Shift", "ArrowLeft"]) buffer.handle({ key });
    expect(buffer.value).toBe("SELECT 1\nFROM x");
  });

  it("ignores shortcuts", () => {
    const buffer = new TypingBuffer();
    expect(buffer.handle({ key: "t", ctrlKey: true })).toBe(false);
    expect(buffer.handle({ key: "é" })).toBe(true);
    expect(buffer.value).toBe("é");
  });
});
