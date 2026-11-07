import type { IconName } from "./dom";
import type { AiProvider, AiSettings } from "./types";

/** What the settings dialog knows about each provider; the backend holds the same default URLs. */
export interface ProviderPreset {
  value: AiProvider;
  label: string;
  icon: IconName;
  /** Public endpoint; empty means the user must enter one. */
  url: string;
  /** Model selected until the provider's list is loaded. */
  model: string;
  hint: string;
  /** Anthropic speaks its own Messages API; everyone else is OpenAI `chat/completions`. */
  wire: "anthropic" | "openai";
}

export const AI_PROVIDERS: ProviderPreset[] = [
  { value: "fosta", label: "Fosta", icon: "rocket", url: "https://admin.fosta.app/v1", model: "astra", hint: "Fosta gateway. Key from admin.fosta.app.", wire: "openai" },
  { value: "openai", label: "OpenAI", icon: "bolt", url: "https://api.openai.com/v1", model: "gpt-5", hint: "Key from platform.openai.com.", wire: "openai" },
  { value: "gemini", label: "Gemini", icon: "globe", url: "https://generativelanguage.googleapis.com/v1beta/openai", model: "gemini-2.5-flash", hint: "Key from aistudio.google.com (Google's OpenAI-compatible endpoint).", wire: "openai" },
  { value: "anthropic", label: "Claude", icon: "layers", url: "https://api.anthropic.com", model: "claude-opus-5-5", hint: "Key from console.anthropic.com.", wire: "anthropic" },
  { value: "custom", label: "Custom", icon: "terminal", url: "", model: "", hint: "Any OpenAI-compatible chat/completions endpoint: Ollama (http://localhost:11434/v1, no key), LM Studio, OpenRouter, DeepSeek…", wire: "openai" },
];

export function providerPreset(value: string | undefined): ProviderPreset {
  return AI_PROVIDERS.find((preset) => preset.value === value) ?? AI_PROVIDERS[0];
}

/** The endpoint requests go to: the user's, or the provider's public one. */
export function baseUrlOf(settings: AiSettings): string {
  return (settings.baseUrl.trim() || providerPreset(settings.provider).url).replace(/\/+$/, "");
}
