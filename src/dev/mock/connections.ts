/**
 * Saved connections of the mock backend: `save_connection` / `delete_connection` on the mock config, which profiles have
 * a saved password, the secret binding of `test_connection` (`PASSWORD_REQUIRED` when the target changed and no password
 * was typed), and `parse_connection_string` modelled on the Rust parsers (`src-tauri/src/engines/<id>/url.rs`):
 * MariaDB/MySQL URLs, SQL Server ADO.NET / JDBC / URL strings, MongoDB (the string minus its password goes to `url`),
 * and plain URLs for the planned engines.
 */
import type { ConnectionSpec, EngineDescriptor, SecretSlot, TlsConfig, TlsMode } from "../../core/contract";
import type { AppConfig, ConnectionProfile } from "../../core/types";
import { sameTarget } from "../../features/connections/connectionForm";
import { PRODUCTION_ICON, relaxations, safetyOf } from "../../features/safety/safety";
import { mockError } from "./errors";
import { state } from "./state";

/** Profile ids with a saved password (the seeded profiles that save one). */
const savedPasswords = new Set(
  state.config.workspaces.flatMap((workspace) => workspace.connections.filter((profile) => profile.savePassword).map((profile) => profile.id)),
);

export function profiles(): ConnectionProfile[] {
  return state.config.workspaces.flatMap((workspace) => workspace.connections);
}

/**
 * Like the backend's `save_connection`: replaces the profile where it is, or adds it to the workspace. A profile that
 * points at another server, command or TLS setting loses its saved password unless one is typed. Making the rules of a
 * Production workspace's connection looser (an override, or a move out) needs the workspace name.
 */
export function saveConnection(
  profile: ConnectionProfile,
  password: string | null,
  workspaceId: string | null,
  secrets: Partial<Record<SecretSlot, string>> | null,
  relaxConfirmation: string | null = null,
): AppConfig {
  checkConnectionRelax(profile, workspaceId, relaxConfirmation);
  const saved = profiles().find((item) => item.id === profile.id);
  if (saved && !sameTarget(saved, profile)) savedPasswords.delete(profile.id);
  if (!profile.savePassword) savedPasswords.delete(profile.id);
  else if (password !== null || secrets?.password !== undefined) savedPasswords.add(profile.id);
  const target = workspaceId ?? state.config.workspaces.find((workspace) => workspace.connections.some((item) => item.id === profile.id))?.id;
  const workspaces = state.config.workspaces.map((workspace) => {
    const index = workspace.connections.findIndex((item) => item.id === profile.id);
    if (index >= 0) {
      const connections = [...workspace.connections];
      connections[index] = profile;
      return { ...workspace, connections };
    }
    if (workspace.id === (target ?? state.config.activeWorkspace)) return { ...workspace, connections: [...workspace.connections, profile] };
    return workspace;
  });
  state.config = { ...state.config, workspaces };
  return state.config;
}

/** `RELAX_CONFIRMATION_REQUIRED` unless the save keeps a Production connection's rules (or the name is typed). */
function checkConnectionRelax(profile: ConnectionProfile, workspaceId: string | null, typed: string | null): void {
  const workspaces = state.config.workspaces;
  const source = workspaces.find((workspace) => workspace.connections.some((item) => item.id === profile.id));
  const saved = source?.connections.find((item) => item.id === profile.id);
  const target = workspaces.find((workspace) => workspace.id === (workspaceId ?? source?.id ?? state.config.activeWorkspace));
  if (!target) return;
  const after = profile.safety ?? saved?.safety ?? safetyOf(target);
  const changes: string[] = [];
  let production: string | null = null;
  if (source && saved) {
    const looser = relaxations(saved.safety ?? safetyOf(source), after);
    changes.push(...looser.map((change) => change.text));
    if (looser.length > 0 && source.icon === PRODUCTION_ICON) production = source.name;
  }
  if (source?.id !== target.id) {
    const looser = relaxations(safetyOf(target), after);
    changes.push(...looser.map((change) => change.text).filter((text) => !changes.includes(text)));
    if (looser.length > 0 && production === null && target.icon === PRODUCTION_ICON) production = target.name;
  }
  if (production !== null && (typed ?? "").trim() !== production.trim())
    throw mockError(
      "RELAX_CONFIRMATION_REQUIRED",
      `This makes the rules of a connection in the Production workspace less strict: type "${production}" to confirm.`,
      {
        expected: production,
        changes,
      },
    );
}

