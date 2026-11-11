/** Provider presets of the embedding dialog: picking one fills the URL, the way the key is sent and a default model. */
import type { EmbeddingAuth } from "../../core/types";
import { embeddingEndpoint } from "./embeddingKeys";

export interface EmbeddingPreset {
  id: string;
  label: string;
  url: string;
  auth: EmbeddingAuth;
  model: string;
  /** Shown under the picker. */
  hint: string;
}

/** Azure's URL names the resource and the deployment; the `<…>` parts are left for the user. */
export const AZURE_TEMPLATE = "https://<resource>.openai.azure.com/openai/deployments/<deployment>/embeddings?api-version=2024-02-01";

export const EMBEDDING_PRESETS: EmbeddingPreset[] = [
  { id: "openai", label: "OpenAI", url: "https://api.openai.com/v1", auth: "bearer", model: "text-embedding-3-small", hint: "Key from platform.openai.com" },
  {
    id: "gemini",
    label: "Gemini (OpenAI-compatible)",
    url: "https://generativelanguage.googleapis.com/v1beta/openai/",
    auth: "bearer",
    model: "gemini-embedding-001",
    hint: "Key from Google AI Studio",
  },
  {
    id: "azure",
    label: "Azure OpenAI",
    url: AZURE_TEMPLATE,
    auth: "api-key",
    model: "text-embedding-3-small",
    hint: "Replace <resource> and <deployment> in the URL; the key is sent as the api-key header",
  },
  {
    id: "ollama",
    label: "Ollama (local)",
    url: "http://localhost:11434/v1",
    auth: "none",
    model: "nomic-embed-text",
    hint: "No key; run ollama pull <model> first",
  },
];

export const CUSTOM_PRESET = "custom";

/** Compared as requests are sent, so a base URL and its full /embeddings URL are the same preset. */
const normalized = (url: string) => embeddingEndpoint(url).toLowerCase();

/** The preset the settings match, or "custom". */
export function presetOf(url: string): string {
  if (!url.trim()) return CUSTOM_PRESET;
  const typed = normalized(url);
  try {
    if (new URL(typed).hostname.endsWith(".openai.azure.com")) return "azure";
  } catch {
    // A URL still being typed matches only a preset URL as written.
  }
  return EMBEDDING_PRESETS.find((preset) => normalized(preset.url) === typed)?.id ?? CUSTOM_PRESET;
}

export interface PresetFields {
  url: string;
  model: string;
  auth: EmbeddingAuth;
}

/**
 * The fields after picking `preset`. The model is replaced only when empty or still another preset's default, so a
 * model the user typed stays. Keys are not touched here: the key picker follows the new URL, and the backend keeps the
 * saved key only while the endpoint and the way it is sent stay the same.
 */
export function applyPreset(current: PresetFields, preset: EmbeddingPreset): PresetFields {
  const model = current.model.trim();
  const defaultModel = !model || EMBEDDING_PRESETS.some((item) => item.model === model);
  return { url: preset.url, auth: preset.auth, model: defaultModel ? preset.model : current.model };
}

/** A problem with the URL that saving would only turn into a failed request: Azure's placeholders still in it. */
export function urlProblem(url: string): string | null {
  return /<[^>]*>/.test(url) ? "Replace the <…> parts of the URL with your resource and deployment names" : null;
}
