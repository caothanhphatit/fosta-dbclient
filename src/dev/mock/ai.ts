/**
 * The assistant in the browser preview: there is no Rust backend, so keys stay in memory for this page load and requests
 * go straight from the browser to the provider. Schema lookups (tools) are desktop-only; the model gets the table list.
 */
import type { Channel } from "@tauri-apps/api/core";
import { errorText } from "../../core/errors";
import type { AiConnection, AiSettings, AppConfig, ChatEvent, ChatRequest, EmbeddingSettings, KeyTarget, SavedKeyRef } from "../../core/types";
import { baseUrlOf, connectionSettings, endpointProblem, providerPreset } from "../../features/assistant/aiProviders";
import { embeddingEndpoint, embeddingKeyKept, missingKeyProblem, sameApi } from "../../features/vector/embeddingKeys";
import { mockError } from "./errors";
import { state } from "./state";

// --- Assistant: real provider calls from the browser ------------------------
// The preview has no Rust backend, so the dialog's key is kept in memory for
// this page load and requests go straight to the provider from the browser.
// Schema lookups (tools) are desktop-only; here the model gets the table list.

/** Keys per LLM connection id, for this page load only. */
const browserKeys = new Map<string, string>();
export const chatRuns = new Map<string, AbortController>();
/** The embedding key, for this page load only. */
let embeddingKey: string | null = null;

