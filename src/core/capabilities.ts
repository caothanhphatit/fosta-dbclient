/**
 * What the running edition supports (the `capabilities` command). The desktop app does everything; the
 * web edition (Docker) has no "Show in folder", downloads through the browser, keeps secrets in an encrypted
 * file on the server, and runs startup tasks only when the operator turned them on.
 *
 * Until `loadCapabilities()` answers, the defaults follow the transport, so text built early is right.
 */
import { api } from "./api";
import { isTauri } from "./transport";
import type { Capabilities } from "./types";

const DESKTOP: Capabilities = {
  edition: "desktop",
  tasks: true,
  revealFile: true,
  browserDownloads: false,
  secretStore: "Windows Credential Manager",
  inContainer: false,
};

const WEB: Capabilities = {
  edition: "web",
  tasks: false,
  revealFile: false,
  browserDownloads: true,
  secretStore: "the server's encrypted secret store",
  inContainer: false,
};

// The dev mock (`npm run dev`) stands in for the desktop backend.
let current: Capabilities = isTauri() || import.meta.env.DEV ? DESKTOP : WEB;

export function capabilities(): Capabilities {
  return current;
}

export async function loadCapabilities(): Promise<Capabilities> {
  try {
    current = await api.capabilities();
  } catch {
    // A backend without the command (or the dev mock): keep the defaults.
  }
  return current;
}

/** Hides elements marked `data-requires="<capability>"` whose capability is off. */
export function hideUnsupported(root: ParentNode): void {
  for (const element of root.querySelectorAll<HTMLElement>("[data-requires]")) {
    const key = element.dataset.requires as keyof Capabilities;
    if (current[key] === false) element.hidden = true;
  }
}

const LOCAL_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "[::1]", "0.0.0.0"]);

/** In a container, `localhost` is the container itself: a hint to use host.docker.internal, or "". */
export function localhostHint(host: string): string {
  if (!current.inContainer || !LOCAL_HOSTS.has(host.trim().toLowerCase())) return "";
  return "Fosta Studio runs in a container, where localhost is the container itself. For a database on the Docker host, use host.docker.internal.";
}