export function deleteConnection(id: string): AppConfig {
  savedPasswords.delete(id);
  state.config = {
    ...state.config,
    workspaces: state.config.workspaces.map((workspace) => ({ ...workspace, connections: workspace.connections.filter((item) => item.id !== id) })),
  };
  return state.config;
}

/** The backend's secret binding: without a typed password, the saved one is used only for the saved server. */
export function checkTestPassword(profile: ConnectionProfile, password: string | null): void {
  if (password === "wrong")
    throw mockError("AUTH_FAILED", `Access denied for user '${profile.user}'@'192.168.1.20' (using password: YES)`, {
      engine: profile.driver,
      vendorCode: 1045,
      sqlstate: "28000",
      hint: 'Check the user name and password (the mock refuses the password "wrong").',
    });
  if (password !== null) return;
  const saved = profiles().find((item) => item.id === profile.id);
  if (!saved || !sameTarget(saved, profile) || !savedPasswords.has(profile.id)) throw mockError("PASSWORD_REQUIRED", "A password is required");
}

// --- Connection strings ---------------------------------------------------------------------------------------------------

function baseSpec(descriptor: EngineDescriptor): ConnectionSpec {
  return {
    engine: descriptor.id,
    host: "",
    port: descriptor.connection.defaultPort ?? 0,
    user: "",
    database: null,
    file: null,
    url: null,
    options: {},
    tls: null,
    ssh: null,
    auth: null,
  };
}

function tlsConfig(mode: TlsMode): TlsConfig {
  return { mode, caFile: null, clientCertFile: null, clientKeyFile: null, serverName: null, acceptInvalidCerts: false, pinnedSha256: null };
}

const flag = (value: string) => ["true", "1", "yes"].includes(value.toLowerCase());