/** A saved key the dialogs can pick by reference, like the backend's `commands::keys`. */
interface KeyOwner {
  id: string;
  label: string;
  endpoint: string;
  secret: string | null;
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function keyOwners(): KeyOwner[] {
  const owners: KeyOwner[] = [];
  const embedding = state.config.embedding;
  if (embedding && embedding.auth !== "none") {
    const endpoint = embeddingEndpoint(embedding.url);
    owners.push({ id: "app:embedding", label: `Embedding API (${hostOf(endpoint)})`, endpoint, secret: embeddingKey });
  }
  for (const connection of state.config.aiConnections) {
    const endpoint = baseUrlOf(connectionSettings(connection));
    if (endpoint)
      owners.push({ id: `ai:${connection.id}`, label: `${connection.name} (${hostOf(endpoint)})`, endpoint, secret: browserKeys.get(connection.id) ?? null });
  }
  return owners;
}

/** The saved keys that may be used for `target`: only those saved for the same API. */
export function savedApiKeys(target: KeyTarget): SavedKeyRef[] {
  const endpoint = target.kind === "embedding" ? embeddingEndpoint(target.url) : baseUrlOf({ provider: target.provider, baseUrl: target.baseUrl, model: "" });
  const own = target.kind === "ai" && target.connectionId ? `ai:${target.connectionId}` : null;
  return keyOwners()
    .filter((owner) => owner.id !== own && owner.secret && sameApi(owner.endpoint, endpoint))
    .map(({ id, label }) => ({ id, label }));
}

/** The secret behind `keyRef`, refused unless it was saved for the same API as `endpoint`. */
export function resolveKeyRef(keyRef: string, endpoint: string): string {
  const owner = keyOwners().find((item) => item.id === keyRef);
  if (!owner) throw mockError("ERROR", "The saved key you picked no longer exists; type the key");
  if (!sameApi(owner.endpoint, endpoint))
    throw mockError("ERROR", `The key saved for ${owner.label} belongs to another endpoint and is not sent to ${hostOf(endpoint)}; type the key`);
  if (!owner.secret) throw mockError("ERROR", `No key is saved for ${owner.label} any more; type the key`);
  return owner.secret;
}

/**
 * Like the backend: an empty key keeps the saved one unless the endpoint or the way it is sent changed, and saving
 * without any key is refused unless none is needed.
 */
export function saveEmbeddingSettings(settings: EmbeddingSettings | null, apiKey: string | null, keyRef: string | null): AppConfig {
  const saved = state.config.embedding;
  if (!settings) {
    embeddingKey = null;
  } else {
    const key = apiKey || (keyRef ? resolveKeyRef(keyRef, embeddingEndpoint(settings.url)) : null);
    const problem = missingKeyProblem(saved, settings, Boolean(key), embeddingKey !== null);
    if (problem) throw mockError("ERROR", problem);
    if (key) embeddingKey = key;
    else if (saved && !embeddingKeyKept(saved, settings)) embeddingKey = null;
  }
  state.config = { ...state.config, embedding: settings };
  return state.config;
}

/** Only for tests: the embedding key the mock holds. */
export function mockEmbeddingKey(): string | null {
  return embeddingKey;
}

/** Mirrors the backend: the active connection must exist; else the first one with its default model. */
function repairActive(next: AppConfig): AppConfig {
  const valid = next.aiConnections.some((item) => item.id === next.aiActive?.connectionId);
  if (valid) return next;
  const first = next.aiConnections[0];
  return { ...next, aiActive: first ? { connectionId: first.id, model: first.defaultModel } : null };
}

export function saveAiConnection(connection: AiConnection, apiKey: string | null, keyRef: string | null = null): AppConfig {
  const item = { ...connection, name: connection.name.trim(), defaultModel: connection.defaultModel.trim(), baseUrl: connection.baseUrl.trim() };
  if (!item.name) throw mockError("ERROR", "Name is required");
  if (!item.defaultModel) throw mockError("ERROR", "Default model is required");
  const problem = endpointProblem(item.baseUrl, item.provider);
  if (problem) throw mockError("ERROR", problem);
  const previous = state.config.aiConnections.find((existing) => existing.id === item.id);
  const url = (text: string) => text.trim().replace(/\/+$/, "");
  // Like the backend: a saved key is never kept for another provider or endpoint.
  if (!apiKey && keyRef) apiKey = resolveKeyRef(keyRef, baseUrlOf(connectionSettings(item)));
  if (apiKey) browserKeys.set(item.id, apiKey);
  else if (previous && (previous.provider !== item.provider || url(previous.baseUrl) !== url(item.baseUrl))) browserKeys.delete(item.id);
  const aiConnections = previous
    ? state.config.aiConnections.map((existing) => (existing.id === item.id ? item : existing))
    : [...state.config.aiConnections, item];
  let aiActive = state.config.aiActive;
  const moved = previous && (previous.provider !== item.provider || previous.baseUrl !== item.baseUrl);
  if (previous && aiActive?.connectionId === item.id && (moved || aiActive.model === previous.defaultModel))
    aiActive = { ...aiActive, model: item.defaultModel };
  state.config = repairActive({ ...state.config, aiConnections, aiActive });
  return state.config;
}

export function deleteAiConnection(id: string): AppConfig {
  browserKeys.delete(id);
  state.config = repairActive({ ...state.config, aiConnections: state.config.aiConnections.filter((item) => item.id !== id) });
  return state.config;
}

export function setAiActive(connectionId: string, model: string): AppConfig {
  if (!model.trim()) throw mockError("ERROR", "Model is required");
  if (!state.config.aiConnections.some((item) => item.id === connectionId)) throw mockError("ERROR", `LLM provider '${connectionId}' no longer exists`);
  state.config = { ...state.config, aiActive: { connectionId, model: model.trim() } };
  return state.config;
}

/** The typed key, else the picked one (`keyRef`), else the one kept for `connectionId`. */
export function draftKey(payload: Record<string, unknown>): string {
  const typed = typeof payload.apiKey === "string" ? payload.apiKey : "";
  if (!typed && typeof payload.keyRef === "string" && payload.keyRef) return resolveKeyRef(payload.keyRef, baseUrlOf(payload.settings as AiSettings));
  return typed || browserKeys.get(String(payload.connectionId ?? "")) || "";
}

/** Settings and key for a chat request, like the backend's `chat_settings`. */
function chatSettings(request: ChatRequest): { settings: AiSettings; apiKey: string } {
  const connectionId = request.connectionId ?? state.config.aiActive?.connectionId;
  if (!connectionId) throw mockError("ERROR", "Add an LLM provider first (Assistant → Set up…)");
  const connection = state.config.aiConnections.find((item) => item.id === connectionId);
  if (!connection) throw mockError("ERROR", `LLM provider '${connectionId}' no longer exists`);
  const model = request.model?.trim() || (state.config.aiActive?.connectionId === connection.id ? state.config.aiActive.model : connection.defaultModel);
  return { settings: connectionSettings(connection, model), apiKey: browserKeys.get(connection.id) ?? "" };
}

function providerHeaders(settings: AiSettings, apiKey: string): Record<string, string> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (providerPreset(settings.provider).wire === "anthropic") {
    headers["x-api-key"] = apiKey;
    headers["anthropic-version"] = "2026-10-21";
    headers["anthropic-dangerous-direct-browser-access"] = "true";
  } else if (apiKey) {
    headers.Authorization = `Bearer ${apiKey}`;
  }
  return headers;
}

