/**
 * Which endpoint a saved API key belongs to; the same rules as the backend (`embedding::endpoint`, `commands::keys`).
 * The dialogs use them to word their hints and the dev mock to keep keys like the backend does.
 */
import type { EmbeddingSettings, SavedKeyRef } from "../../core/types";

/**
 * The URL embedding requests go to. A base URL (`https://api.openai.com/v1`, as the assistant's settings take it) gets
 * `/embeddings` added; posted as it was, the provider answered 404. Any other path is used unchanged.
 */
export function embeddingEndpoint(url: string): string {
  const trimmed = url.trim();
  const queryAt = trimmed.indexOf("?");
  const path = (queryAt < 0 ? trimmed : trimmed.slice(0, queryAt)).replace(/\/+$/, "");
  const query = queryAt < 0 ? "" : trimmed.slice(queryAt);
  const afterScheme = path.includes("://") ? path.slice(path.indexOf("://") + 3) : path;
  const route = afterScheme.includes("/") ? afterScheme.slice(afterScheme.indexOf("/") + 1) : "";
  const last = route.split("/").pop() ?? "";
  const root = /^v\d[a-z0-9]*$/i.test(last) || last.toLowerCase() === "openai";
  return `${root ? `${path}/embeddings` : path}${query}`;
}

/** Whether a key saved for endpoint `a` may be sent to `b`: same scheme, host and port, and one path inside the other. */
export function sameApi(a: string, b: string): boolean {
  let left: URL;
  let right: URL;
  try {
    left = new URL(a.trim());
    right = new URL(b.trim());
  } catch {
    return false;
  }
  const path = (url: URL) => url.pathname.replace(/\/+$/, "");
  const inside = (inner: string, outer: string) => inner === outer || inner.startsWith(`${outer}/`);
  return (
    left.protocol === right.protocol &&
    left.hostname !== "" &&
    left.hostname === right.hostname &&
    left.port === right.port &&
    (inside(path(left), path(right)) || inside(path(right), path(left)))
  );
}

/** Whether saving `next` without a key keeps the key saved for `saved`: same endpoint and the same way of sending it. */
export function embeddingKeyKept(saved: EmbeddingSettings, next: EmbeddingSettings): boolean {
  return embeddingEndpoint(saved.url) === embeddingEndpoint(next.url) && saved.auth === next.auth;
}

/** The embedding API's own saved key, as `saved_api_keys` lists it. */
export const OWN_EMBEDDING_KEY = "app:embedding";

export interface KeyChoice {
  /** A saved key's reference, or "" for "type a key". */
  value: string;
  label: string;
}

/**
 * The key picker of the embedding dialog: the endpoint's own saved key first ("keep"), then keys saved for the same API
 * elsewhere, then "type a new key". The own key is preselected, so changing only the model asks for nothing.
 */
export function keyChoices(refs: SavedKeyRef[]): { choices: KeyChoice[]; selected: string } {
  const own = refs.find((ref) => ref.id === OWN_EMBEDDING_KEY);
  const others = refs.filter((ref) => ref.id !== OWN_EMBEDDING_KEY);
  const choices: KeyChoice[] = [
    ...(own ? [{ value: own.id, label: `Keep the key saved for ${own.label}` }] : []),
    ...others.map((ref) => ({ value: ref.id, label: `Use the key saved for ${ref.label}` })),
    { value: "", label: refs.length ? "Type a new key" : "Type a key" },
  ];
  return { choices, selected: own ? own.id : "" };
}

/** What the dialog sends for the picked choice: a typed key, or a reference the backend resolves. */
export function keyToSend(choice: string, typed: string): { apiKey: string | null; keyRef: string | null } {
  return choice ? { apiKey: null, keyRef: choice } : { apiKey: typed || null, keyRef: null };
}
