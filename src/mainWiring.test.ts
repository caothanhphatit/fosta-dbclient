/**
 * Wiring in `main.ts` that has no unit of its own (the app shell needs the DOM and the backend), checked on its source.
 */
import { describe, expect, it } from "vitest";
import main from "./main.ts?raw";

describe("the app shell", () => {
  it("refreshes the assistant's context when the tree selection changes", () => {
    // The tree's row handlers select first; listeners on the tree element run as the event bubbles, so the panel reads
    // the new selection at once (not one click late).
    const wiring = /for \(const type of \[([^\]]*)\]\) \{\s*tree\.element\.addEventListener\(type, \(\) => \{\s*assistant\.refreshContext\(\);/.exec(main);
    expect(wiring, "tree selection must call assistant.refreshContext()").not.toBeNull();
    expect(wiring![1]).toContain('"click"');
    expect(wiring![1]).toContain('"keydown"');
  });

  it("passes the text the chat shows for an inline Ask to the panel", () => {
    expect(main).toMatch(/askAssistant: \(prompt, extra, samples, shown\) => \{[^}]*assistant\.ask\(prompt, extra, samples, shown\)/);
  });
});
