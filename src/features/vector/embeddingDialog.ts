import { capabilities } from "../../core/capabilities";
import { api } from "../../core/api";
import { field, openModal, textInput, toast } from "../../core/dialogs";
import { h } from "../../core/dom";
import type { AppConfig, EmbeddingAuth, EmbeddingSettings, SavedKeyRef } from "../../core/types";
import { ENDPOINT_ACCEPTS, endpointHint, keyChoices, keyToSend, missingKeyProblem, OWN_EMBEDDING_KEY } from "./embeddingKeys";
import { applyPreset, CUSTOM_PRESET, EMBEDDING_PRESETS, presetOf, urlProblem } from "./embeddingPresets";

const AUTH_OPTIONS: { value: EmbeddingAuth; label: string }[] = [
  { value: "bearer", label: "Authorization: Bearer <key> (OpenAI, Gemini, vLLM, most gateways)" },
  { value: "api-key", label: "api-key: <key> (Azure OpenAI)" },
  { value: "none", label: "No key (local Ollama, LM Studio)" },
];

export function embeddingDialog(existing: EmbeddingSettings | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const preset = h(
      "select",
      { class: "input" },
      ...EMBEDDING_PRESETS.map((item) => h("option", { value: item.id }, item.label)),
      h("option", { value: CUSTOM_PRESET }, "Custom"),
    );
    preset.value = presetOf(existing?.url ?? "");
    const presetHint = h("span", { class: "field-hint" });
    const url = textInput(existing?.url ?? "", { class: "input mono", placeholder: "https://api.openai.com/v1" });
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
    /** Why the settings cannot be saved without a key, shown before anything is sent. */
    const keyProblem = h("p", { class: "test-status error" });
    const keyField = h(
      "div",
      { class: "field" },
      h("span", { class: "field-label" }, "API key"),
      keySource,
      apiKey,
      keyProblem,
      h("span", { class: "field-hint" }, `Stored in ${capabilities().secretStore}; a saved key is only used for the endpoint it was saved for`),
    );
    const dimensions = textInput(existing?.dimensions ? String(existing.dimensions) : "", { type: "number", min: 1, placeholder: "optional, e.g. 1024" });
    /** Whether the endpoint's own key is saved and offered for the URL as typed. */
    let ownKeySaved = false;

    const describePreset = () => {
      presetHint.textContent = EMBEDDING_PRESETS.find((item) => item.id === preset.value)?.hint ?? "Any OpenAI-compatible /embeddings endpoint";
    };
    const describeUrl = () => {
      urlHint.textContent = endpointHint(url.value);
      urlHint.hidden = !urlHint.textContent;
    };
    const showKey = () => {
      keyField.hidden = auth.value === "none";
      keySource.hidden = keySource.options.length < 2;
      apiKey.hidden = keySource.value !== "";
    };
    let ticket = 0;
    /** Offers the keys saved for the URL as typed; refreshed when the URL or the preset changes. */
    const loadKeys = async () => {
      const current = ++ticket;
      let refs: SavedKeyRef[] = [];
      try {
        refs = url.value.trim() ? await api.savedApiKeys({ kind: "embedding", url: url.value.trim() }) : [];
      } catch {
        refs = [];
      }
      if (current !== ticket) return;
      ownKeySaved = refs.some((ref) => ref.id === OWN_EMBEDDING_KEY);
      const { choices, selected } = keyChoices(refs);
      keySource.replaceChildren(...choices.map((choice) => h("option", { value: choice.value, selected: choice.value === selected }, choice.label)));
      showKey();
    };
    let keysLoaded = loadKeys();
    const reloadKeys = () => {
      keyProblem.textContent = "";
      keysLoaded = loadKeys();
    };
    preset.addEventListener("change", () => {
      describePreset();
      const picked = EMBEDDING_PRESETS.find((item) => item.id === preset.value);
      if (!picked) return;
      const next = applyPreset({ url: url.value, model: model.value, auth: auth.value as EmbeddingAuth }, picked);
      url.value = next.url;
      model.value = next.model;
      auth.value = next.auth;
      describeUrl();
      showKey();
      reloadKeys();
      // Azure's URL still needs the resource name: select it for typing.
      const placeholder = next.url.indexOf("<");
      if (placeholder >= 0) {
        url.focus();
        url.setSelectionRange(placeholder, next.url.indexOf(">", placeholder) + 1);
      }
    });
    keySource.addEventListener("change", () => {
      keyProblem.textContent = "";
      showKey();
      if (!apiKey.hidden) apiKey.focus();
    });
    apiKey.addEventListener("input", () => (keyProblem.textContent = ""));
    auth.addEventListener("change", () => {
      keyProblem.textContent = "";
      showKey();
    });
    url.addEventListener("input", () => {
      describeUrl();
      preset.value = presetOf(url.value);
      describePreset();
    });
    url.addEventListener("change", reloadKeys);
    describePreset();
    describeUrl();
    showKey();

    const readSettings = (): EmbeddingSettings => {
      if (!url.value.trim() || !model.value.trim()) throw new Error("URL and model are required");
      const problem = urlProblem(url.value);
      if (problem) throw new Error(problem);
      return {
        url: url.value.trim(),
        model: model.value.trim(),
        auth: auth.value as EmbeddingAuth,
        dimensions: Number(dimensions.value) || null,
      };
    };
    /** Saves the settings; `false` (the dialog stays open) when a key is needed and none was given. */
    const save = async (): Promise<boolean> => {
      const settings = readSettings();
      // The key picker follows the URL: wait for it, so a key offered for the previous URL is not sent.
      await keysLoaded;
      const { apiKey: typed, keyRef } = settings.auth === "none" ? { apiKey: null, keyRef: null } : keyToSend(keySource.value, apiKey.value);
      const problem = missingKeyProblem(existing, settings, Boolean(typed || keyRef), ownKeySaved);
      keyProblem.textContent = problem ?? "";
      if (problem) {
        if (!apiKey.hidden) apiKey.focus();
        return false;
      }
      saved = await api.saveEmbeddingSettings(settings, typed, keyRef);
      return true;
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "OpenAI-compatible /embeddings endpoint; use the model that produced the stored vectors."),
      h("label", { class: "field" }, h("span", { class: "field-label" }, "Provider"), preset, presetHint),
      h("label", { class: "field" }, h("span", { class: "field-label" }, "Endpoint URL"), url, h("span", { class: "field-hint" }, ENDPOINT_ACCEPTS), urlHint),
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
            if (!(await save())) return false;
            toast(`Embedding OK: ${await api.testEmbedding()} dimensions`, "success");
          },
        },
        { label: "Save", primary: true, run: save },
      ],
      { width: 540, onClose: () => resolve(saved) },
    );
  });
}
