/**
 * The "Tunnel command" section of the connection dialog: a program the backend starts before connecting and stops on
 * disconnect (`src-tauri/src/tunnel.rs`). Hidden where startup tasks are off (web edition without FOSTA_STUDIO_TASKS).
 *
 *     const tunnel = renderTunnelSection(profile, { template, applyEndpoint });
 *     profile.tunnel = tunnel.read();   // TunnelConfig | null
 */
import { capabilities } from "../../core/capabilities";
import { field, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { ConnectionProfile, TunnelConfig } from "../../core/types";
import { tunnelFromValues, tunnelValues, type TunnelValues } from "./tunnelForm";

export interface TunnelSection {
  element: HTMLElement;
  read(): TunnelConfig | null;
}

export interface TunnelSectionHooks {
  /** The template for the engine selected now. */
  template(): TunnelValues;
  /** Called after the template is filled, with its values (to point host and port at the local end). */
  filled(values: TunnelValues): void;
}

export function renderTunnelSection(profile: Pick<ConnectionProfile, "tunnel"> | null, hooks: TunnelSectionHooks): TunnelSection {
  const saved = profile?.tunnel ?? null;
  if (!capabilities().tasks) {
    // Kept as saved, so the backend can say why it cannot run.
    return { element: h("details", { class: "conn-section", hidden: true }), read: () => saved };
  }
  const initial = tunnelValues(saved);
  const program = textInput(initial.program, { class: "input mono", placeholder: "kubectl" });
  const args = textInput(initial.args, { class: "input mono", placeholder: "port-forward -n <namespace> svc/<service> <local>:<remote>" });
  const readyPattern = textInput(initial.readyPattern, { placeholder: "Empty: when host:port accepts connections" });
  const timeout = textInput(initial.timeout, { type: "number", min: 1, max: 600 });
  const template = h("button", { class: "link-button", type: "button" }, "Fill kubectl port-forward template");
  template.addEventListener("click", () => {
    const values = hooks.template();
    program.value = values.program;
    args.value = values.args;
    readyPattern.value = values.readyPattern;
    timeout.value = values.timeout;
    hooks.filled(values);
  });
  const element = h(
    "details",
    { class: "conn-section" },
    h("summary", {}, "Tunnel command"),
    h(
      "div",
      { class: "form-grid conn-section-body" },
      h("p", { class: "driver-note" }, "Runs before connecting, stops on disconnect. ", template),
      field("Program", program),
      field("Arguments", args),
      h("div", { class: "field-row" }, field("Ready when output contains", readyPattern), h("div", { class: "narrow" }, field("Timeout (s)", timeout))),
    ),
  );
  if (saved) element.open = true;
  return {
    element,
    read: () => tunnelFromValues({ program: program.value, args: args.value, readyPattern: readyPattern.value, timeout: timeout.value }),
  };
}
