/**
 * The pure part of the connection dialog: which inputs an engine's descriptor asks for (`formLayout`, `authInputs`), the
 * form values (`ConnectionForm`) and their mapping to and from a saved profile and a parsed connection string. No DOM
 * here, so it is unit-tested; `connectionDialog.ts` renders it.
 *
 * Everything is read from the `EngineDescriptor` (`connection` form): host/port/user per `usesHost`, the file per
 * `usesFile`, the database per `databaseLabel` (none when empty), extra `fields` keyed `database` / `file` / `url` /
 * `options.<name>` (password-kind fields are secrets with their slot), auth methods per `auth`. No engine id appears.
 */
import type { AuthMethodId, ConnectionSpec, EngineDescriptor, FieldSpec, SecretSlot, SshConfig, TlsConfig } from "../../core/contract";
import type { ConnectionProfile } from "../../core/types";

/** The values the dialog edits. Numbers stay text while typed; `password` and `secrets` are never stored in the profile. */
export interface ConnectionForm {
  engine: string;
  name: string;
  host: string;
  port: string;
  user: string;
  database: string;
  file: string;
  /** A connection string without its password (from the URL tab or a `url` field). */
  url: string;
  /** `options.<name>` fields, by name; checkboxes as "true" / "false". */
  options: Record<string, string>;
  auth: AuthMethodId;
  /** The typed password; empty keeps the saved one. */
  password: string;
  /** Other typed secrets (API key, token, password-kind fields), by slot; empty keeps the saved ones. */
  secrets: Partial<Record<SecretSlot, string>>;
  savePassword: boolean;
}

/** Where a descriptor field's value lives. */
export type FieldTarget =
  { kind: "database" } | { kind: "file" } | { kind: "url" } | { kind: "option"; name: string } | { kind: "secret"; slot: SecretSlot } | { kind: "unknown" };

export interface FormLayout {
  /** Host and port inputs. */
  host: boolean;
  /** The database input (from a `database` field, else built from `databaseLabel` / `databaseHint`); null when none. */
  database: FieldSpec | null;
  /** The file input (from a `file` field, else a default one when `usesFile`); null when none. */
  file: FieldSpec | null;
  /** The other fields per section, in descriptor order. */
  general: FieldSpec[];
  auth: FieldSpec[];
  advanced: FieldSpec[];
  /** Whether the URL tab is offered (the engine parses connection strings). */
  urlTab: boolean;
}

/** What the auth part of the form shows for one auth method. */
export interface AuthInputs {
  user: boolean;
  password: boolean;
  /** A secret other than the password (API key, bearer token). */
  secret: { slot: SecretSlot; label: string } | null;
}

export const AUTH_LABELS: Record<AuthMethodId, string> = {
  password: "User and password",
  aclUser: "ACL user and password",
  none: "No authentication",
  apiKey: "API key",
  bearer: "Bearer token",
};

/** The option key the pre-v2 dialog stored its tab in; dropped on save. */
const LEGACY_MODE_OPTION = "connectionMode";

export function fieldTarget(field: FieldSpec): FieldTarget {
  if (field.kind.type === "password") return { kind: "secret", slot: field.kind.slot };
  if (field.key === "database" || field.key === "file" || field.key === "url") return { kind: field.key };
  if (field.key.startsWith("options.")) return { kind: "option", name: field.key.slice("options.".length) };
  return { kind: "unknown" };
}

function syntheticField(key: string, label: string, kind: FieldSpec["kind"], hint: string | null, placeholder: string | null): FieldSpec {
  return { key, kind, label, placeholder, hint, section: "general", default: null, required: false };
}

