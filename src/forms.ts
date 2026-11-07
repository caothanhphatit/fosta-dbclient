import { api, errorText } from "./api";
import { checkbox, field, openModal, textInput, toast } from "./dialogs";
import { h, icon, iconButton } from "./dom";
import { AI_PROVIDERS, providerPreset, type ProviderPreset } from "./aiProviders";
import { describeServer, driverInfo, DRIVERS, type DriverInfo } from "./drivers";
import { formatArgs, parseArgs } from "./shell";
import type { AiSettings, AppConfig, ConnectionProfile, EmbeddingAuth, EmbeddingSettings, TaskDef, WorkspaceMeta } from "./types";
import { WORKSPACE_COLORS, WORKSPACE_ICONS, workspaceColor, workspaceIcon } from "./workspaces";

const AUTH_OPTIONS: { value: EmbeddingAuth; label: string }[] = [
  { value: "bearer", label: "Authorization: Bearer <key> (OpenAI, vLLM, most gateways)" },
  { value: "api-key", label: "api-key: <key> (Azure OpenAI)" },
  { value: "none", label: "No key (local Ollama, LM Studio)" },
];

const PORT_FORWARD_TEMPLATE = {
  name: "MariaDB port-forward",
  program: "kubectl",
  args: "port-forward -n default svc/mariadb 13306:3306",
  readyPattern: "Forwarding from",
};

export function connectionDialog(existing: ConnectionProfile | null, workspaceName: string): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    let driver = driverInfo(existing?.driver);
    let previousDriver = driver;
    const name = textInput(existing?.name ?? "", { placeholder: "e.g. Staging replica" });
    const host = textInput(existing?.host ?? "127.0.0.1");
    const port = textInput(String(existing?.port ?? driver.defaultPort), { type: "number", min: 1, max: 65535 });
    const user = textInput(existing?.user ?? driver.defaultUser);
    const password = textInput("", {
      type: "password",
      placeholder: existing?.savePassword ? "Saved, leave empty to keep" : "",
    });
    const database = textInput(existing?.database ?? "", { placeholder: driver.databaseHint });
    const databaseField = field(driver.databaseLabel, database);
    const remember = checkbox("Save password in Windows Credential Manager", existing?.savePassword ?? true);
    const optionsHost = h("div", { class: "form-grid driver-options" });
    const availability = h("p", { class: "driver-note" });
    const options: Record<string, string> = { ...(existing?.options ?? {}) };
    const optionInputs = new Map<string, HTMLInputElement>();

    const renderOptions = () => {
      optionInputs.clear();
      const controls = driver.options.map((option) => {
        if (option.kind === "checkbox") {
          const stored = options[option.key];
          const box = checkbox(option.label, stored === undefined ? option.default : stored === "true");
          optionInputs.set(option.key, box.input);
          return box.element;
        }
        const input = textInput(options[option.key] ?? "", { class: "input mono", placeholder: option.placeholder ?? "" });
        optionInputs.set(option.key, input);
        return field(option.label, input, option.hint ?? "");
      });
      optionsHost.replaceChildren(...controls);
      optionsHost.hidden = controls.length === 0;
      availability.textContent = driver.available ? "" : `${driver.label} support is in progress: the profile can be saved now, connecting comes in a later build.`;
      availability.hidden = driver.available;
    };

    const selectDriver = (next: DriverInfo) => {
      for (const [key, input] of optionInputs) {
        options[key] = input.type === "checkbox" ? String(input.checked) : input.value;
      }
      driver = next;
      if (Number(port.value) === previousDriver.defaultPort) port.value = String(next.defaultPort);
      if (user.value.trim() === previousDriver.defaultUser) user.value = next.defaultUser;
      previousDriver = next;
      database.placeholder = next.databaseHint;
      databaseField.querySelector(".field-label")!.textContent = next.databaseLabel;
      for (const card of cards) card.classList.toggle("active", card.dataset.driver === next.id);
      renderOptions();
    };

    const cards = DRIVERS.map((info) => {
      const card = h(
        "button",
        { class: `driver-card${info.id === driver.id ? " active" : ""}`, type: "button", "data-driver": info.id, title: info.label },
        icon(info.icon, `driver-icon ${info.id}`),
        h("span", { class: "driver-label" }, info.label),
        info.available ? null : h("span", { class: "driver-soon" }, "soon"),
      );
      card.addEventListener("click", () => selectDriver(info));
      return card;
    });
    renderOptions();

    const readOptions = (): Record<string, string> => {
      const result: Record<string, string> = {};
      for (const [key, input] of optionInputs) {
        // Checkboxes are always stored so an unticked default-on option (TLS) stays off.
        if (input.type === "checkbox") result[key] = String(input.checked);
        else if (input.value.trim() !== "") result[key] = input.value.trim();
      }
      return result;
    };
    const readProfile = (): ConnectionProfile => ({
      id,
      name: name.value.trim() || `${host.value.trim()}:${port.value}`,
      driver: driver.id,
      host: host.value.trim(),
      port: Number(port.value) || driver.defaultPort,
      user: user.value.trim(),
      database: database.value.trim() || null,
      savePassword: remember.input.checked,
      options: readOptions(),
    });
    const readPassword = (): string | null => (password.value === "" && existing?.savePassword ? null : password.value);

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "Saved in workspace ", h("strong", {}, workspaceName), "."),
      field("Database type", h("div", { class: "driver-cards" }, ...cards)),
      availability,
      field("Connection name", name),
      h("div", { class: "field-row" }, field("Host", host), h("div", { class: "narrow" }, field("Port", port))),
      h("div", { class: "field-row even" }, field("User", user), field("Password", password)),
      databaseField,
      optionsHost,
      remember.element,
    );
    openModal(existing ? "Edit connection" : "New connection", body, [
      {
        label: "Test connection",
        alignLeft: true,
        run: async () => {
          const profile = readProfile();
          const version = await api.testConnection(profile, readPassword());
          toast(`Connection OK · ${describeServer(profile, version)}`, "success");
          return false;
        },
      },
      { label: "Cancel", run: () => undefined },
      { label: "Save", primary: true, run: async () => void (saved = await api.saveConnection(readProfile(), readPassword())) },
    ], { width: 520, onClose: () => resolve(saved) });
  });
}

