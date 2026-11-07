import { capabilities } from "../../core/capabilities";
import { api } from "../../core/api";
import { field, openModal, textInput, toast } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { AppConfig, EmbeddingAuth, EmbeddingSettings } from "../../core/types";

const AUTH_OPTIONS: { value: EmbeddingAuth; label: string }[] = [
  { value: "bearer", label: "Authorization: Bearer <key> (OpenAI, vLLM, most gateways)" },
  { value: "api-key", label: "api-key: <key> (Azure OpenAI)" },
  { value: "none", label: "No key (local Ollama, LM Studio)" },
];

export function embeddingDialog(existing: EmbeddingSettings | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const url = textInput(existing?.url ?? "", { class: "input mono", placeholder: "https://api.openai.com/v1/embeddings" });
    const model = textInput(existing?.model ?? "", { class: "input mono", placeholder: "text-embedding-3-large" });
    const auth = h(
      "select",
      { class: "input" },
      ...AUTH_OPTIONS.map((option) => h("option", { value: option.value, selected: option.value === (existing?.auth ?? "bearer") }, option.label)),
    );
    const apiKey = textInput("", { type: "password", placeholder: existing ? "Saved, leave empty to keep" : "" });
    const dimensions = textInput(existing?.dimensions ? String(existing.dimensions) : "", { type: "number", min: 1, placeholder: "optional, e.g. 1024" });

    const readSettings = (): EmbeddingSettings => {
      if (!url.value.trim() || !model.value.trim()) throw new Error("URL and model are required");
      return {
        url: url.value.trim(),
        model: model.value.trim(),
        auth: auth.value as EmbeddingAuth,
        dimensions: Number(dimensions.value) || null,
      };
    };
    const save = async () => {
      saved = await api.saveEmbeddingSettings(readSettings(), apiKey.value || null);
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h(
        "p",
        { class: "form-intro" },
        "Any OpenAI-compatible /embeddings endpoint. Use the same model that produced the stored vectors, otherwise distances are meaningless.",
      ),
      field("Endpoint URL", url),
      field("Model", model),
      field("Authentication", auth),
      field("API key", apiKey, `Stored in ${capabilities().secretStore}`),
      field("Dimensions", dimensions, "Sent as `dimensions` for models that support shortening (OpenAI v3)"),
    );
    openModal(
      "Embedding API",
      body,
      [
        { label: "Remove", alignLeft: true, danger: true, run: async () => void (saved = await api.saveEmbeddingSettings(null, null)) },
        { label: "Cancel", run: () => undefined },
        {
          label: "Save & test",
          run: async () => {
            await save();
            toast(`Embedding OK: ${await api.testEmbedding()} dimensions`, "success");
          },
        },
        { label: "Save", primary: true, run: save },
      ],
      { width: 540, onClose: () => resolve(saved) },
    );
  });
}
