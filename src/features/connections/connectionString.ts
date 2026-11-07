/**
 * Connection strings for the "Connection string" tab of the connection dialog.
 * Strings are parsed into the same profile fields the "Parameters" tab edits;
 * the password is returned separately so it goes to Credential Manager and
 * never into db-studio.json. MongoDB keeps its URI (minus the password)
 * because options such as replica sets and SRV records live in it.
 */
import type { DriverId } from "../../core/types";

export interface ParsedConnection {
  host: string;
  port: number;
  user: string;
  /** Null when the string carries no password (keep the saved one). */
  password: string | null;
  database: string | null;
  /** Driver options derived from the string (TLS flags, the Mongo URI). */
  options: Record<string, string>;
}

/** Fields the formatter needs; matches ConnectionProfile minus identity. */
export interface ConnectionFields {
  host: string;
  port: number;
  user: string;
  database: string | null;
  options: Record<string, string>;
}

/** Stored in profile.options so the dialog reopens on the tab the user chose. */
export const MODE_OPTION = "connectionMode";

const DEFAULT_PORTS: Record<DriverId, number> = { mariadb: 3306, mysql: 3306, mssql: 1433, mongodb: 27017 };

export const CONNECTION_STRING_EXAMPLES: Record<DriverId, string> = {
  mariadb: "mysql://root:password@127.0.0.1:3306/shop",
  mysql: "mysql://root:password@127.0.0.1:3306/shop",
  mssql: "Server=127.0.0.1,1433;Database=ERP;User Id=sa;Password=…;Encrypt=True;TrustServerCertificate=True",
  mongodb: "mongodb+srv://user:password@cluster0.example.net/?authSource=admin",
};

export function parseConnectionString(driver: DriverId, text: string): ParsedConnection {
  const value = text.trim();
  if (!value) throw new Error("Paste a connection string first");
  switch (driver) {
    case "mssql":
      return parseSqlServer(value);
    case "mongodb":
      return parseMongo(value);
    default:
      return parseMysql(value, driver);
  }
}

/** The string for the current fields, without a password. */
export function formatConnectionString(driver: DriverId, fields: ConnectionFields): string {
  switch (driver) {
    case "mssql": {
      const parts = [`Server=${fields.host},${fields.port}`];
      if (fields.database) parts.push(`Database=${fields.database}`);
      if (fields.user) parts.push(`User Id=${fields.user}`);
      parts.push(`Encrypt=${fields.options.encrypt === "false" ? "False" : "True"}`);
      if (fields.options.trustServerCertificate === "true") parts.push("TrustServerCertificate=True");
      return parts.join(";");
    }
    case "mongodb": {
      if (fields.options.uri) return fields.options.uri;
      const user = fields.user ? `${encodeURIComponent(fields.user)}@` : "";
      const auth = fields.database ? `?authSource=${encodeURIComponent(fields.database)}` : "";
      return `mongodb://${user}${fields.host}:${fields.port}/${auth}`;
    }
    default: {
      const user = fields.user ? `${encodeURIComponent(fields.user)}@` : "";
      const database = fields.database ? `/${encodeURIComponent(fields.database)}` : "";
      return `mysql://${user}${fields.host}:${fields.port}${database}`;
    }
  }
}

