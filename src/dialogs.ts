import { errorText } from "./api";
import { h } from "./dom";

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

export function openModal(title: string, body: HTMLElement, actions: ModalAction[], options: ModalOptions = {}): void {
  const footer = h("div", { class: "modal-footer" });
  const dialog = h(
    "div",
    { class: "modal", style: `width:${options.width ?? 460}px` },
    h("div", { class: "modal-title" }, title),
    h("div", { class: "modal-body" }, body),
    footer,
  );
  const overlay = h("div", { class: "modal-overlay" }, dialog);
  const close = () => {
    overlay.remove();
    document.removeEventListener("keydown", onKey);
    options.onClose?.();
  };
  const buttons = actions.map((action) => actionButton(action, close));
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "Escape") close();
    if (event.key === "Enter" && event.target instanceof HTMLInputElement) {
      buttons.find((_, index) => actions[index].primary)?.click();
    }
  };
  footer.append(...buttons);
  document.body.append(overlay);
  document.addEventListener("keydown", onKey);
  dialog.querySelector<HTMLElement>("input:not([type=checkbox]), select, textarea")?.focus();
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
      toast(errorText(error), "error");
    } finally {
      button.disabled = false;
    }
  });
  return button;
}

export function confirmDialog(message: string, confirmLabel = "OK"): Promise<boolean> {
  return new Promise((resolve) => {
    let confirmed = false;
    openModal("Confirm", h("p", { class: "confirm-text" }, message), [
      { label: "Cancel", run: () => undefined },
      { label: confirmLabel, primary: true, danger: confirmLabel === "Delete", run: () => void (confirmed = true) },
    ], { width: 400, onClose: () => resolve(confirmed) });
  });
}

export function promptPassword(connectionName: string): Promise<{ password: string; remember: boolean } | null> {
  return new Promise((resolve) => {
    let answer: { password: string; remember: boolean } | null = null;
    const password = textInput("", { type: "password" });
    const remember = checkbox("Save password in Windows Credential Manager", true);
    openModal(`Password for ${connectionName}`, h("div", { class: "form-grid" }, field("Password", password), remember.element), [
      { label: "Cancel", run: () => undefined },
      { label: "Connect", primary: true, run: () => void (answer = { password: password.value, remember: remember.input.checked }) },
    ], { width: 400, onClose: () => resolve(answer) });
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
