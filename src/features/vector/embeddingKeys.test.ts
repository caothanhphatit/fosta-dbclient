import { beforeEach, describe, expect, it } from "vitest";
import { mockEmbeddingKey, resolveKeyRef, saveAiConnection, saveEmbeddingSettings, savedApiKeys } from "../../dev/mock/ai";
import { state } from "../../dev/mock/state";
import type { EmbeddingSettings } from "../../core/types";
import { embeddingEndpoint, embeddingKeyKept, keyChoices, keyToSend, OWN_EMBEDDING_KEY, sameApi } from "./embeddingKeys";

const OPENAI: EmbeddingSettings = { url: "https://api.openai.com/v1/embeddings", model: "text-embedding-3-large", auth: "bearer", dimensions: null };

describe("embeddingEndpoint", () => {
  it("adds the embeddings route to a base URL, which was posted as it was and answered 404", () => {
    expect(embeddingEndpoint("https://api.openai.com/v1")).toBe("https://api.openai.com/v1/embeddings");
    expect(embeddingEndpoint("https://admin.fosta.app/v1/")).toBe("https://admin.fosta.app/v1/embeddings");
    expect(embeddingEndpoint("https://generativelanguage.googleapis.com/v1beta/openai")).toBe(
      "https://generativelanguage.googleapis.com/v1beta/openai/embeddings",
    );
  });

  it("leaves a full URL alone: no doubled route, no lost query", () => {
    expect(embeddingEndpoint("https://api.openai.com/v1/embeddings")).toBe("https://api.openai.com/v1/embeddings");
    const azure = "https://x.openai.azure.com/openai/deployments/emb/embeddings?api-version=2024-02-01";
    expect(embeddingEndpoint(azure)).toBe(azure);
    expect(embeddingEndpoint("https://v1.example.com")).toBe("https://v1.example.com");
  });
});

describe("sameApi", () => {
  it("needs the same scheme, host and port, and one path inside the other", () => {
    expect(sameApi("https://api.openai.com/v1", "https://api.openai.com/v1/embeddings")).toBe(true);
    expect(sameApi("https://api.openai.com/v1", "https://api.openai.com.evil.com/v1")).toBe(false);
    expect(sameApi("https://api.openai.com/v1", "http://api.openai.com/v1")).toBe(false);
    expect(sameApi("https://api.openai.com/v1", "https://api.openai.com/v10")).toBe(false);
    expect(sameApi("https://api.openai.com/v1", "not a url")).toBe(false);
  });
});

describe("embeddingKeyKept", () => {
  it("keeps the key when only the model or dimensions change", () => {
    expect(embeddingKeyKept(OPENAI, { ...OPENAI, model: "text-embedding-3-small", dimensions: 512 })).toBe(true);
    expect(embeddingKeyKept(OPENAI, { ...OPENAI, url: "https://api.openai.com/v1/" })).toBe(true);
  });

  it("forgets it when the endpoint or the way it is sent changes", () => {
    expect(embeddingKeyKept(OPENAI, { ...OPENAI, url: "https://evil.example.com/v1/embeddings" })).toBe(false);
    expect(embeddingKeyKept(OPENAI, { ...OPENAI, auth: "api-key" })).toBe(false);
  });
});

describe("key picker", () => {
  it("preselects the endpoint's own saved key, so a model-only change asks for nothing", () => {
    const { choices, selected } = keyChoices([
      { id: OWN_EMBEDDING_KEY, label: "Embedding API (api.openai.com)" },
      { id: "ai:work", label: "Work OpenAI (api.openai.com)" },
    ]);
    expect(selected).toBe(OWN_EMBEDDING_KEY);
    expect(choices.map((choice) => choice.label)).toEqual([
      "Keep the key saved for Embedding API (api.openai.com)",
      "Use the key saved for Work OpenAI (api.openai.com)",
      "Type a new key",
    ]);
    expect(keyChoices([]).selected).toBe("");
  });

  it("sends a reference or the typed key, never both", () => {
    expect(keyToSend("ai:work", "ignored")).toEqual({ apiKey: null, keyRef: "ai:work" });
    expect(keyToSend("", "sk-typed")).toEqual({ apiKey: "sk-typed", keyRef: null });
    expect(keyToSend("", "")).toEqual({ apiKey: null, keyRef: null });
  });
});

describe("dev mock keys", () => {
  beforeEach(() => {
    state.config = { ...state.config, embedding: null, aiConnections: [], aiActive: null };
    saveEmbeddingSettings(null, null, null);
  });

  it("keeps the embedding key on a model-only change and forgets it on an endpoint change", () => {
    saveEmbeddingSettings(OPENAI, "sk-emb", null);
    saveEmbeddingSettings({ ...OPENAI, model: "text-embedding-3-small" }, null, null);
    expect(mockEmbeddingKey()).toBe("sk-emb");
    expect(state.config.embedding?.model).toBe("text-embedding-3-small");
    saveEmbeddingSettings({ ...OPENAI, url: "https://other.example.com/v1/embeddings" }, null, null);
    expect(mockEmbeddingKey()).toBeNull();
  });

  it("uses a key picked for the same API and refuses one for another host", () => {
    saveAiConnection({ id: "work", name: "Work OpenAI", provider: "openai", baseUrl: "", defaultModel: "gpt-5" }, "sk-work");
    expect(savedApiKeys({ kind: "embedding", url: OPENAI.url })).toEqual([{ id: "ai:work", label: "Work OpenAI (api.openai.com)" }]);
    expect(savedApiKeys({ kind: "embedding", url: "https://evil.example.com/v1/embeddings" })).toEqual([]);
    saveEmbeddingSettings(OPENAI, null, "ai:work");
    expect(mockEmbeddingKey()).toBe("sk-work");
    expect(() => resolveKeyRef("ai:work", "https://evil.example.com/v1/embeddings")).toThrow(/another endpoint/);
    expect(() => saveEmbeddingSettings({ ...OPENAI, url: "https://evil.example.com/v1/embeddings" }, null, "ai:work")).toThrow(/another endpoint/);
  });
});
