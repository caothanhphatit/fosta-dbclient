import { api, errorText } from "../../core/api";
import { field, openModal, textInput } from "../../core/dialogs";
import { h, icon, iconButton } from "../../core/dom";
import type { AiConnection, AiSettings, AppConfig } from "../../core/types";
import { AI_PROVIDERS, endpointProblem, providerPreset, type ProviderPreset } from "./aiProviders";

const CUSTOM_MODEL = "__custom__";

/** Adds (`existing` null) or edits one LLM connection. */
export function aiDialog(existing: AiConnection | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    let preset = providerPreset(existing?.provider ?? "fosta");
    const name = textInput(existing?.name ?? preset.label, { placeholder: "e.g. Work OpenAI" });
    const baseUrl = textInput(existing?.provider === "custom" ? existing.baseUrl : "", { class: "input mono", placeholder: "http://localhost:11434/v1" });
    const apiKey = textInput("", { type: "password", placeholder: existing ? "Saved, leave empty to keep" : "" });
    /** The saved key only belongs to the provider it was saved for. */
    const savedKeyOwner = () => (existing !== null && existing.provider === preset.value ? existing.id : null);
    const hint = h("p", { class: "form-intro provider-hint" });
    /** Only Custom asks for a URL; the built-in providers have theirs fixed. */
    const endpointField = field("Endpoint", baseUrl, "The base URL that has /chat/completions under it");
    const describeProvider = (item: ProviderPreset) => {
      hint.replaceChildren(item.hint, item.url ? " Endpoint: " : "", item.url ? h("code", {}, item.url) : "");
      endpointField.hidden = item.value !== "custom";
    };
    const modelSelect = h("select", { class: "input mono" });
    const customModel = textInput("", { class: "input mono", placeholder: "model name as the provider expects it" });
    const modelStatus = h("span", { class: "field-hint" });
    const reloadButton = iconButton("refresh", "Reload the model list", () => void loadModels());
    let models: string[] = [];
    let loading = 0;

    /** Fills the dropdown: provider models (or the preset default) plus "Custom…". */
    const renderModels = (selected: string) => {
      const known = models.length ? models : preset.model ? [preset.model] : [];
      const custom = selected && !known.includes(selected);
      modelSelect.replaceChildren(
        ...known.map((name) => h("option", { value: name, selected: name === selected }, name)),
        h("option", { value: CUSTOM_MODEL, selected: custom }, "Custom…"),
      );
      if (custom) customModel.value = selected;
      customModel.hidden = !custom && known.length > 0;
    };
    const currentModel = () => (modelSelect.value === CUSTOM_MODEL ? customModel.value.trim() : modelSelect.value);
    modelSelect.addEventListener("change", () => {
      customModel.hidden = modelSelect.value !== CUSTOM_MODEL;
      if (!customModel.hidden) customModel.focus();
    });

    /** The list is only fetched once there is a key to send (typed, or saved earlier). Custom endpoints may need none. */
    const loadModels = async () => {
      const wanted = currentModel() || preset.model;
      const draft: AiSettings = { provider: preset.value, baseUrl: preset.value === "custom" ? baseUrl.value.trim() : "", model: wanted || "-" };
      const hasKey = Boolean(apiKey.value.trim()) || savedKeyOwner() !== null;
      const problem = endpointProblem(draft.baseUrl, preset.value);
      if (problem) {
        models = [];
        renderModels(wanted);
        modelStatus.textContent = problem;
        return;
      }
      if (!hasKey && preset.value !== "custom") {
        models = [];
        renderModels(wanted);
        modelStatus.textContent = `Enter your ${preset.label} key to load its models; ${wanted} is the default`;
        return;
      }
      const ticket = ++loading;
      modelStatus.textContent = "Loading the provider's models…";
      try {
        const loaded = await api.listAiModels(draft, apiKey.value || null, savedKeyOwner());
        if (ticket !== loading) return;
        models = loaded;
        renderModels(wanted);
        modelStatus.textContent = loaded.length ? `${loaded.length} models from ${preset.label}` : `${preset.label} returned no models; pick Custom… and type one`;
      } catch (error) {
        if (ticket !== loading) return;
        models = [];
        renderModels(wanted);
        modelStatus.textContent = `Could not list models (${errorText(error)}). Using the default; pick Custom… to type one.`;
      }
    };

    const cards = AI_PROVIDERS.map((item) => {
      const card = h("button", { class: "driver-card", type: "button", "data-provider": item.value, title: item.label }, icon(item.icon, "driver-icon provider-icon"), h("span", { class: "driver-label" }, item.label));
      card.addEventListener("click", () => select(item));
      return card;
    });
    const select = (next: ProviderPreset) => {
      if (!name.value.trim() || name.value.trim() === preset.label) name.value = next.label;
      preset = next;
      models = [];
      for (const card of cards) card.classList.toggle("active", card.dataset.provider === next.value);
      baseUrl.value = "";
      describeProvider(next);
      renderModels(next.model);
      void loadModels();
    };
    for (const card of cards) {
      if (card.dataset.provider === preset.value) card.classList.add("active");
    }
    describeProvider(preset);
    renderModels(existing?.defaultModel ?? preset.model);
    void loadModels();
    apiKey.addEventListener("change", () => void loadModels());
    baseUrl.addEventListener("change", () => void loadModels());

    const readSettings = (): AiSettings => {
      const model = currentModel();
      if (!model) throw new Error("Default model is required");
      const problem = endpointProblem(baseUrl.value, preset.value);
      if (problem) throw new Error(problem);
      return { provider: preset.value, baseUrl: preset.value === "custom" ? baseUrl.value.trim() : "", model };
    };
    const save = async () => {
      const settings = readSettings();
      const label = name.value.trim();
      if (!label) throw new Error("Name is required");
      const connection: AiConnection = { id, name: label, provider: settings.provider, baseUrl: settings.baseUrl, defaultModel: settings.model };
      saved = await api.saveAiConnection(connection, apiKey.value || null);
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "The assistant calls the provider directly from this computer with your key. It only sees table and column names, your editor text and the errors you ask about; it never runs queries."),
      field("Provider", h("div", { class: "driver-cards provider-cards" }, ...cards)),
      hint,
      field("Name", name, "Shown in the model switcher of the chat"),
      endpointField,
      field("API key", apiKey, "Stored in Windows Credential Manager"),
      h("div", { class: "field" }, h("span", { class: "field-label" }, "Default model"), h("div", { class: "model-row" }, modelSelect, reloadButton), customModel, modelStatus),
    );
    const testStatus = h("p", { class: "test-status" });
    body.append(testStatus);
    /** Tries the settings as typed; the dialog stays open either way. */
    const test = async (): Promise<false> => {
      testStatus.className = "test-status";
      testStatus.textContent = "Connecting…";
      try {
        const settings = readSettings();
        const started = performance.now();
        const reply = await api.testAi(settings, apiKey.value || null, savedKeyOwner());
        testStatus.classList.add("ok");
        testStatus.textContent = `${providerPreset(settings.provider).label} · ${settings.model} replied "${reply}" in ${Math.round(performance.now() - started)} ms`;
      } catch (error) {
        testStatus.classList.add("error");
        testStatus.textContent = errorText(error);
      }
      return false;
    };
    const remove = existing ? [{ label: "Remove", alignLeft: true, danger: true, run: async () => void (saved = await api.deleteAiConnection(existing.id)) }] : [];
    openModal(existing ? "Edit LLM provider" : "Add LLM provider", body, [
      ...remove,
      { label: "Test connection", alignLeft: true, run: test },
      { label: "Cancel", run: () => undefined },
      { label: "Save", primary: true, run: save },
    ], { width: 580, onClose: () => resolve(saved) });
  });
}