const URL_SHAPE = /^([a-z][a-z0-9+.-]*):\/\/(?:([^:@/]*)(?::([^@/]*))?@)?([^/?#]+)(\/[^?#]*)?(\?[^#]*)?$/i;

function parseMysql(value: string, driver: DriverId): ParsedConnection {
  const match = URL_SHAPE.exec(value.replace(/^jdbc:/i, ""));
  if (!match || !/^(mysql|mariadb)$/i.test(match[1])) throw new Error("Expected mysql://user:password@host:port/database");
  const [, , user = "", password, hostPart, path = "", query = ""] = match;
  const [host, port] = splitHostPort(hostPart.split(",")[0], DEFAULT_PORTS[driver]);
  const params = new URLSearchParams(query);
  const database = decode(path.replace(/^\//, "")) || params.get("database") || null;
  return { host, port, user: decode(user), password: password === undefined ? null : decode(password), database, options: {} };
}

function parseMongo(value: string): ParsedConnection {
  const match = URL_SHAPE.exec(value);
  if (!match || !/^mongodb(\+srv)?$/i.test(match[1])) throw new Error("Expected mongodb://… or mongodb+srv://…");
  const [, scheme, user = "", password, hosts, path = "", query = ""] = match;
  const srv = scheme.toLowerCase() === "mongodb+srv";
  const [host, port] = srv ? [hosts, DEFAULT_PORTS.mongodb] : splitHostPort(hosts.split(",")[0], DEFAULT_PORTS.mongodb);
  const params = new URLSearchParams(query);
  const database = params.get("authSource") || decode(path.replace(/^\//, "")) || null;
  // Keep everything but the password: hosts, replica set, TLS and SRV options all live in the URI.
  const uri = `${scheme}://${user ? `${user}@` : ""}${hosts}${path}${query}`;
  return { host, port, user: decode(user), password: password === undefined ? null : decode(password), database, options: { uri } };
}

const SQLSERVER_KEYS: Record<string, string> = {
  server: "server",
  "data source": "server",
  address: "server",
  addr: "server",
  "network address": "server",
  database: "database",
  "initial catalog": "database",
  databasename: "database",
  "user id": "user",
  uid: "user",
  user: "user",
  username: "user",
  password: "password",
  pwd: "password",
  encrypt: "encrypt",
  trustservercertificate: "trust",
  "trust server certificate": "trust",
  port: "port",
  portnumber: "port",
};

function parseSqlServer(value: string): ParsedConnection {
  let text = value.replace(/^jdbc:/i, "");
  const fields: Record<string, string> = {};
  const url = /^sqlserver:\/\/([^;]*)/i.exec(text);
  if (url) {
    fields.server = url[1];
    text = text.slice(url[0].length);
  }
  for (const [name, raw] of keyValuePairs(text)) {
    const key = SQLSERVER_KEYS[name.toLowerCase()];
    if (key) fields[key] = raw;
  }
  if (!fields.server) throw new Error("Expected Server=host,port;Database=…;User Id=…;Password=…");
  // "tcp:host,1433", "host\\INSTANCE" (named instances need SQL Browser; the host is used as-is).
  const server = fields.server.replace(/^tcp:/i, "").split("\\")[0];
  const [host, parsedPort] = server.includes(",") ? splitHostPort(server.replace(",", ":"), DEFAULT_PORTS.mssql) : splitHostPort(server, DEFAULT_PORTS.mssql);
  const port = fields.port ? Number(fields.port) || parsedPort : parsedPort;
  const options: Record<string, string> = {
    encrypt: String(!/^(false|no|optional|0)$/i.test(fields.encrypt ?? "true")),
    trustServerCertificate: String(/^(true|yes|1)$/i.test(fields.trust ?? "false")),
  };
  return { host, port, user: fields.user ?? "", password: fields.password ?? null, database: fields.database || null, options };
}

/** `key=value;` pairs where a value may be wrapped in {braces} (with `}}` as an escaped brace) to hold `;`. */
function keyValuePairs(text: string): [string, string][] {
  const pairs: [string, string][] = [];
  let index = 0;
  while (index < text.length) {
    const equals = text.indexOf("=", index);
    if (equals < 0) break;
    const name = text
      .slice(index, equals)
      .replace(/^[\s;]+/, "")
      .trim();
    let cursor = equals + 1;
    while (text[cursor] === " ") cursor += 1;
    let value = "";
    if (text[cursor] === "{") {
      cursor += 1;
      while (cursor < text.length) {
        if (text[cursor] === "}" && text[cursor + 1] === "}") {
          value += "}";
          cursor += 2;
        } else if (text[cursor] === "}") {
          cursor += 1;
          break;
        } else {
          value += text[cursor];
          cursor += 1;
        }
      }
      const next = text.indexOf(";", cursor);
      index = next < 0 ? text.length : next + 1;
    } else {
      const next = text.indexOf(";", cursor);
      value = text.slice(cursor, next < 0 ? text.length : next).trim();
      index = next < 0 ? text.length : next + 1;
    }
    if (name) pairs.push([name, value]);
  }
  return pairs;
}

function splitHostPort(value: string, fallback: number): [string, number] {
  const trimmed = value.trim();
  const ipv6 = /^\[([^\]]+)\](?::(\d+))?$/.exec(trimmed);
  if (ipv6) return [ipv6[1], Number(ipv6[2]) || fallback];
  const index = trimmed.lastIndexOf(":");
  if (index > 0 && /^\d+$/.test(trimmed.slice(index + 1))) return [trimmed.slice(0, index), Number(trimmed.slice(index + 1))];
  return [trimmed, fallback];
}

function decode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}