export function workspaceDialog(existing: WorkspaceMeta | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    const name = textInput(existing?.name ?? "", { placeholder: "e.g. Production, Client X, Local" });
    let color = workspaceColor(existing?.color ?? WORKSPACE_COLORS[Math.floor(Math.random() * WORKSPACE_COLORS.length)]);
    let iconName = workspaceIcon(existing?.icon ?? "laptop");
    const preview = h("div", { class: `ws-preview color-${color}` }, icon(iconName));
    const refreshPreview = () => {
      preview.className = `ws-preview color-${color}`;
      preview.replaceChildren(icon(iconName));
    };
    const swatches = WORKSPACE_COLORS.map((option) => {
      const swatch = h("button", { class: `swatch color-${option}${option === color ? " active" : ""}`, type: "button", title: option, "aria-label": option });
      swatch.addEventListener("click", () => {
        color = option;
        for (const item of swatches) item.classList.toggle("active", item === swatch);
        refreshPreview();
      });
      return swatch;
    });
    const icons = WORKSPACE_ICONS.map((option) => {
      const button = h("button", { class: `icon-choice${option.name === iconName ? " active" : ""}`, type: "button", title: option.label, "aria-label": option.label }, icon(option.name), h("span", {}, option.label));
      button.addEventListener("click", () => {
        iconName = option.name;
        for (const item of icons) item.classList.toggle("active", item === button);
        refreshPreview();
      });
      return button;
    });

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "A workspace keeps its own connections and startup tasks. Switch between them from the rail on the left."),
      h("div", { class: "ws-preview-row" }, preview, field("Workspace name", name)),
      field("Kind", h("div", { class: "icon-choices" }, ...icons)),
      field("Color", h("div", { class: "swatches" }, ...swatches)),
    );
    openModal(existing ? "Edit workspace" : "New workspace", body, [
      { label: "Cancel", run: () => undefined },
      {
        label: existing ? "Save" : "Create",
        primary: true,
        run: async () => {
          const trimmed = name.value.trim();
          if (!trimmed) throw new Error("Workspace name is required");
          saved = await api.saveWorkspace({ id, name: trimmed, color, icon: iconName });
        },
      },
    ], { width: 480, onClose: () => resolve(saved) });
  });
}

