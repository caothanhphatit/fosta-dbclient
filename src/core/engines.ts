/**
 * What the UI knows about engines, all of it from the backend: the engine descriptors (`engine_descriptors`, loaded once
 * at startup by `loadEngines`) and, per open connection, the `ConnectInfo` the server reported on connect (capabilities,
 * languages, server version). Shared UI code asks these instead of comparing engine ids, so adding an engine needs no
 * edit here.
 */
import { api } from "./api";
import type { CapabilityName, ConnectInfo, EngineDescriptor, LanguageSpec } from "./contract";
import type { ConnectionProfile } from "./types";

/** A descriptor; `unknown` when the id is not one this build knows (a profile from a newer version, a typo). */
export type EngineInfo = EngineDescriptor & { unknown?: true };

/** Set to "1" in localStorage to list planned (not yet connectable) engines in pickers. */
export const SHOW_PLANNED_KEY = "fosta.dev.showPlannedEngines";

let descriptors: EngineDescriptor[] = [];
let byId = new Map<string, EngineDescriptor>();
let loading: Promise<EngineDescriptor[]> | null = null;
const connections = new Map<string, ConnectInfo>();

/** Fetches the descriptors once; later calls return the cached list. Call before the UI needs engine data. */
export function loadEngines(): Promise<EngineDescriptor[]> {
  loading ??= api.engineDescriptors().then(
    (list) => {
      setEngines(list);
      return list;
    },
    (error: unknown) => {
      loading = null;
      throw error;
    },
  );
  return loading;
}

/** Replaces the cached descriptors (tests, and `loadEngines`). */
export function setEngines(list: EngineDescriptor[]): void {
  descriptors = [...list];
  byId = new Map(list.map((descriptor) => [descriptor.id, descriptor]));
}

/** Every engine, planned ones included, in the backend's order. */
export function engines(): readonly EngineDescriptor[] {
  return descriptors;
}

/** Whether planned engines are shown in pickers (a developer setting). */
export function showPlannedEngines(): boolean {
  try {
    return localStorage.getItem(SHOW_PLANNED_KEY) === "1";
  } catch {
    return false;
  }
}

/** The engines a picker offers: planned ones only with the developer setting on. */
export function visibleEngines(): EngineDescriptor[] {
  const planned = showPlannedEngines();
  return descriptors.filter((descriptor) => planned || descriptor.status !== "planned");
}

/** The descriptor of `id`; an id this build does not know gets a clearly marked placeholder (never another engine's). */
export function engineInfo(id: string | null | undefined): EngineInfo {
  const known = id ? byId.get(id) : undefined;
  return known ?? unknownEngine(id ?? "");
}

export function engineOf(profile: Pick<ConnectionProfile, "driver">): EngineInfo {
  return engineInfo(profile.driver);
}

function unknownEngine(id: string): EngineInfo {
  return {
    id,
    label: id ? `Unknown engine "${id}"` : "Unknown engine",
    family: "sql",
    icon: "database",
    status: "planned",
    sqlDialect: null,
    capabilities: [],
    objectsLabel: "objects",
    connection: {
      usesHost: true,
      usesFile: false,
      defaultPort: null,
      defaultUser: "",
      databaseLabel: "Database",
      databaseHint: "optional",
      url: null,
      auth: ["password"],
      fields: [],
      ssh: false,
      tls: null,
    },
    languages: [],
    filter: { structured: false, raw: null, placeholder: "Filter" },
    unknown: true,
  };
}

/** The language a new query session uses (the engine's default, else its first). */
export function defaultLanguage(source: Pick<EngineDescriptor, "languages"> | Pick<ConnectInfo, "languages">): LanguageSpec | null {
  return source.languages.find((language) => language.default) ?? source.languages[0] ?? null;
}

export function hasCapability(source: Pick<EngineDescriptor, "capabilities"> | Pick<ConnectInfo, "capabilities">, capability: CapabilityName): boolean {
  return source.capabilities.includes(capability);
}

/** "MariaDB 11.8.2": the label the server reported, else the engine label and version. */
export function describeServer(info: ConnectInfo): string {
  if (info.server.label) return info.server.label;
  return `${engineInfo(info.engine).label} ${info.server.version}`.trim();
}

// --- Open connections ---------------------------------------------------------------------------------------------------

/** Remembers what the server reported on connect; `null` forgets it (disconnect). */
export function setConnectInfo(connectionId: string, info: ConnectInfo | null): void {
  if (info) connections.set(connectionId, info);
  else connections.delete(connectionId);
}

export function connectInfo(connectionId: string): ConnectInfo | null {
  return connections.get(connectionId) ?? null;
}

/**
 * Whether an open connection offers `capability` (from its `ConnectInfo`, which can be narrower than the descriptor:
 * an old server version, a read replica). False when the connection is not open.
 */
export function capabilityOf(connectionId: string, capability: CapabilityName): boolean {
  const info = connections.get(connectionId);
  return info ? info.capabilities.includes(capability) : false;
}
