/**
 * The view registry (ADR 0001): which view opens for an object kind and a role ("data", "design", "console", …).
 * Shared registrations come from `src/engines/shared.ts`; engine UI modules (`src/engines/<id>/`) add or override theirs.
 * Views are loaded lazily (`load`), so an engine's views are a chunk of their own.
 */
import type { ConnectionApi } from "./connectionApi";
import type { CapabilityName, ConnectInfo, EngineDescriptor, EngineFamily, ObjectNode, ObjectRef } from "./contract";
import { iconOrDefault, type IconName } from "./dom";
import type { Navigator } from "./navigator";
import type { TabContent } from "./tabs";
import type { ConnectionProfile } from "./types";

/**
 * `workspace`: an engine's dedicated tab for an object, with a strip of sections (`features/workspace`). Where one is
 * registered for a kind, a default open uses it instead of `data` (`defaultRole`); the grid stays one menu entry away.
 */
export type ViewRole = "data" | "design" | "overview" | "value" | "console" | "server" | "diagram" | "workspace" | (string & {});

export interface ViewContext {
  connection: ConnectionApi;
  profile: ConnectionProfile;
  engine: EngineDescriptor;
  info: ConnectInfo;
  object: ObjectRef | null;
  node: ObjectNode | null;
  navigator: Navigator;
  /** Role-specific extras (a database, an initial query text, a filter, the query tab's host). */
  params?: Record<string, unknown>;
}

/** What a view gives the tab manager: its element and `dispose()`, like every tab content. */
export type EngineView = TabContent;

export type ViewFactory = (ctx: ViewContext) => EngineView;

export interface ViewRegistration {
  /** An engine id, or "*" for every engine (narrowed by `family`). */
  engine: string | "*";
  family?: EngineFamily;
  /** Object kinds (`table`, `collection`, `redis.stream`) or "*" for any kind. */
  kinds: string[] | "*";
  role: ViewRole;
  /** The view is offered only when the engine (and, once connected, the connection) has this capability. */
  requires?: CapabilityName;
  title?(ctx: ViewContext): string;
  icon?: IconName;
  load: () => Promise<ViewFactory>;
}

const registrations: ViewRegistration[] = [];

/** Adds registrations; a later registration wins over an earlier one of the same precedence. */
export function registerViews(regs: ViewRegistration[]): void {
  registrations.push(...regs);
}

/** Removes every registration (tests). */
export function clearViews(): void {
  registrations.length = 0;
}

/** Ranks how specifically `reg` matches; 0 when it does not match. Higher wins. */
function precedence(reg: ViewRegistration, engine: EngineDescriptor, kind: string, role: ViewRole): number {
  if (reg.role !== role) return 0;
  if (reg.engine !== "*" && reg.engine !== engine.id) return 0;
  if (reg.family && reg.family !== engine.family) return 0;
  const kindMatch = reg.kinds === "*" ? "any" : reg.kinds.includes(kind) ? "exact" : null;
  if (!kindMatch) return 0;
  if (reg.requires && !engine.capabilities.includes(reg.requires)) return 0;
  const specificEngine = reg.engine !== "*";
  // engine + kind > family + kind > "*" + kind > engine + "*" > family + "*" > "*" + "*"
  if (kindMatch === "exact") return specificEngine ? 6 : reg.family ? 5 : 4;
  return specificEngine ? 3 : reg.family ? 2 : 1;
}

/** The best registration synchronously (the engine's UI module must already be loaded; see `ensureEngine`). */
export function findView(engine: EngineDescriptor, kind: string, role: ViewRole): ViewRegistration | null {
  let best: ViewRegistration | null = null;
  let bestRank = 0;
  for (const reg of registrations) {
    const rank = precedence(reg, engine, kind, role);
    if (rank > 0 && rank >= bestRank) {
      best = reg;
      bestRank = rank;
    }
  }
  return best;
}

/** The roles registered for an object kind, in registration order (for "Open as…" menus). */
export function rolesFor(engine: EngineDescriptor, kind: string): ViewRole[] {
  const roles = new Set<ViewRole>();
  for (const reg of registrations) if (precedence(reg, engine, kind, reg.role) > 0) roles.add(reg.role);
  return [...roles];
}

/**
 * The role a default open (double-click, Enter, the node menu's first entry) uses: `workspace` instead of `data` when
 * one is registered for the kind; any other role stays. Synchronous like `findView` (the engine's UI module is loaded).
 */
export function defaultRole(engine: EngineDescriptor, kind: string, role: ViewRole): ViewRole {
  return role === "data" && findView(engine, kind, "workspace") ? "workspace" : role;
}

let engineLoader: ((id: string) => Promise<void>) | null = null;

/** Installed by `src/engines/index.ts` so `resolveView` can load an engine's UI module first (core never imports engines). */
export function setEngineLoader(loader: (id: string) => Promise<void>): void {
  engineLoader = loader;
}

/** Loads the engine's UI module (its views and node icons) if it is not loaded yet. */
export async function loadEngineUi(engineId: string): Promise<void> {
  if (engineLoader) await engineLoader(engineId);
}

/** What an engine's UI module gets when connecting failed (to offer a fix, such as installing a client library). */
export interface ConnectErrorContext {
  profile: ConnectionProfile;
  /** Saves a changed profile (for a setting the fix needs); the saved password is kept. Absent for unsaved profiles. */
  saveProfile?: (profile: ConnectionProfile) => Promise<void>;
}

type ConnectErrorHandler = (engineId: string, error: unknown, context: ConnectErrorContext) => Promise<boolean>;

let connectErrorHandler: ConnectErrorHandler | null = null;

/** Installed by `src/engines/index.ts`: forwards a connect error to the engine's UI module. */
export function setConnectErrorHandler(handler: ConnectErrorHandler): void {
  connectErrorHandler = handler;
}

/** Lets the engine's UI module handle a failed connect; true when it fixed the cause and the connect should be retried. */
export async function recoverConnectError(engineId: string, error: unknown, context: ConnectErrorContext): Promise<boolean> {
  if (!connectErrorHandler) return false;
  try {
    return await connectErrorHandler(engineId, error, context);
  } catch {
    return false;
  }
}

let iconResolver: (icon: string) => string = (icon) => icon;

/** Installed by `src/engines/index.ts`: maps "engine:<id>/<name>" node icons through the engine UI module's `icons`. */
export function setIconResolver(resolver: (icon: string) => string): void {
  iconResolver = resolver;
}

/** The icon to draw for an `ObjectNode.icon` (an IconName or "engine:<id>/<name>"); `fallback` for anything unknown. */
export function nodeIcon(icon: string | null | undefined, fallback: IconName): IconName {
  return iconOrDefault(icon ? iconResolver(icon) : null, fallback);
}

/** Loads the engine's UI module if needed, then picks the registration: engine+kind > family+kind > "*"+kind > engine+"*". */
export async function resolveView(engine: EngineDescriptor, kind: string, role: ViewRole): Promise<ViewRegistration | null> {
  if (engineLoader) await engineLoader(engine.id);
  return findView(engine, kind, role);
}
