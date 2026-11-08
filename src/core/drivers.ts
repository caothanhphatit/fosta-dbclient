/**
 * Compatibility shim over `engines.ts` for the views not yet moved to engine descriptors (the assistant panel). Every
 * field is derived from the backend's `EngineDescriptor`; nothing here lists engines. Deleted in step 16.
 */
import { iconOrDefault, type IconName } from "./dom";
import { defaultLanguage, describeServer as describeConnectedServer, engineInfo, visibleEngines, type EngineInfo } from "./engines";
import type { ConnectionProfile, DriverId } from "./types";

export { describeConnectedServer as describeServer };

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
}

function toDriverInfo(descriptor: EngineInfo): DriverInfo {
  return {
    id: descriptor.id,
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
