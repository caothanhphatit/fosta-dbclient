/**
 * The TLS section of the connection dialog, driven by the descriptor's `connection.tls` (`TlsSupport`): the modes it
 * lists, a CA file when `ca`, client certificate and key when `clientCert`, a server name when `serverName`, a
 * certificate pin when `pin`, and "Accept invalid certificates" (encrypt without checking the certificate: self-signed
 * test servers; SQL Server tools call it "Trust server certificate").
 *
 * API (kept small; the SSH/TLS work owns this file):
 *
 *     const tls = renderTlsSection(profile, descriptor);
 *     form.append(tls.element);          // a collapsible section; empty and hidden when the engine has no TLS settings
 *     profile.tls = tls.read();          // TlsConfig | null
 *     secrets = { ...tls.secrets() };    // the client key passphrase (slot tlsKeyPassphrase), when typed
 *     tls.pointAtAcceptInvalid();        // after an untrusted-certificate error: open and highlight the checkbox
 *
 * `read()` returns null when the engine has no TLS settings, or when the profile had none and the user left the mode on
 * "Driver default" without setting anything: the profile then keeps the engine's own default (as before v2).
 */
import type { EngineDescriptor, SecretSlot, TlsConfig, TlsModeId, TlsSupport } from "../../core/contract";
import { checkbox, field, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { ConnectionProfile } from "../../core/types";

export interface TlsSection {
  element: HTMLElement;
  read(): TlsConfig | null;
  secrets(): Partial<Record<SecretSlot, string>>;
  /** Opens the section and highlights "Accept invalid certificates"; false when the engine has none or it is ticked. */
  pointAtAcceptInvalid(): boolean;
}

export const TLS_MODE_LABELS: Record<TlsModeId, string> = {
  disable: "Disable",
  prefer: "Prefer (TLS when the server offers it)",
  require: "Require",
  verifyCa: "Verify CA",
  verifyFull: "Verify CA and host name",
};

/** What the inputs hold; "" for the mode is "Driver default". */
export interface TlsValues {
  mode: TlsModeId | "";
  caFile: string;
  clientCertFile: string;
  clientKeyFile: string;
  serverName: string;
  pinnedSha256: string;
  acceptInvalidCerts: boolean;
}

export function tlsValues(tls: TlsConfig | null | undefined): TlsValues {
  return {
    mode: tls?.mode ?? "",
    caFile: tls?.caFile ?? "",
    clientCertFile: tls?.clientCertFile ?? "",
    clientKeyFile: tls?.clientKeyFile ?? "",
    serverName: tls?.serverName ?? "",
    pinnedSha256: tls?.pinnedSha256 ?? "",
    acceptInvalidCerts: tls?.acceptInvalidCerts ?? false,
  };
}

/** The mode used when settings are set on "Driver default": Require if offered, else the first mode that encrypts. */
export function fallbackMode(support: TlsSupport): TlsModeId {
  return support.modes.includes("require") ? "require" : (support.modes.find((mode) => mode !== "disable") ?? "prefer");
}

/** The config for the inputs (only what `support` offers); null for "Driver default" with nothing else set. */
export function tlsFromValues(values: TlsValues, support: TlsSupport): TlsConfig | null {
  const text = (value: string, offered: boolean) => (offered && value.trim() ? value.trim() : null);
  const config: TlsConfig = {
    mode: values.mode || fallbackMode(support),
    caFile: text(values.caFile, support.ca),
    clientCertFile: text(values.clientCertFile, support.clientCert),
    clientKeyFile: text(values.clientKeyFile, support.clientCert),
    serverName: text(values.serverName, support.serverName),
    acceptInvalidCerts: values.acceptInvalidCerts,
    pinnedSha256: text(values.pinnedSha256, support.pin),
  };
  const anySet = config.caFile || config.clientCertFile || config.clientKeyFile || config.serverName || config.pinnedSha256 || config.acceptInvalidCerts;
  return values.mode === "" && !anySet ? null : config;
}

export function renderTlsSection(profile: Pick<ConnectionProfile, "tls"> | null, descriptor: EngineDescriptor): TlsSection {
  const support = descriptor.connection.tls;
  if (!support) {
    const element = h("details", { class: "conn-section", hidden: true });
    return { element, read: () => null, secrets: () => ({}), pointAtAcceptInvalid: () => false };
  }
  const initial = tlsValues(profile?.tls);
  const mode = h(
    "select",
    { class: "input" },
    h("option", { value: "" }, "Driver default"),
    ...support.modes.map((id) => h("option", { value: id }, TLS_MODE_LABELS[id])),
  );
  mode.value = support.modes.includes(initial.mode as TlsModeId) ? initial.mode : "";
  const file = (value: string, placeholder: string) => textInput(value, { class: "input mono", placeholder });
  const caFile = file(initial.caFile, "ca.pem");
  const clientCert = file(initial.clientCertFile, "client-cert.pem");
  const clientKey = file(initial.clientKeyFile, "client-key.pem");
  const keyPassphrase = textInput("", { type: "password", placeholder: "Only for an encrypted key; empty keeps the saved one" });
  const serverName = textInput(initial.serverName, { placeholder: "Defaults to the host" });
  const pin = textInput(initial.pinnedSha256, { class: "input mono", placeholder: "SHA-256 fingerprint, hex" });
  const accept = checkbox("Accept invalid certificates", initial.acceptInvalidCerts);
  accept.input.dataset.tls = "acceptInvalidCerts";
  const certificates = h(
    "div",
    { class: "form-grid" },
    support.ca ? field("CA certificate file", caFile, "PEM file of the authority that signed the server certificate.") : null,
    support.clientCert ? h("div", { class: "field-row even" }, field("Client certificate", clientCert), field("Client key", clientKey)) : null,
    support.clientCert ? field("Client key passphrase", keyPassphrase) : null,
    support.serverName ? field("Server name (SNI)", serverName) : null,
    support.pin ? field("Pinned certificate", pin, "Accepts only the certificate with this fingerprint.") : null,
    h("div", { class: "field" }, accept.element, h("span", { class: "field-hint" }, ACCEPT_HINT)),
  );
  const sync = () => {
    certificates.hidden = mode.value === "disable";
  };
  /** Settings on "Driver default" switch the mode to one that encrypts, so the saved mode is visible. */
  const promote = () => {
    if (mode.value === "") mode.value = fallbackMode(support);
    sync();
  };
  mode.addEventListener("change", sync);
  accept.input.addEventListener("change", () => {
    accept.element.classList.remove("attention");
    if (accept.input.checked) promote();
  });
  for (const input of [caFile, clientCert, clientKey, serverName, pin]) input.addEventListener("change", () => input.value.trim() && promote());
  sync();

  const summary = h("summary", {}, "TLS / SSL");
  const element = h("details", { class: "conn-section" }, summary, h("div", { class: "form-grid conn-section-body" }, field("Mode", mode), certificates));
  if (profile?.tls) element.open = true;
  const read = (): TlsConfig | null =>
    tlsFromValues(
      {
        mode: mode.value as TlsModeId | "",
        caFile: caFile.value,
        clientCertFile: clientCert.value,
        clientKeyFile: clientKey.value,
        serverName: serverName.value,
        pinnedSha256: pin.value,
        acceptInvalidCerts: accept.input.checked,
      },
      support,
    );
  return {
    element,
    read,
    secrets: () => (support.clientCert && keyPassphrase.value ? { tlsKeyPassphrase: keyPassphrase.value } : {}),
    pointAtAcceptInvalid: () => {
      if (accept.input.checked) return false;
      element.open = true;
      if (mode.value === "disable" || mode.value === "") mode.value = fallbackMode(support);
      sync();
      accept.element.classList.add("attention");
      accept.input.focus();
      accept.element.scrollIntoView({ block: "nearest" });
      return true;
    },
  };
}

const ACCEPT_HINT = "Encrypts without verifying the server certificate; self-signed or test servers only.";