export function formLayout(descriptor: EngineDescriptor): FormLayout {
  const form = descriptor.connection;
  const byKey = (key: string) => form.fields.find((field) => field.key === key && field.kind.type !== "password") ?? null;
  const databaseField = byKey("database");
  const fileField = byKey("file");
  const database =
    databaseField ?? (form.databaseLabel.trim() ? syntheticField("database", form.databaseLabel, { type: "text" }, null, form.databaseHint || null) : null);
  const file = fileField ?? (form.usesFile ? syntheticField("file", "Database file", { type: "file", filters: [], save: false }, null, null) : null);
  const rest = form.fields.filter((field) => field !== databaseField && field !== fileField && fieldTarget(field).kind !== "unknown");
  return {
    host: form.usesHost,
    database,
    file,
    general: rest.filter((field) => field.section === "general"),
    auth: rest.filter((field) => field.section === "auth"),
    advanced: rest.filter((field) => field.section === "advanced"),
    urlTab: form.url !== null,
  };
}

/**
 * The inputs of an auth method. "password" asks for a user too, except on engines that also offer "aclUser": there
 * "password" is a password alone (Redis `requirepass`) and "aclUser" adds the user.
 */
export function authInputs(descriptor: EngineDescriptor, method: AuthMethodId): AuthInputs {
  switch (method) {
    case "password":
      return { user: !descriptor.connection.auth.includes("aclUser"), password: true, secret: null };
    case "aclUser":
      return { user: true, password: true, secret: null };
    case "apiKey":
      return { user: false, password: false, secret: { slot: "apiKey", label: "API key" } };
    case "bearer":
      return { user: false, password: false, secret: { slot: "token", label: "Bearer token" } };
    case "none":
      return { user: false, password: false, secret: null };
  }
}

/** The method a form starts on: the profile's when the engine offers it, else the engine's first. */
export function initialAuth(descriptor: EngineDescriptor, saved: AuthMethodId | null | undefined): AuthMethodId {
  const offered = descriptor.connection.auth;
  return saved && offered.includes(saved) ? saved : (offered[0] ?? "password");
}

function optionDefaults(descriptor: EngineDescriptor): Record<string, string> {
  const result: Record<string, string> = {};
  for (const field of descriptor.connection.fields) {
    const target = fieldTarget(field);
    if (target.kind === "option" && field.default !== null) result[target.name] = field.default;
  }
  return result;
}

/** A new connection on `descriptor`, with its default port, user and option defaults. */
export function newForm(descriptor: EngineDescriptor, defaultHost: string): ConnectionForm {
  const form = descriptor.connection;
  return {
    engine: descriptor.id,
    name: "",
    host: form.usesHost ? defaultHost : "",
    port: form.defaultPort === null ? "" : String(form.defaultPort),
    user: form.defaultUser,
    database: "",
    file: "",
    url: "",
    options: optionDefaults(descriptor),
    auth: initialAuth(descriptor, null),
    password: "",
    secrets: {},
    savePassword: true,
  };
}

/** The form for a saved profile (`descriptor` is the profile's engine, possibly an unknown-engine placeholder). */
export function formFromProfile(profile: ConnectionProfile, descriptor: EngineDescriptor): ConnectionForm {
  return {
    engine: profile.driver,
    name: profile.name,
    host: profile.host,
    port: profile.port ? String(profile.port) : descriptor.connection.defaultPort === null ? "" : String(descriptor.connection.defaultPort),
    user: profile.user,
    database: profile.database ?? "",
    file: profile.file ?? "",
    url: profile.url ?? "",
    options: { ...optionDefaults(descriptor), ...profile.options },
    auth: initialAuth(descriptor, profile.auth),
    password: "",
    secrets: {},
    savePassword: profile.savePassword,
  };
}

/** Moves a form to another engine: port and user follow the engine when they were the previous engine's defaults. */
export function switchEngine(form: ConnectionForm, from: EngineDescriptor, to: EngineDescriptor, defaultHost: string): ConnectionForm {
  const fromPort = from.connection.defaultPort === null ? "" : String(from.connection.defaultPort);
  const toPort = to.connection.defaultPort === null ? "" : String(to.connection.defaultPort);
  return {
    ...form,
    engine: to.id,
    host: to.connection.usesHost && form.host.trim() === "" ? defaultHost : form.host,
    port: form.port.trim() === "" || form.port.trim() === fromPort ? toPort : form.port,
    user: form.user.trim() === "" || form.user.trim() === from.connection.defaultUser ? to.connection.defaultUser : form.user,
    // A connection string belongs to one engine.
    url: "",
    options: { ...optionDefaults(to), ...form.options },
    auth: initialAuth(to, form.auth),
  };
}

