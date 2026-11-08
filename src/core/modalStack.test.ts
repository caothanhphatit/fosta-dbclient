import { describe, expect, it } from "vitest";
import { ModalStack, type ModalEntry, type StackKeyEvent } from "./modalStack";

interface FakeModal extends ModalEntry {
  closed: number;
  primary: number;
}

/** A dialog that closes on Esc and runs its primary action on Enter inside it. */
function fakeModal(stack: ModalStack, element: object): FakeModal {
  const modal: FakeModal = {
    closed: 0,
    primary: 0,
    contains: (target) => target === element,
    handleKey: (event, inside) => {
      if (event.key === "Escape") {
        modal.closed += 1;
        remove();
        return true;
      }
      if (event.key === "Enter" && inside) {
        modal.primary += 1;
        return true;
      }
      return false;
    },
  };
  const remove = stack.push(modal);
  return modal;
}

function key(name: string, target: unknown): StackKeyEvent & { stopped: boolean } {
  const event = {
    key: name,
    target,
    stopped: false,
    stopPropagation: () => void (event.stopped = true),
    preventDefault: () => undefined,
  };
  return event;
}

describe("ModalStack", () => {
  it("closes only the top-most dialog on Esc", () => {
    const stack = new ModalStack();
    const importDialog = {};
    const confirmation = {};
    const lower = fakeModal(stack, importDialog);
    const upper = fakeModal(stack, confirmation);
    stack.dispatch(key("Escape", confirmation));
    expect(upper.closed).toBe(1);
    expect(lower.closed).toBe(0);
    expect(stack.size).toBe(1);
    stack.dispatch(key("Escape", pageBody));
    expect(lower.closed).toBe(1);
    expect(stack.size).toBe(0);
  });

  it("runs Enter only on the top-most dialog", () => {
    const stack = new ModalStack();
    const lowerElement = {};
    const upperElement = {};
    const lower = fakeModal(stack, lowerElement);
    const upper = fakeModal(stack, upperElement);
    stack.dispatch(key("Enter", upperElement));
    // Focus left behind in the lower dialog does not trigger anything either.
    stack.dispatch(key("Enter", lowerElement));
    expect(upper.primary).toBe(1);
    expect(lower.primary).toBe(0);
  });

  it("keeps keys aimed behind a dialog from reaching the app", () => {
    const stack = new ModalStack();
    fakeModal(stack, {});
    const shortcut = key("n", pageBody);
    stack.dispatch(shortcut);
    expect(shortcut.stopped).toBe(true);
  });

  it("lets typing inside the dialog through", () => {
    const stack = new ModalStack();
    const element = {};
    fakeModal(stack, element);
    const typing = key("a", element);
    stack.dispatch(typing);
    expect(typing.stopped).toBe(false);
  });

  it("does nothing without dialogs", () => {
    const stack = new ModalStack();
    const event = key("Escape", pageBody);
    stack.dispatch(event);
    expect(event.stopped).toBe(false);
  });
});

const pageBody = { body: true };