export function embeddingDialog(existing: EmbeddingSettings | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const url = textInput(existing?.url ?? "", { class: "input mono", placeholder: "https://api.openai.com/v1/embeddings" });
    const model = textInput(existing?.model ?? "", { class: "input mono", placeholder: "text-embedding-3-large" });
    const auth = h("select", { class: "input" }, ...AUTH_OPTIONS.map((option) => h("option", { value: option.value, selected: option.value === (existing?.auth ?? "bearer") }, option.label)));
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
      h("p", { class: "form-intro" }, "Any OpenAI-compatible /embeddings endpoint. Use the same model that produced the stored vectors, otherwise distances are meaningless."),
      field("Endpoint URL", url),
      field("Model", model),
      field("Authentication", auth),
      field("API key", apiKey, "Stored in Windows Credential Manager"),
      field("Dimensions", dimensions, "Sent as `dimensions` for models that support shortening (OpenAI v3)"),
    );
    openModal("Embedding API", body, [
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
    ], { width: 540, onClose: () => resolve(saved) });
  });
}

export function taskDialog(existing: TaskDef | null, workspaceName: string): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    const id = existing?.id ?? crypto.randomUUID();
    const name = textInput(existing?.name ?? "");
    const program = textInput(existing?.program ?? "kubectl", { class: "input mono" });
    const args = textInput(formatArgs(existing?.args ?? []), {
      class: "input mono",
      placeholder: "port-forward -n <namespace> svc/<service> <local>:<remote>",
    });
    const readyPattern = textInput(existing?.readyPattern ?? "", { placeholder: "text in output that means ready" });
    const autoStart = checkbox("Start automatically when this workspace opens", existing?.autoStart ?? true);
    const autoRestart = checkbox("Restart automatically when the process exits", existing?.autoRestart ?? true);
    const useTemplate = h("button", { class: "link-button", type: "button" }, "Fill kubectl port-forward template");
    useTemplate.addEventListener("click", () => {
      name.value ||= PORT_FORWARD_TEMPLATE.name;
      program.value = PORT_FORWARD_TEMPLATE.program;
      args.value = PORT_FORWARD_TEMPLATE.args;
      readyPattern.value = PORT_FORWARD_TEMPLATE.readyPattern;
    });

    const readTask = (): TaskDef => {
      const command = program.value.trim();
      if (!command) throw new Error("Program is required");
      return {
        id,
        name: name.value.trim() || command,
        program: command,
        args: parseArgs(args.value),
        autoStart: autoStart.input.checked,
        autoRestart: autoRestart.input.checked,
        readyPattern: readyPattern.value.trim() || null,
      };
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "Runs in the background while workspace ", h("strong", {}, workspaceName), " is open. ", useTemplate),
      field("Task name", name),
      field("Program", program, "Must be on PATH, e.g. kubectl"),
      field("Arguments", args),
      field("Ready when output contains", readyPattern),
      autoStart.element,
      autoRestart.element,
    );
    openModal(existing ? "Edit startup task" : "New startup task", body, [
      { label: "Cancel", run: () => undefined },
      { label: "Save", primary: true, run: async () => void (saved = await api.saveTask(readTask())) },
    ], { width: 520, onClose: () => resolve(saved) });
  });
}

