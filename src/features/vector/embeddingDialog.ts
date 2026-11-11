import { capabilities } from "../../core/capabilities";
import { api } from "../../core/api";
import { field, openModal, textInput, toast } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { AppConfig, EmbeddingAuth, EmbeddingSettings, SavedKeyRef } from "../../core/types";
import { embeddingEndpoint, keyChoices, keyToSend } from "./embeddingKeys";

const AUTH_OPTIONS: { value: EmbeddingAuth; label: string }[] = [
  { value: "bearer", label: "Authorization: Bearer <key> (OpenAI, vLLM, most gateways)" },
  { value: "api-key", label: "api-key: <key> (Azure OpenAI)" },
  { value: "none", label: "No key (local Ollama, LM Studio)" },
];

export function embeddingDialog(existing: EmbeddingSettings | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const url = textInput(existing?.url ?? "", { class: "input mono", placeholder: "https://api.openai.com/v1/embeddings" });
    const urlHint = h("span", { class: "field-hint" });
    const model = textInput(existing?.model ?? "", { class: "input mono", placeholder: "text-embedding-3-large" });
    const auth = h(
      "select",
      { class: "input" },
      ...AUTH_OPTIONS.map((option) => h("option", { value: option.value, selected: option.value === (existing?.auth ?? "bearer") }, option.label)),
    );
    /** Saved keys for this endpoint, by reference: the dialog never sees a saved secret. */
    const keySource = h("select", { class: "input" });
    const apiKey = textInput("", { type: "password", placeholder: "API key" });
    const keyField = h(
      "div",
      { class: "field" },
      h("span", { class: "field-label" }, "API key"),
      keySource,
      apiKey,
      h("span", { class: "field-hint" }, `Stored in ${capabilities().secretStore}; a saved key is only used for the endpoint it was saved for`),
    );
    const dimensions = textInput(existing?.dimensions ? String(existing.dimensions) : "", { type: "number", min: 1, placeholder: "optional, e.g. 1024" });

    const describeUrl = () => {
      const typed = url.value.trim();
      const sent = embeddingEndpoint(typed);
      urlHint.textContent = typed && sent !== typed.replace(/\/+$/, "") ? `Requests go to ${sent}` : "The full URL, or the API's base URL ending in /v1";
    };
    const showKey = () => {
      keyField.hidden = auth.value === "none";
      keySource.hidden = keySource.options.length < 2;
      apiKey.hidden = keySource.value !== "";
    };
    let ticket = 0;
    /** Offers the keys saved for the URL as typed; refreshed when the URL changes. */
    const loadKeys = async () => {
      const current = ++ticket;
      let refs: SavedKeyRef[] = [];
      try {
        refs = url.value.trim() ? await api.savedApiKeys({ kind: "embedding", url: url.value.trim() }) : [];
      } catch {
        refs = [];
      }
      if (current !== ticket) return;
      const { choices, selected } = keyChoices(refs);
      keySource.replaceChildren(...choices.map((choice) => h("option", { value: choice.value, selected: choice.value === selected }, choice.label)));
      showKey();
    };
    keySource.addEventListener("change", () => {
      showKey();
      if (!apiKey.hidden) apiKey.focus();
    });
    auth.addEventListener("change", showKey);
    url.addEventListener("input", describeUrl);
    url.addEventListener("change", () => void loadKeys());
    describeUrl();
    showKey();
    void loadKeys();

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
      const settings = readSettings();
      const { apiKey: typed, keyRef } = settings.auth === "none" ? { apiKey: null, keyRef: null } : keyToSend(keySource.value, apiKey.value);
      saved = await api.saveEmbeddingSettings(settings, typed, keyRef);
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "OpenAI-compatible /embeddings endpoint; use the model that produced the stored vectors."),
      h("label", { class: "field" }, h("span", { class: "field-label" }, "Endpoint URL"), url, urlHint),
      field("Model", model),
      field("Authentication", auth),
      keyField,
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