/** `scheme://user:password@hosts/path?query`, split without URL normalisation (host lists, `+srv`). */
function splitUrl(text: string) {
  const match = /^([\w+.-]+):\/\/([^/?#]*)([^?#]*)(?:\?([^#]*))?/.exec(text);
  if (!match) return null;
  const [, scheme, authority, path, query = ""] = match;
  const at = authority.lastIndexOf("@");
  const credentials = at >= 0 ? authority.slice(0, at) : null;
  const hosts = at >= 0 ? authority.slice(at + 1) : authority;
  const colon = credentials?.indexOf(":") ?? -1;
  const rawUser = credentials === null ? "" : colon >= 0 ? credentials.slice(0, colon) : credentials;
  const password = credentials !== null && colon >= 0 ? decodeURIComponent(credentials.slice(colon + 1)) : null;
  const params = query
    .split("&")
    .filter(Boolean)
    .map((pair) => {
      const index = pair.indexOf("=");
      return index < 0 ? [pair, ""] : [pair.slice(0, index), decodeURIComponent(pair.slice(index + 1))];
    });
  return { scheme: scheme.toLowerCase(), rawUser, user: decodeURIComponent(rawUser), password, hosts, path: path.replace(/^\//, ""), query, params };
}

function hostPort(hosts: string, defaultPort: number): { host: string; port: number } {
  const first = hosts.split(",")[0].trim();
  const bracket = /^\[([^\]]+)\](?::(\d+))?$/.exec(first);
  if (bracket) return { host: bracket[1], port: Number(bracket[2] ?? defaultPort) };
  const match = /^(.*):(\d+)$/.exec(first);
  if (!match) return { host: first, port: defaultPort };
  const port = Number(match[2]);
  if (port < 1 || port > 65535) throw mockError("ERROR", `'${match[2]}' is not a valid port`);
  return { host: match[1], port };
}

function mysqlTlsMode(value: string): TlsMode {
  const key = value.toLowerCase().replace(/[-_]/g, "");
  if (["disabled", "disable", "false", "0", "no"].includes(key)) return "disable";
  if (["preferred", "prefer", "trust"].includes(key)) return "prefer";
  if (["required", "require", "true", "1", "yes"].includes(key)) return "require";
  if (key === "verifyca") return "verifyCa";
  if (key === "verifyidentity" || key === "verifyfull") return "verifyFull";
  throw mockError("ERROR", `Unknown TLS mode '${value}'`);
}

function parseMysql(descriptor: EngineDescriptor, text: string) {
  const url = splitUrl(text.replace(/^jdbc:/i, ""));
  if (!url || !["mysql", "mariadb"].includes(url.scheme) || !url.hosts) throw mockError("ERROR", "Expected mysql://user:password@host:port/database");
  const spec = { ...baseSpec(descriptor), ...hostPort(url.hosts, 3306), user: url.user, database: decodeURIComponent(url.path.replace(/\/$/, "")) || null };
  let password = url.password;
  for (const [rawKey, value] of url.params) {
    const key = rawKey.toLowerCase().replace(/[-_]/g, "");
    if ((key === "database" || key === "dbname") && !spec.database && value) spec.database = value;
    else if ((key === "user" || key === "username") && !spec.user) spec.user = value;
    else if (key === "password" && password === null) password = value;
    else if (["sslmode", "usessl", "ssl", "tls", "requiressl"].includes(key)) spec.tls = { ...(spec.tls ?? tlsConfig("prefer")), mode: mysqlTlsMode(value) };
    else if (["sslca", "sslrootcert", "serversslcert"].includes(key)) spec.tls = { ...(spec.tls ?? tlsConfig("prefer")), caFile: value.replace(/^file:/, "") };
    else if (key === "sslcert") spec.tls = { ...(spec.tls ?? tlsConfig("prefer")), clientCertFile: value };
    else if (key === "sslkey") spec.tls = { ...(spec.tls ?? tlsConfig("prefer")), clientKeyFile: value };
  }
  return { spec, password };
}

function parseMssql(descriptor: EngineDescriptor, text: string) {
  const value = text.replace(/^jdbc:/i, "");
  const fields: Record<string, string> = {};
  const set = (key: string, item: string) => {
    const name = key.trim().toLowerCase();
    const field = ["server", "data source", "address", "addr", "network address", "servername"].includes(name)
      ? "server"
      : ["database", "initial catalog", "databasename"].includes(name)
        ? "database"
        : ["user id", "uid", "user", "username"].includes(name)
          ? "user"
          : ["password", "pwd"].includes(name)
            ? "password"
            : name === "encrypt"
              ? "encrypt"
              : ["trustservercertificate", "trust server certificate"].includes(name)
                ? "trust"
                : ["port", "portnumber"].includes(name)
                  ? "port"
                  : null;
    if (field) fields[field] = item.trim().replace(/^\{(.*)\}$/, "$1");
  };
  const scheme = /^(sqlserver|mssql):\/\//i.exec(value);
  if (scheme) {
    const [main, ...pairs] = value.slice(scheme[0].length).split(";");
    const url = splitUrl(`x://${main}`);
    if (url) {
      fields.server = url.hosts;
      if (url.user) fields.user = url.user;
      if (url.password !== null) fields.password = url.password;
      if (url.path) fields.database = decodeURIComponent(url.path);
      for (const [key, item] of url.params) set(key, item);
    }
    for (const pair of pairs) {
      const index = pair.indexOf("=");
      if (index > 0) set(pair.slice(0, index), pair.slice(index + 1));
    }
  } else {
    for (const pair of value.split(";")) {
      const index = pair.indexOf("=");
      if (index > 0) set(pair.slice(0, index), pair.slice(index + 1));
    }
  }
  const server = (fields.server ?? "").replace(/^tcp:/i, "").split("\\")[0];
  if (!server.trim()) throw mockError("ERROR", "Expected Server=host,port;Database=…;User Id=…;Password=…");
  const [host, port] = server.includes(",") ? server.split(",") : [hostPort(server, 1433).host, String(hostPort(server, 1433).port)];
  const encrypt = !["false", "no", "optional", "0"].includes((fields.encrypt ?? "").toLowerCase());
  const tls = { ...tlsConfig(encrypt ? "require" : "disable"), acceptInvalidCerts: flag(fields.trust ?? "") };
  return {
    spec: {
      ...baseSpec(descriptor),
      host: host.trim(),
      port: Number(fields.port ?? port) || 1433,
      user: fields.user ?? "",
      database: fields.database || null,
      tls,
    },
    password: fields.password ?? null,
  };
}

function parseMongo(descriptor: EngineDescriptor, text: string) {
  const url = splitUrl(text);
  if (!url || !["mongodb", "mongodb+srv"].includes(url.scheme) || !url.hosts) throw mockError("ERROR", "Expected mongodb://… or mongodb+srv://…");
  const srv = url.scheme === "mongodb+srv";
  const { host, port } = srv ? { host: url.hosts, port: 27017 } : hostPort(url.hosts, 27017);
  let authSource: string | null = null;
  let tls: TlsConfig | null = null;
  for (const [rawKey, value] of url.params) {
    const key = rawKey.toLowerCase();
    if (key === "authsource" && value) authSource = value;
    else if (key === "tls" || key === "ssl") tls = { ...(tls ?? tlsConfig("require")), mode: flag(value) ? "require" : "disable" };
    else if (key === "tlscafile") tls = { ...(tls ?? tlsConfig("require")), caFile: value };
    else if (key === "tlscertificatekeyfile") tls = { ...(tls ?? tlsConfig("require")), clientCertFile: value };
    else if (key === "tlsallowinvalidcertificates" || key === "tlsinsecure") tls = { ...(tls ?? tlsConfig("require")), acceptInvalidCerts: flag(value) };
  }
  // Keep everything but the password: hosts, replica set, TLS and SRV options all live in the string.
  const user = url.rawUser ? `${url.rawUser}@` : "";
  const path = /^[\w+.-]+:\/\/[^/?#]*(\/[^?#]*)?/.exec(text)?.[1] ?? "";
  const stored = `${url.scheme}://${user}${url.hosts}${path}${url.query ? `?${url.query}` : ""}`;
  return {
    spec: { ...baseSpec(descriptor), host, port, user: url.user, database: authSource ?? (decodeURIComponent(url.path) || null), url: stored, tls },
    password: url.password,
  };
}

/** The planned engines: `scheme://user:password@host:port/database` (a file path for file-based engines). */
function parseGeneric(descriptor: EngineDescriptor, text: string) {
  const schemes = descriptor.connection.url?.schemes ?? [];
  const url = splitUrl(text);
  if (!url || !schemes.includes(url.scheme))
    throw mockError(
      "ERROR",
      `A ${descriptor.label} connection string starts with ${schemes.map((scheme) => `${scheme}://`).join(" or ")}, e.g. ${descriptor.connection.url?.example ?? ""}`,
    );
  if (descriptor.connection.usesFile)
    return { spec: { ...baseSpec(descriptor), file: decodeURIComponent(`/${url.path}`.replace(/^\/([A-Za-z]:)/, "$1")) }, password: null };
  return {
    spec: {
      ...baseSpec(descriptor),
      ...hostPort(url.hosts, descriptor.connection.defaultPort ?? 0),
      user: url.user,
      database: decodeURIComponent(url.path) || null,
    },
    password: url.password,
  };
}

export function parseConnectionString(descriptor: EngineDescriptor, text: string): { spec: ConnectionSpec; password: string | null } {
  const value = text.trim();
  if (!value) throw mockError("ERROR", "Paste a connection string first");
  switch (descriptor.id) {
    case "mariadb":
    case "mysql":
      return parseMysql(descriptor, value);
    case "mssql":
      return parseMssql(descriptor, value);
    case "mongodb":
      return parseMongo(descriptor, value);
    default:
      return parseGeneric(descriptor, value);
  }
}
