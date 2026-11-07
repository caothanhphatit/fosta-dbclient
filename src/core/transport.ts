/**
 * How `api.ts` reaches the backend. The same bundle runs in two places:
 *
 * - **Tauri** (the desktop app, and `npm run dev` with the dev mock, which installs `__TAURI_INTERNALS__`):
 *   Tauri's `invoke`, `Channel` and `listen`.
 * - **Web** (the Docker image, `fosta-studio-server`): `POST /api/invoke/<command>` with the same JSON
 *   arguments and result, `ai_chat`'s events streamed as NDJSON in its response, and task events over
 *   Server-Sent Events from `/api/events`. The session cookie authenticates; the `X-Fosta-Studio` header
 *   marks the call as coming from this app (CSRF protection).
 *
 * The choice is made per call, so the dev mock (installed after this module loads) is still picked up.
 */
import { Channel, invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";

export type { UnlistenFn };

export function isTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

/** Thrown (as Tauri does) with the backend's error message. */
export async function invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri()) return tauriInvoke<T>(command, args);
  const response = await post(command, args);
  return (await response.json()) as T;
}

/** A command that streams events while it runs; `channel` names the argument Tauri expects the `Channel` in. */
export async function invokeStreaming<T, E>(command: string, args: Record<string, unknown>, channel: string, onEvent: (event: E) => void): Promise<T> {
  if (isTauri()) {
    const events = new Channel<E>();
    events.onmessage = onEvent;
    return tauriInvoke<T>(command, { ...args, [channel]: events });
  }
  const response = await post(command, args);
  if (!response.body) throw new Error("The server sent no reply");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    const { done, value } = await reader.read();
    buffer += decoder.decode(value, { stream: !done });
    let newline = buffer.indexOf("\n");
    while (newline >= 0) {
      const line = buffer.slice(0, newline).trim();
      buffer = buffer.slice(newline + 1);
      newline = buffer.indexOf("\n");
      if (!line) continue;
      const message = JSON.parse(line) as { event?: E; result?: T; error?: string };
      if (message.error !== undefined) throw message.error;
      if ("result" in message) return message.result as T;
      if (message.event !== undefined) onEvent(message.event);
    }
    if (done) throw new Error("The connection to the server closed before the reply finished");
  }
}

async function post(command: string, args: Record<string, unknown>): Promise<Response> {
  let response: Response;
  try {
    response = await fetch(`/api/invoke/${encodeURIComponent(command)}`, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", "X-Fosta-Studio": "1" },
      body: JSON.stringify(args),
    });
  } catch {
    throw "Cannot reach the Fosta Studio server. Check that it is still running.";
  }
  if (response.status === 401) {
    // The session expired or the token changed: the server shows its sign-in page.
    window.location.assign("/");
    throw "Signed out. Sign in again with the access token.";
  }
  if (!response.ok) {
    // A string, or a structured error object (`code`, `message`, …) exactly as Tauri rejects with.
    const body = (await response.json().catch(() => null)) as { error?: unknown } | null;
    throw body?.error ?? `The server answered ${response.status} ${response.statusText}`;
  }
  return response;
}

/** Web edition: ends every session on the server (one token per server), then shows the sign-in page. */
export async function signOut(): Promise<void> {
  let response: Response;
  try {
    response = await fetch("/api/logout", { method: "POST", credentials: "same-origin", headers: { "X-Fosta-Studio": "1" } });
  } catch {
    throw "Cannot reach the Fosta Studio server. Check that it is still running.";
  }
  if (!response.ok && response.status !== 401) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    throw body?.error ?? `The server answered ${response.status} ${response.statusText}`;
  }
  window.location.assign("/");
}

type Handler = (payload: unknown) => void;

const handlers = new Map<string, Set<Handler>>();
let source: EventSource | null = null;

/** Like Tauri's `listen`, with the payload only. In the browser, one EventSource serves every listener. */
export async function listen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  if (isTauri()) return tauriListen<T>(event, (message) => handler(message.payload));
  source ??= new EventSource("/api/events", { withCredentials: true });
  let set = handlers.get(event);
  if (!set) {
    const listeners = new Set<Handler>();
    handlers.set(event, listeners);
    source.addEventListener(event, (message) => {
      const payload = JSON.parse((message as MessageEvent<string>).data) as unknown;
      for (const listener of listeners) listener(payload);
    });
    set = listeners;
  }
  const entry = handler as Handler;
  set.add(entry);
  return () => set.delete(entry);
}

/** Hands a file to the browser's download manager; returns the file name. */
export function downloadInBrowser(name: string, text: string | null, base64: string | null): string {
  let blob: Blob;
  if (text !== null) blob = new Blob([text], { type: "application/octet-stream" });
  else if (base64 !== null) blob = new Blob([Uint8Array.from(atob(base64), (char) => char.charCodeAt(0))], { type: "application/octet-stream" });
  else throw "Nothing to save";
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.hidden = true;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
  return name;
}