async function failureText(response: Response): Promise<string> {
  const body = await response.text();
  try {
    const parsed = JSON.parse(body) as { error?: { message?: string } | string; message?: string };
    const message = typeof parsed.error === "string" ? parsed.error : (parsed.error?.message ?? parsed.message);
    if (message) return `${response.status}: ${message}`;
  } catch {
    // Not JSON; fall through to the raw preview.
  }
  return `${response.status}: ${body.slice(0, 300)}`;
}

/** Parses JSON, naming the URL when the server answered with something else (a login page, HTML). */
async function jsonBody(response: Response, url: string): Promise<unknown> {
  const text = await response.text();
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw mockError("ERROR", `${url} did not return JSON (got "${text.slice(0, 40).replace(/\s+/g, " ")}…"). Check the endpoint URL.`);
  }
}

/** fetch() from a page fails without detail when the provider blocks cross-origin calls; say so. */
async function providerFetch(url: string, init: RequestInit): Promise<Response> {
  try {
    return await fetch(url, init);
  } catch (error) {
    throw mockError(
      "ERROR",
      `The browser preview could not reach ${new URL(url).host} (${errorText(error)}). The provider probably blocks cross-origin requests; the desktop app calls it from Rust and is not affected.`,
    );
  }
}

export async function fetchModels(settings: AiSettings, key: string): Promise<string[]> {
  const problem = endpointProblem(settings.baseUrl, settings.provider);
  if (problem) throw mockError("ERROR", problem);
  const anthropic = providerPreset(settings.provider).wire === "anthropic";
  const url = `${baseUrlOf(settings)}${anthropic ? "/v1/models?limit=1000" : "/models"}`;
  const response = await providerFetch(url, { headers: providerHeaders(settings, key) });
  if (!response.ok) throw mockError("ERROR", `${providerPreset(settings.provider).label} API returned ${await failureText(response)}`);
  const payload = (await jsonBody(response, url)) as { data?: unknown; models?: unknown } | unknown[];
  // Same shapes the backend accepts: { data: [...] } (OpenAI, Anthropic), { models: [...] } (Ollama native), or a bare array.
  const items = Array.isArray(payload) ? payload : ((payload as { data?: unknown }).data ?? (payload as { models?: unknown }).models);
  if (!Array.isArray(items)) throw mockError("ERROR", `${url} returned no model list`);
  const ids = items
    .map((item) => (typeof item === "string" ? item : String((item as { id?: string; name?: string }).id ?? (item as { name?: string }).name ?? "")))
    .map((id) => id.replace(/^models\//, ""))
    .filter(Boolean);
  return [...new Set(ids)].sort();
}

function systemPrompt(context: ChatRequest["context"]): string {
  const lines = [
    "You are the assistant built into Fosta Studio, a desktop database client. You cannot run anything: put every runnable query in a fenced code block (```sql, or ```json for MongoDB commands) for the user to run from the editor. Answer in the user's language and keep prose short.",
    "Context:",
    context.connectionId
      ? `- Connection: ${context.connectionName ?? "connection"} (${context.driverLabel ?? "unknown"} ${context.serverVersion ?? ""})`
      : "- No connection is open; write generic SQL and say which engine you assumed.",
  ];
  if (context.database)
    lines.push(
      `- Database: ${context.database}`,
      "- Tables: products (~1240 rows), categories (12), orders (48210), order_items (153900), customers (9031), v_daily_sales (view)",
    );
  if (context.table) lines.push(`- Table the user is looking at: ${context.table}`);
  if (context.editorText) lines.push("- Current editor text:", "```", context.editorText, "```");
  if (context.lastError) lines.push(`- Last error from the server: ${context.lastError}`);
  if (context.extra) lines.push(`- ${context.extra}`);
  return lines.join("\n");
}

/** Streams the provider's reply through the channel the UI passed, like the backend does. */
export async function streamChat(request: ChatRequest, channel: Channel<ChatEvent>): Promise<string> {
  const { settings, apiKey } = chatSettings(request);
  const internals = (window as unknown as { __TAURI_INTERNALS__: { runCallback(id: number, data: unknown): void } }).__TAURI_INTERNALS__;
  let index = 0;
  const emit = (message: ChatEvent) => internals.runCallback(channel.id, { index: index++, message });
  const preset = providerPreset(settings.provider);
  const anthropic = preset.wire === "anthropic";
  const system = systemPrompt(request.context);
  const messages = request.messages.map((message) => ({ role: message.role, content: message.content }));
  const body = anthropic
    ? { model: settings.model, max_tokens: 8192, stream: true, system, messages }
    : { model: settings.model, stream: true, messages: [{ role: "system", content: system }, ...messages] };
  const controller = new AbortController();
  chatRuns.set(request.id, controller);
  let text = "";
  try {
    const response = await providerFetch(`${baseUrlOf(settings)}${anthropic ? "/v1/messages" : "/chat/completions"}`, {
      method: "POST",
      headers: providerHeaders(settings, apiKey),
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    if (!response.ok || !response.body) throw mockError("ERROR", `${preset.label} API returned ${await failureText(response)}`);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        const payload = line.replace(/\r$/, "").replace(/^data:\s*/, "");
        if (!line.startsWith("data:") || !payload || payload === "[DONE]") continue;
        const event = JSON.parse(payload) as {
          type?: string;
          delta?: { type?: string; text?: string };
          choices?: { delta?: { content?: string } }[];
          error?: { message?: string };
        };
        if (event.error?.message) throw mockError("ERROR", `${preset.label} API: ${event.error.message}`);
        const piece = anthropic
          ? event.type === "content_block_delta" && event.delta?.type === "text_delta"
            ? event.delta.text
            : undefined
          : event.choices?.[0]?.delta?.content;
        if (piece) {
          text += piece;
          emit({ type: "text", text: piece });
        }
      }
    }
    return text;
  } catch (error) {
    if (controller.signal.aborted) throw mockError("ERROR", "Stopped");
    throw error;
  } finally {
    chatRuns.delete(request.id);
    emit({ type: "done" });
  }
}

/** "Save & test": one short exchange without streaming. */
export async function probeChat(settings: AiSettings, apiKey: string): Promise<string> {
  const problem = endpointProblem(settings.baseUrl, settings.provider);
  if (problem) throw mockError("ERROR", problem);
  const preset = providerPreset(settings.provider);
  const anthropic = preset.wire === "anthropic";
  const prompt = { role: "user", content: "Reply with the single word OK." };
  const body = anthropic ? { model: settings.model, max_tokens: 64, messages: [prompt] } : { model: settings.model, messages: [prompt] };
  const response = await providerFetch(`${baseUrlOf(settings)}${anthropic ? "/v1/messages" : "/chat/completions"}`, {
    method: "POST",
    headers: providerHeaders(settings, apiKey),
    body: JSON.stringify(body),
  });
  if (!response.ok) throw mockError("ERROR", `${preset.label} API returned ${await failureText(response)}`);
  const payload = (await jsonBody(response, baseUrlOf(settings))) as { content?: { text?: string }[]; choices?: { message?: { content?: string } }[] };
  return (anthropic ? payload.content?.map((block) => block.text ?? "").join("") : payload.choices?.[0]?.message?.content)?.trim() || "(empty reply)";
}
