import { describe, expect, it } from "vitest";
import { embeddingEndpoint } from "./embeddingKeys";
import { applyPreset, AZURE_TEMPLATE, CUSTOM_PRESET, EMBEDDING_PRESETS, presetOf, urlProblem } from "./embeddingPresets";

const preset = (id: string) => {
  const found = EMBEDDING_PRESETS.find((item) => item.id === id);
  if (!found) throw new Error(id);
  return found;
};

describe("embedding presets", () => {
  it("fill the URL, the way the key is sent and a default model", () => {
    expect(applyPreset({ url: "", model: "", auth: "bearer" }, preset("ollama"))).toEqual({
      url: "http://localhost:11434/v1",
      model: "nomic-embed-text",
      auth: "none",
    });
    expect(applyPreset({ url: "", model: "", auth: "bearer" }, preset("azure"))).toMatchObject({ url: AZURE_TEMPLATE, auth: "api-key" });
    expect(applyPreset({ url: "", model: "", auth: "none" }, preset("gemini"))).toEqual({
      url: "https://generativelanguage.googleapis.com/v1beta/openai/",
      model: "gemini-embedding-001",
      auth: "bearer",
    });
  });

  it("replace another preset's default model but keep a typed one", () => {
    const fromOpenai = applyPreset({ url: "https://api.openai.com/v1", model: "text-embedding-3-small", auth: "bearer" }, preset("gemini"));
    expect(fromOpenai.model).toBe("gemini-embedding-001");
    const typed = applyPreset({ url: "https://api.openai.com/v1", model: "text-embedding-3-large", auth: "bearer" }, preset("openai"));
    expect(typed.model).toBe("text-embedding-3-large");
  });

  it("send requests to the provider's /embeddings route", () => {
    expect(embeddingEndpoint(preset("openai").url)).toBe("https://api.openai.com/v1/embeddings");
    expect(embeddingEndpoint(preset("gemini").url)).toBe("https://generativelanguage.googleapis.com/v1beta/openai/embeddings");
    expect(embeddingEndpoint(preset("ollama").url)).toBe("http://localhost:11434/v1/embeddings");
    expect(embeddingEndpoint(AZURE_TEMPLATE)).toBe(AZURE_TEMPLATE);
  });

  it("are recognised from saved settings", () => {
    expect(presetOf("https://api.openai.com/v1/embeddings")).toBe("openai");
    expect(presetOf("https://api.openai.com/v1/")).toBe("openai");
    expect(presetOf("https://generativelanguage.googleapis.com/v1beta/openai")).toBe("gemini");
    expect(presetOf("https://team.openai.azure.com/openai/deployments/emb/embeddings?api-version=2024-02-01")).toBe("azure");
    expect(presetOf(AZURE_TEMPLATE)).toBe("azure");
    expect(presetOf("http://localhost:11434/v1")).toBe("ollama");
    expect(presetOf("https://gateway.example.com/embed")).toBe(CUSTOM_PRESET);
    expect(presetOf("")).toBe(CUSTOM_PRESET);
  });

  it("refuse Azure's template until its placeholders are replaced", () => {
    expect(urlProblem(AZURE_TEMPLATE)).toMatch(/Replace the <…> parts/);
    expect(urlProblem("https://team.openai.azure.com/openai/deployments/emb/embeddings?api-version=2024-02-01")).toBeNull();
  });
});
