import { showError } from "./errorPanel";
import { capabilities } from "./capabilities";
import { h } from "./dom";
import { ModalStack } from "./modalStack";

export interface ModalAction {
  label: string;
  primary?: boolean;
  danger?: boolean;
  alignLeft?: boolean;
  run: () => Promise<boolean | void> | boolean | void;
}

interface ModalOptions {
  width?: number;
  onClose?: () => void;
}

/** Opens a dialog on the modal stack; returns the function that closes it (for bodies with actions of their own). */
export function openModal(title: string, body: HTMLElement, actions: ModalAction[], options: ModalOptions = {}): () => void {
  const footer = h("div", { class: "modal-footer" });
  const dialog = h(
    "div",
    { class: "modal", style: `width:${options.width ?? 460}px` },
    h("div", { class: "modal-title" }, title),
    h("div", { class: "modal-body" }, body),
    footer,
  );
  const overlay = h("div", { class: "modal-overlay" }, dialog);
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    overlay.remove();
    unregister();
    options.onClose?.();
  };
  const buttons = actions.map((action) => actionButton(action, close));
  const unregister = registerModal(overlay, (event, inside) => {
    if (event.key === "Escape") {
      close();
      return true;
    }
    if (event.key === "Enter" && inside && event.target instanceof HTMLInputElement && !event.isComposing) {
      buttons.find((_, index) => actions[index].primary)?.click();
      return true;
    }
    return false;
  });
  footer.append(...buttons);
  document.body.append(overlay);
  focusInto(dialog, dialog.querySelector<HTMLElement>("input:not([type=checkbox]), select, textarea"));
  return close;
}

const modals = new ModalStack();
let modalKeysInstalled = false;

/** True while any modal dialog is open; global shortcuts are ignored then. */
export function isModalOpen(): boolean {
  return modals.size > 0;
}

/**
 * Adds a dialog to the modal stack. `handleKey` sees keys only while the dialog is the top-most one; it
 * returns true when it used the key. Returns the function that removes the dialog from the stack.
 */
export function registerModal(element: HTMLElement, handleKey: (event: KeyboardEvent, inside: boolean) => boolean): () => void {
  if (!modalKeysInstalled) {
    modalKeysInstalled = true;
    // Capturing, so the top-most dialog sees keys before the views and shortcuts behind it.
    document.addEventListener("keydown", (event) => modals.dispatch(event), true);
  }
  return modals.push({
    contains: (target) => target instanceof Node && element.contains(target),
    handleKey: (event, inside) => handleKey(event as KeyboardEvent, inside),
  });
}

/**
 * Moves focus into a dialog that just opened. Blurring whatever had focus also closes editor popups
 * (such as the SQL completion list) that would otherwise stay drawn above the dialog.
 */
export function focusInto(dialog: HTMLElement, preferred: HTMLElement | null): void {
  if (document.activeElement instanceof HTMLElement && !dialog.contains(document.activeElement)) document.activeElement.blur();
  if (preferred) {
    preferred.focus();
    return;
  }
  if (!dialog.hasAttribute("tabindex")) dialog.tabIndex = -1;
  dialog.focus();
}

function actionButton(action: ModalAction, close: () => void): HTMLButtonElement {
  const classes = ["btn", action.primary ? "primary" : "", action.danger ? "danger" : "", action.alignLeft ? "align-left" : ""];
  const button = h("button", { class: classes.filter(Boolean).join(" "), type: "button" }, action.label);
  button.addEventListener("click", async () => {
    button.disabled = true;
    try {
      const keepOpen = (await action.run()) === false;
      if (!keepOpen) close();
    } catch (error) {
      showError(error, action.label);
    } finally {
      button.disabled = false;
    }
  });
  return button;
}

export function confirmDialog(message: string, confirmLabel = "OK"): Promise<boolean> {
  return new Promise((resolve) => {
    let confirmed = false;
    openModal(
      "Confirm",
      h("p", { class: "confirm-text" }, message),
      [
        { label: "Cancel", run: () => undefined },
        { label: confirmLabel, primary: true, danger: confirmLabel === "Delete", run: () => void (confirmed = true) },
      ],
      { width: 400, onClose: () => resolve(confirmed) },
    );
  });
}

export function promptPassword(connectionName: string): Promise<{ password: string; remember: boolean } | null> {
  return new Promise((resolve) => {
    let answer: { password: string; remember: boolean } | null = null;
    const password = textInput("", { type: "password" });
    const remember = checkbox(`Save password in ${capabilities().secretStore}`, true);
    openModal(
      `Password for ${connectionName}`,
      h("div", { class: "form-grid" }, field("Password", password), remember.element),
      [
        { label: "Cancel", run: () => undefined },
        { label: "Connect", primary: true, run: () => void (answer = { password: password.value, remember: remember.input.checked }) },
      ],
      { width: 400, onClose: () => resolve(answer) },
    );
  });
}

export function field(label: string, control: HTMLElement, hint = ""): HTMLElement {
  return h("label", { class: "field" }, h("span", { class: "field-label" }, label), control, hint ? h("span", { class: "field-hint" }, hint) : null);
}

export function textInput(value: string, attributes: Record<string, unknown> = {}): HTMLInputElement {
  return h("input", { class: "input", type: "text", value, spellcheck: "false", autocomplete: "off", ...attributes });
}

export function checkbox(label: string, checked: boolean): { element: HTMLElement; input: HTMLInputElement } {
  const input = h("input", { type: "checkbox", checked });
  return { element: h("label", { class: "checkbox" }, input, h("span", {}, label)), input };
}

export function toast(message: string, kind: "info" | "success" | "error" = "info"): void {
  const host = document.querySelector(".toasts") ?? document.body.appendChild(h("div", { class: "toasts" }));
  const item = h("div", { class: `toast ${kind}` }, message);
  item.addEventListener("click", () => item.remove());
  host.append(item);
  setTimeout(() => item.remove(), kind === "error" ? 9000 : 3500);
}