/**
 * Fills the form from a parsed connection string (`parse_connection_string`): host, port, user, database, file, url,
 * options and auth from the spec; the string's password goes into `password` (never into options or url). A string
 * without a password keeps the typed one. TLS and SSH are returned by the spec for the sections.
 */
export function applySpec(form: ConnectionForm, spec: ConnectionSpec, password: string | null, descriptor: EngineDescriptor): ConnectionForm {
  const defaultPort = descriptor.connection.defaultPort;
  return {
    ...form,
    host: spec.host,
    port: spec.port ? String(spec.port) : defaultPort === null ? "" : String(defaultPort),
    user: spec.user,
    database: spec.database ?? "",
    file: spec.file ?? "",
    url: spec.url ?? "",
    options: { ...form.options, ...spec.options },
    auth: spec.auth && descriptor.connection.auth.includes(spec.auth) ? spec.auth : form.auth,
    password: password ?? form.password,
  };
}

function basename(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** The name a connection gets when the name field is empty. */
export function defaultName(form: ConnectionForm, descriptor: EngineDescriptor): string {
  const layout = formLayout(descriptor);
  if (layout.file && form.file.trim()) return basename(form.file.trim());
  if (form.url.trim() && form.host.trim()) return form.host.trim();
  if (layout.host && form.host.trim()) return form.port.trim() ? `${form.host.trim()}:${form.port.trim()}` : form.host.trim();
  return descriptor.label;
}

/** The option keys the form owns for `descriptor`. */
function formOptionKeys(descriptor: EngineDescriptor): Set<string> {
  const keys = new Set<string>();
  for (const field of descriptor.connection.fields) {
    const target = fieldTarget(field);
    if (target.kind === "option") keys.add(target.name);
  }
  return keys;
}

/**
 * The version-2 profile for the form. `base` is the profile being edited: options the form does not show (and its
 * safety rules) are kept when the engine is unchanged. `tls` / `ssh` come from the sections.
 */
export function profileFromForm(
  id: string,
  form: ConnectionForm,
  descriptor: EngineDescriptor,
  tls: TlsConfig | null,
  ssh: SshConfig | null,
  base: ConnectionProfile | null = null,
): ConnectionProfile {
  const layout = formLayout(descriptor);
  const inputs = authInputs(descriptor, form.auth);
  const owned = formOptionKeys(descriptor);
  const options: Record<string, string> = {};
  if (base && base.driver === descriptor.id) {
    for (const [key, value] of Object.entries(base.options)) if (!owned.has(key) && key !== LEGACY_MODE_OPTION) options[key] = value;
  }
  for (const field of descriptor.connection.fields) {
    const target = fieldTarget(field);
    if (target.kind !== "option") continue;
    const value = (form.options[target.name] ?? "").trim();
    // Checkboxes are always stored so an unticked default-on option stays off.
    if (field.kind.type === "checkbox") options[target.name] = value === "true" ? "true" : "false";
    else if (value !== "") options[target.name] = value;
  }
  const port = Number(form.port);
  const profile: ConnectionProfile = {
    id,
    name: form.name.trim() || defaultName(form, descriptor),
    driver: descriptor.id,
    host: layout.host ? form.host.trim() : "",
    port: layout.host ? (Number.isInteger(port) && port > 0 ? port : (descriptor.connection.defaultPort ?? 0)) : 0,
    user: inputs.user ? form.user.trim() : "",
    database: layout.database ? form.database.trim() || null : null,
    savePassword: form.savePassword,
    options,
  };
  if (layout.file && form.file.trim()) profile.file = form.file.trim();
  if (form.url.trim()) profile.url = form.url.trim();
  // The engine's first method is its default: stored only when chosen otherwise, or when the profile already names one.
  if (descriptor.connection.auth.length > 1 && (form.auth !== descriptor.connection.auth[0] || base?.auth)) profile.auth = form.auth;
  if (tls) profile.tls = tls;
  if (ssh) profile.ssh = ssh;
  if (base?.safety) profile.safety = base.safety;
  return profile;
}

/** The secrets of a save or test: `password` for the password slot (null keeps the saved one), the other slots typed. */
export interface FormSecrets {
  password: string | null;
  secrets: Partial<Record<SecretSlot, string>>;
}

/**
 * The secrets the form sends. An empty password keeps the saved one when the profile saves passwords; a method without a
 * password sends "" so the backend does not ask for one. `extra` are the sections' secrets (SSH, TLS key passphrase).
 */
export function formSecrets(
  form: ConnectionForm,
  descriptor: EngineDescriptor,
  savedPassword: boolean,
  extra: Partial<Record<SecretSlot, string>> = {},
): FormSecrets {
  const inputs = authInputs(descriptor, form.auth);
  const password = inputs.password ? (form.password === "" && savedPassword ? null : form.password) : "";
  const used = new Set<SecretSlot>();
  if (inputs.secret) used.add(inputs.secret.slot);
  for (const field of descriptor.connection.fields) if (field.kind.type === "password") used.add(field.kind.slot);
  const secrets: Partial<Record<SecretSlot, string>> = {};
  for (const [slot, value] of Object.entries(form.secrets) as [SecretSlot, string][]) {
    if (used.has(slot) && value !== "" && slot !== "password") secrets[slot] = value;
  }
  for (const [slot, value] of Object.entries(extra) as [SecretSlot, string][]) if (value) secrets[slot] = value;
  return { password, secrets };
}

const trimmed = (value: string | null | undefined) => (value ?? "").trim();

/** The SSH settings that decide where secrets go, as a comparable string; "" without a tunnel. */
function sshRoute(ssh: SshConfig | null | undefined): string {
  if (!ssh?.enabled) return "";
  const auth = ssh.auth.method === "privateKey" ? { method: "privateKey", keyFile: ssh.auth.keyFile.trim() } : { method: ssh.auth.method };
  const hops = ssh.jumpHosts.map((hop) => ({ host: hop.host.trim(), port: hop.port, user: hop.user.trim() }));
  return JSON.stringify([ssh.host.trim(), ssh.port, ssh.user.trim(), auth, hops, ssh.hostKey]);
}

/** The TLS settings as a comparable string; none compare as the default ("prefer", nothing else). */
function tlsChecks(tls: TlsConfig | null | undefined): string {
  return JSON.stringify([
    tls?.mode ?? "prefer",
    trimmed(tls?.caFile),
    trimmed(tls?.clientCertFile),
    trimmed(tls?.clientKeyFile),
    trimmed(tls?.serverName),
    tls?.acceptInvalidCerts ?? false,
    trimmed(tls?.pinnedSha256),
  ]);
}

/**
 * Whether `draft` still points at the `saved` profile's server, SSH route, command and TLS checks, like the
 * backend's `ConnectionProfile::same_target`. When it does not, saving without typing the password again forgets the
 * saved secrets.
 */
export function sameTarget(saved: ConnectionProfile, draft: ConnectionProfile): boolean {
  return (
    saved.driver === draft.driver &&
    trimmed(saved.host) === trimmed(draft.host) &&
    saved.port === draft.port &&
    trimmed(saved.user) === trimmed(draft.user) &&
    trimmed(saved.url) === trimmed(draft.url) &&
    trimmed(saved.file) === trimmed(draft.file) &&
    sshRoute(saved.ssh) === sshRoute(draft.ssh) &&
    tlsChecks(saved.tls) === tlsChecks(draft.tls) &&
    trimmed(saved.commandId) === trimmed(draft.commandId)
  );
}

/** `profile` using the workspace command `commandId` (null: none). */
export function withCommand(profile: ConnectionProfile, commandId: string | null): ConnectionProfile {
  const next = { ...profile };
  delete next.commandId;
  return commandId ? { ...next, commandId } : next;
}
