/**
 * Compatibility shim over `engines.ts` for the views not yet moved to engine descriptors (steps 12-15). Every field is
 * derived from the backend's `EngineDescriptor`; nothing here lists engines. Deleted in step 16.
 */
import type { EngineDescriptor } from "./contract";
import { iconOrDefault, type IconName } from "./dom";
import { defaultLanguage, describeServer as describeConnectedServer, engineInfo, visibleEngines, type EngineInfo } from "./engines";
import type { ConnectionProfile, DriverId } from "./types";

export { describeConnectedServer as describeServer };

/** Extra per-engine fields the connection dialog shows, stored in `profile.options`. */
export type DriverOption =
  { key: string; kind: "text"; label: string; placeholder?: string; hint?: string } | { key: string; kind: "checkbox"; label: string; default: boolean };

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

/**
 * Until the descriptor-driven dialog (step 15) has its TLS section, the old dialog keeps writing these v1 options; the
 * backend moves them into `tls` (`ConnectionProfile::normalize`). Keyed by the descriptor's SQL dialect, not an id list.
 */
const LEGACY_TLS_OPTIONS: Partial<Record<string, DriverOption[]>> = {
  msSql: [
    { key: "encrypt", kind: "checkbox", label: "Encrypt connection (TLS)", default: true },
    { key: "trustServerCertificate", kind: "checkbox", label: "Trust server certificate (self-signed)", default: false },
  ],
};

function optionsOf(descriptor: EngineDescriptor): DriverOption[] {
  const fromFields = descriptor.connection.fields.flatMap((field): DriverOption[] => {
    if (!field.key.startsWith("options.")) return [];
    const key = field.key.slice("options.".length);
    if (field.kind.type === "checkbox") return [{ key, kind: "checkbox", label: field.label, default: field.default === "true" }];
    if (field.kind.type === "text" || field.kind.type === "number" || field.kind.type === "textarea")
      return [{ key, kind: "text", label: field.label, placeholder: field.placeholder ?? undefined, hint: field.hint ?? undefined }];
    return [];
  });
  return [...(LEGACY_TLS_OPTIONS[descriptor.sqlDialect ?? ""] ?? []), ...fromFields];
}

function toDriverInfo(descriptor: EngineInfo): DriverInfo {
  return {
    id: descriptor.id as DriverId,
    label: descriptor.label,
    icon: iconOrDefault(descriptor.icon, "database"),
    defaultPort: descriptor.connection.defaultPort ?? 0,
    defaultUser: descriptor.connection.defaultUser,
    databaseLabel: descriptor.connection.databaseLabel,
    databaseHint: descriptor.connection.databaseHint,
    objectsLabel: descriptor.objectsLabel,
    available: descriptor.status !== "planned" && !descriptor.unknown,
    filterKind: descriptor.filter.raw === null || descriptor.filter.raw === "sql" ? "sql" : "json",
    filterPlaceholder: descriptor.filter.placeholder,
    queryHint: defaultLanguage(descriptor)?.hint ?? "Ctrl+Enter runs the query.",
    supportsCancel: descriptor.capabilities.includes("cancel"),
    options: optionsOf(descriptor),
  };
}

/** The engines a picker offers (planned ones only with the developer setting on). */
export function drivers(): DriverInfo[] {
  return visibleEngines().map(toDriverInfo);
}

export function driverInfo(id: string | undefined): DriverInfo {
  return toDriverInfo(engineInfo(id));
}

export function driverOf(profile: ConnectionProfile): DriverInfo {
  return driverInfo(profile.driver);
}
