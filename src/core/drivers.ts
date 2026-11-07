import type { IconName } from "./dom";
import type { ConnectionProfile, DriverId } from "./types";

/** Extra per-driver fields the connection dialog shows, stored in `profile.options`. */
export type DriverOption =
  | { key: string; kind: "text"; label: string; placeholder?: string; hint?: string }
  | { key: string; kind: "checkbox"; label: string; default: boolean };

export interface DriverInfo {
  id: DriverId;
  label: string;
  icon: IconName;
  defaultPort: number;
  defaultUser: string;
  /** What the "database" field means for this engine. */
  databaseLabel: string;
  databaseHint: string;
  /** Name for the second level of the tree (tables, collections). */
  objectsLabel: string;
  /** Whether this build can actually open the connection. */
  available: boolean;
  /** How the table filter box and column filters are expressed. */
  filterKind: "sql" | "json";
  filterPlaceholder: string;
  /** What the query tab runs and how to introduce it. */
  queryHint: string;
  supportsCancel: boolean;
  options: DriverOption[];
}

const SQL_FILTER = "Filter: SQL WHERE, e.g. status = 'ACTIVE'";
const SQL_HINT = "Ctrl+Enter runs the query. Selected text runs on its own.";

export const DRIVERS: DriverInfo[] = [
  {
    id: "mariadb",
    label: "MariaDB",
    icon: "mariadb",
    defaultPort: 3306,
    defaultUser: "root",
    databaseLabel: "Default database",
    databaseHint: "optional",
    objectsLabel: "tables",
    available: true,
    filterKind: "sql",
    filterPlaceholder: SQL_FILTER,
    queryHint: SQL_HINT,
    supportsCancel: true,
    options: [],
  },
  {
    id: "mysql",
    label: "MySQL",
    icon: "mysql",
    defaultPort: 3306,
    defaultUser: "root",
    databaseLabel: "Default database",
    databaseHint: "optional",
    objectsLabel: "tables",
    available: true,
    filterKind: "sql",
    filterPlaceholder: SQL_FILTER,
    queryHint: SQL_HINT,
    supportsCancel: true,
    options: [],
  },
  {
    id: "mssql",
    label: "SQL Server",
    icon: "mssql",
    defaultPort: 1433,
    defaultUser: "sa",
    databaseLabel: "Database",
    databaseHint: "optional, defaults to the login's database",
    objectsLabel: "tables",
    available: true,
    filterKind: "sql",
    filterPlaceholder: "Filter: T-SQL WHERE, e.g. Status = 'Active'",
    queryHint: "Ctrl+Enter runs the batch. Tables are addressed as schema.table; the default schema is dbo.",
    supportsCancel: false,
    options: [
      { key: "encrypt", kind: "checkbox", label: "Encrypt connection (TLS)", default: true },
      { key: "trustServerCertificate", kind: "checkbox", label: "Trust server certificate (self-signed)", default: false },
    ],
  },
  {
    id: "mongodb",
    label: "MongoDB",
    icon: "mongodb",
    defaultPort: 27017,
    defaultUser: "",
    databaseLabel: "Auth database",
    databaseHint: "optional, usually admin",
    objectsLabel: "collections",
    available: true,
    filterKind: "json",
    filterPlaceholder: 'Filter: JSON query, e.g. { "status": "ACTIVE", "stock": { "$gt": 0 } }',
    queryHint: 'Write a database command as JSON and press Ctrl+Enter, e.g. { "find": "products", "filter": { "stock": 0 }, "limit": 50 }',
    supportsCancel: false,
    // Replica sets and mongodb+srv:// go through the "Connection string" tab (stored as options.uri).
    options: [],
  },
];

export function driverInfo(id: string | undefined): DriverInfo {
  return DRIVERS.find((driver) => driver.id === id) ?? DRIVERS[0];
}

export function driverOf(profile: ConnectionProfile): DriverInfo {
  return driverInfo(profile.driver);
}

/** "MariaDB 11.8.2" from the raw version string the server reported. */
export function describeServer(profile: ConnectionProfile, version: string): string {
  const label = driverOf(profile).label;
  const trimmed = version.replace(/-?mariadb/i, "").trim();
  return `${label} ${trimmed}`;
}
