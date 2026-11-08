/**
 * The SSH tunnel section of the connection dialog (shown when the descriptor's `connection.ssh` is true): on/off, host,
 * port, user, auth (password / private key with its key file / agent), the host-key policy, and the SSH secrets. SSH
 * tunnels themselves come in a later build: the settings are saved, and connecting with SSH on fails with
 * `SSH_NOT_AVAILABLE` (the section says so). Jump hosts and keepalive are kept from the profile, not edited yet.
 *
 * API (kept small; the SSH/TLS work owns this file):
 *
 *     const ssh = renderSshSection(profile, descriptor);
 *     form.append(ssh.element);          // a collapsible section; empty and hidden when the engine has no SSH
 *     profile.ssh = ssh.read();          // SshConfig | null
 *     secrets = { ...ssh.secrets() };    // sshPassword or sshKeyPassphrase, when typed
 *
 * `read()` returns null when the engine has no SSH, or when the profile had no SSH settings, the tunnel stays off and
 * no host was typed; otherwise the settings are kept, with `enabled: false` while the tunnel is off.
 */
import type { EngineDescriptor, SecretSlot, SshAuth, SshConfig } from "../../core/contract";
import { checkbox, field, textInput } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { ConnectionProfile } from "../../core/types";

export interface SshSection {
  element: HTMLElement;
  read(): SshConfig | null;
  secrets(): Partial<Record<SecretSlot, string>>;
}

export const SSH_NOTE =
  "SSH tunnels are available in a later build: the settings are saved, but connecting with the tunnel on fails with " +
  "SSH_NOT_AVAILABLE. Until then, open the tunnel yourself (ssh -L 13306:db:3306 bastion) and connect to its local port.";

export type SshAuthMethod = SshAuth["method"];

export const SSH_AUTH_LABELS: Record<SshAuthMethod, string> = {
  password: "Password",
  privateKey: "Private key",
  agent: "SSH agent",
};

export const HOST_KEY_LABELS: Record<SshConfig["hostKey"], string> = {
  acceptNew: "Accept new hosts, refuse changed keys",
  strict: "Known hosts only",
  off: "Do not check (unsafe)",
};

/** What the inputs hold. */
export interface SshValues {
  enabled: boolean;
  host: string;
  port: string;
  user: string;
  method: SshAuthMethod;
  keyFile: string;
  hostKey: SshConfig["hostKey"];
}

export function sshValues(ssh: SshConfig | null | undefined): SshValues {
  return {
    enabled: ssh?.enabled ?? false,
    host: ssh?.host ?? "",
    port: String(ssh?.port ?? 22),
    user: ssh?.user ?? "",
    method: ssh?.auth.method ?? "privateKey",
    keyFile: ssh?.auth.method === "privateKey" ? ssh.auth.keyFile : "",
    hostKey: ssh?.hostKey ?? "acceptNew",
  };
}

/** The config for the inputs; `saved` keeps what the section does not edit (jump hosts, keepalive). */
export function sshFromValues(values: SshValues, saved: SshConfig | null | undefined): SshConfig | null {
  if (!values.enabled && !saved && !values.host.trim()) return null;
  const port = Number(values.port);
  const auth: SshAuth = values.method === "privateKey" ? { method: "privateKey", keyFile: values.keyFile.trim() } : { method: values.method };
  return {
    enabled: values.enabled,
    host: values.host.trim(),
    port: Number.isInteger(port) && port > 0 && port < 65536 ? port : 22,
    user: values.user.trim(),
    auth,
    jumpHosts: saved?.jumpHosts ?? [],
    hostKey: values.hostKey,
    keepaliveSecs: saved?.keepaliveSecs ?? null,
  };
}

/** The secret slot the auth method uses. */
export function sshSecretSlot(method: SshAuthMethod): SecretSlot {
  return method === "password" ? "sshPassword" : "sshKeyPassphrase";
}

export function renderSshSection(profile: Pick<ConnectionProfile, "ssh"> | null, descriptor: EngineDescriptor): SshSection {
  if (!descriptor.connection.ssh) {
    const element = h("details", { class: "conn-section", hidden: true });
    return { element, read: () => profile?.ssh ?? null, secrets: () => ({}) };
  }
  const saved = profile?.ssh ?? null;
  const initial = sshValues(saved);
  const enabled = checkbox("Connect through an SSH tunnel", initial.enabled);
  const host = textInput(initial.host, { placeholder: "bastion.example.com" });
  const port = textInput(initial.port, { type: "number", min: 1, max: 65535 });
  const user = textInput(initial.user, { placeholder: "e.g. ubuntu" });
  const method = h("select", { class: "input" }, ...Object.entries(SSH_AUTH_LABELS).map(([id, label]) => h("option", { value: id }, label)));
  method.value = initial.method;
  const keyFile = textInput(initial.keyFile, { class: "input mono", placeholder: "~/.ssh/id_ed25519" });
  const secret = textInput("", { type: "password", placeholder: "Empty keeps the saved one" });
  const hostKey = h("select", { class: "input" }, ...Object.entries(HOST_KEY_LABELS).map(([id, label]) => h("option", { value: id }, label)));
  hostKey.value = initial.hostKey;
  const keyField = field("Private key file", keyFile);
  const secretField = field("SSH password", secret);
  const details = h(
    "div",
    { class: "form-grid" },
    h("div", { class: "field-row" }, field("SSH host", host), h("div", { class: "narrow" }, field("Port", port))),
    h("div", { class: "field-row even" }, field("SSH user", user), field("Authentication", method)),
    keyField,
    secretField,
    field("Host key", hostKey),
  );
  const sync = () => {
    details.hidden = !enabled.input.checked;
    keyField.hidden = method.value !== "privateKey";
    secretField.hidden = method.value === "agent";
    secretField.querySelector(".field-label")!.textContent = method.value === "password" ? "SSH password" : "Key passphrase";
  };
  enabled.input.addEventListener("change", sync);
  method.addEventListener("change", sync);
  sync();
  const element = h(
    "details",
    { class: "conn-section" },
    h("summary", {}, "SSH tunnel"),
    h("div", { class: "form-grid conn-section-body" }, h("p", { class: "driver-note" }, SSH_NOTE), enabled.element, details),
  );
  if (initial.enabled) element.open = true;
  return {
    element,
    read: () =>
      sshFromValues(
        {
          enabled: enabled.input.checked,
          host: host.value,
          port: port.value,
          user: user.value,
          method: method.value as SshAuthMethod,
          keyFile: keyFile.value,
          hostKey: hostKey.value as SshConfig["hostKey"],
        },
        saved,
      ),
    secrets: () => {
      const current = method.value as SshAuthMethod;
      return enabled.input.checked && current !== "agent" && secret.value ? { [sshSecretSlot(current)]: secret.value } : {};
    },
  };
}