const CUSTOM_MODEL = "__custom__";

export function aiDialog(existing: AiSettings | null): Promise<AppConfig | null> {
  return new Promise((resolve) => {
    let saved: AppConfig | null = null;
    let preset = providerPreset(existing?.provider ?? "fosta");
    const baseUrl = textInput(existing?.baseUrl ?? "", { class: "input mono" });
    const apiKey = textInput("", { type: "password", placeholder: existing ? "Saved, leave empty to keep" : "" });
    const hint = h("p", { class: "form-intro provider-hint" });
    const endpointField = field("Endpoint", baseUrl, "Leave empty for the provider's public endpoint");
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
      customModel.hidden = !custom;
    };
    const currentModel = () => (modelSelect.value === CUSTOM_MODEL ? customModel.value.trim() : modelSelect.value);
    modelSelect.addEventListener("change", () => {
      customModel.hidden = modelSelect.value !== CUSTOM_MODEL;
      if (!customModel.hidden) customModel.focus();
    });

    /** The list is only fetched once there is a key to send (typed, or saved earlier). Custom endpoints may need none. */
    const loadModels = async () => {
      const wanted = currentModel() || preset.model;
      const draft: AiSettings = { provider: preset.value, baseUrl: baseUrl.value.trim(), model: wanted || "-" };
      const hasKey = Boolean(apiKey.value.trim()) || (existing !== null && existing.provider === preset.value);
      if (preset.value === "custom" && !draft.baseUrl) {
        models = [];
        renderModels(wanted);
        modelStatus.textContent = "Enter the endpoint to list its models";
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
        const loaded = await api.listAiModels(draft, apiKey.value || null);
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
      preset = next;
      models = [];
      for (const card of cards) card.classList.toggle("active", card.dataset.provider === next.value);
      baseUrl.value = "";
      baseUrl.placeholder = next.url || "http://localhost:11434/v1";
      hint.textContent = next.hint;
      endpointField.querySelector(".field-hint")!.textContent = next.value === "custom" ? "Required: the base URL that has /chat/completions under it" : "Leave empty for the provider's public endpoint";
      renderModels(next.model);
      void loadModels();
    };
    for (const card of cards) {
      if (card.dataset.provider === preset.value) card.classList.add("active");
    }
    baseUrl.placeholder = preset.url || "http://localhost:11434/v1";
    hint.textContent = preset.hint;
    renderModels(existing?.model ?? preset.model);
    void loadModels();
    apiKey.addEventListener("change", () => void loadModels());
    baseUrl.addEventListener("change", () => void loadModels());

    const readSettings = (): AiSettings => {
      const model = currentModel();
      if (!model) throw new Error("Model is required");
      if (preset.value === "custom" && !baseUrl.value.trim()) throw new Error("Endpoint is required for a custom provider");
      return { provider: preset.value, baseUrl: baseUrl.value.trim(), model };
    };
    const save = async () => {
      saved = await api.saveAiSettings(readSettings(), apiKey.value || null);
    };

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "The assistant calls the provider directly from this computer with your key. It only sees table and column names, your editor text and the errors you ask about; it never runs queries."),
      field("Provider", h("div", { class: "driver-cards provider-cards" }, ...cards)),
      hint,
      endpointField,
      field("API key", apiKey, "Stored in Windows Credential Manager"),
      h("div", { class: "field" }, h("span", { class: "field-label" }, "Model"), h("div", { class: "model-row" }, modelSelect, reloadButton), customModel, modelStatus),
    );
    openModal("Assistant", body, [
      { label: "Remove", alignLeft: true, danger: true, run: async () => void (saved = await api.saveAiSettings(null, null)) },
      { label: "Cancel", run: () => undefined },
      {
        label: "Save & test",
        run: async () => {
          await save();
          toast(`Model replied: ${await api.testAi()}`, "success");
        },
      },
      { label: "Save", primary: true, run: save },
    ], { width: 580, onClose: () => resolve(saved) });
  });
}
