/**
 * Wiring in `main.ts` that has no unit of its own (the app shell needs the DOM and the backend), checked on its source.
 */
import { describe, expect, it } from "vitest";
import main from "./main.ts?raw";
import navigation from "./app/navigation.ts?raw";
import queryView from "./features/query/queryView.ts?raw";

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

describe("assistant panel wiring", () => {
  it("rechecks what the assistant knows after DDL from a query tab", () => {
    expect(main).toMatch(/schemaMayHaveChanged: \(connectionId\) => assistant\.schemaMayHaveChanged\(connectionId\)/);
    expect(navigation).toMatch(/schemaMayHaveChanged: \(id\) => this\.host\.schemaMayHaveChanged\(id\)/);
    expect(queryView).toMatch(
      /if \(changesSchema\(text\) && results\.entries\.length > 0\) \{\s*this\.refreshNames\(\);\s*this\.host\.schemaMayHaveChanged\?\.\(this\.connectionId\);/,
    );
  });

  it("reloads an open knowledge tab when a learn run from anywhere is over", () => {
    const learn = /async function learnDatabase[\s\S]*?\n}\n/.exec(main)?.[0] ?? "";
    expect(learn).toMatch(/tabs\.contentOf\(knowledgeTabKey\(workspaceId\(\), connectionId, database\)\)/);
    expect(learn).toMatch(/open instanceof ProfileView\) open\.reload\(\)/);
  });

  it("moves the focus into the composer only from the keyboard shortcut", () => {
    expect(main).toMatch(/if \(composerTakesFocus\(byKeyboard, typingElsewhere\(document\.activeElement, assistant\.element\)\)\) assistant\.focus\(\);/);
    expect(main).toMatch(/toggleAssistant: \(\) => toggleAssistant\(true\)/);
    expect(main).toMatch(/toggleAssistant: \(\) => toggleAssistant\(\),/);
    // Asking from elsewhere opens the panel without taking the focus.
    expect(main).toMatch(/showAssistant\(true\);\s*assistant\.ask\(/);
    expect(main.match(/assistant\.focus\(\)/g)).toHaveLength(1);
  });
});
