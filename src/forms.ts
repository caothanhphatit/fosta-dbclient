import { api, errorText } from "./api";
import { checkbox, field, openModal, textInput, toast } from "./dialogs";
import { h, icon, iconButton } from "./dom";
import { AI_PROVIDERS, endpointProblem, providerPreset, type ProviderPreset } from "./aiProviders";
import { CONNECTION_STRING_EXAMPLES, formatConnectionString, MODE_OPTION, parseConnectionString, type ConnectionFields, type ParsedConnection } from "./connectionString";
import { describeServer, driverInfo, DRIVERS, type DriverInfo } from "./drivers";
import { formatArgs, parseArgs } from "./shell";
import type { AiConnection, AiSettings, AppConfig, ConnectionProfile, EmbeddingAuth, EmbeddingSettings, TaskDef, WorkspaceMeta } from "./types";
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
    let mode: "params" | "string" = existing?.options[MODE_OPTION] === "string" || existing?.options.uri ? "string" : "params";
    const connectionString = h("textarea", { class: "input mono connection-string", rows: "3", spellcheck: "false" });
    const stringHint = h("span", { class: "field-hint" });
    const stringField = h("label", { class: "field" }, h("span", { class: "field-label" }, "Connection string"), connectionString, stringHint);
    const paramsSection = h("div", { class: "form-grid" });
    const modeButtons = (["params", "string"] as const).map((value) => {
      const button = h("button", { class: "segment", type: "button", "data-mode": value }, value === "params" ? "Parameters" : "Connection string");
      button.addEventListener("click", () => switchMode(value));
      return button;
    });
    const currentFields = (): ConnectionFields => ({
      host: host.value.trim(),
      port: Number(port.value) || driver.defaultPort,
      user: user.value.trim(),
      database: database.value.trim() || null,
      options: { ...options, ...readOptions() },
    });
    /** Fills the parameter inputs from a parsed string, so either tab shows the same connection. */
    const applyParsed = (parsed: ParsedConnection) => {
      host.value = parsed.host;
      port.value = String(parsed.port);
      user.value = parsed.user;
      database.value = parsed.database ?? "";
      if (parsed.password !== null) password.value = parsed.password;
      Object.assign(options, parsed.options);
      if (driver.id !== "mongodb") delete options.uri;
      renderOptions();
    };
    const renderMode = () => {
      for (const button of modeButtons) button.classList.toggle("active", button.dataset.mode === mode);
      paramsSection.hidden = mode !== "params";
      stringField.hidden = mode !== "string";
      stringHint.textContent = `e.g. ${CONNECTION_STRING_EXAMPLES[driver.id]}. A password in the string goes to Credential Manager, not into the config file.`;
    };
    const switchMode = (next: "params" | "string") => {
      if (next === mode) return;
      if (next === "string") {
        if (driver.id === "mongodb") delete options.uri;
        connectionString.value = formatConnectionString(driver.id, currentFields());
      } else if (connectionString.value.trim()) {
        try {
          applyParsed(parseConnectionString(driver.id, connectionString.value));
        } catch (error) {
          toast(errorText(error), "error");
          return;
        }
      }
      if (next === "params") delete options.uri;
      mode = next;
      renderMode();
    };

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
      if (mode === "string") {
        delete options.uri;
        connectionString.value = formatConnectionString(next.id, currentFields());
      }
      renderMode();
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
    /** In string mode the string is the source of truth; its password, if any, is returned by readPassword. */
    let stringPassword: string | null = null;
    const readProfile = (): ConnectionProfile => {
      if (mode === "string") {
        const parsed = parseConnectionString(driver.id, connectionString.value);
        stringPassword = parsed.password;
        const label = parsed.options.uri ? parsed.host : `${parsed.host}:${parsed.port}`;
        return {
          id,
          name: name.value.trim() || label,
          driver: driver.id,
          host: parsed.host,
          port: parsed.port,
          user: parsed.user,
          database: parsed.database,
          savePassword: remember.input.checked,
          options: { ...parsed.options, [MODE_OPTION]: "string" },
        };
      }
      stringPassword = null;
      return {
        id,
        name: name.value.trim() || `${host.value.trim()}:${port.value}`,
        driver: driver.id,
        host: host.value.trim(),
        port: Number(port.value) || driver.defaultPort,
        user: user.value.trim(),
        database: database.value.trim() || null,
        savePassword: remember.input.checked,
        options: readOptions(),
      };
    };
    const readPassword = (): string | null => {
      if (mode === "string") return stringPassword ?? (existing?.savePassword ? null : "");
      return password.value === "" && existing?.savePassword ? null : password.value;
    };
    if (mode === "string") connectionString.value = existing?.options.uri ?? formatConnectionString(driver.id, currentFields());
    paramsSection.append(
      h("div", { class: "field-row" }, field("Host", host), h("div", { class: "narrow" }, field("Port", port))),
      h("div", { class: "field-row even" }, field("User", user), field("Password", password)),
      databaseField,
      optionsHost,
    );
    renderMode();

    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "Saved in workspace ", h("strong", {}, workspaceName), "."),
      field("Database type", h("div", { class: "driver-cards" }, ...cards)),
      availability,
      field("Connection name", name),
      h("div", { class: "segmented" }, ...modeButtons),
      paramsSection,
      stringField,
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

/** "New database…" on a connection. Resolves with the created name, or null when cancelled. */
export function databaseDialog(profile: ConnectionProfile): Promise<string | null> {
  return new Promise((resolve) => {
    let created: string | null = null;
    const info = driverInfo(profile.driver);
    const mongo = info.id === "mongodb";
    const name = textInput("", { class: "input mono", placeholder: mongo ? "e.g. analytics" : "e.g. shop_staging" });
    const collection = textInput("", { class: "input mono", placeholder: "e.g. events" });
    const note =
      info.id === "mssql"
        ? "Created with the server's default size and collation."
        : mongo
          ? "MongoDB only keeps a database once it holds a collection, so the first one is created with it."
          : "Created with utf8mb4 / utf8mb4_unicode_ci.";
    const body = h(
      "div",
      { class: "form-grid" },
      h("p", { class: "form-intro" }, "On ", h("strong", {}, profile.name), ` (${info.label}). ${note}`),
      field("Database name", name),
      mongo ? field("First collection", collection) : null,
    );
    openModal("New database", body, [
      { label: "Cancel", run: () => undefined },
      {
        label: "Create",
        primary: true,
        run: async () => {
          const value = name.value.trim();
          if (!value) throw new Error("Database name is required");
          await api.createDatabase(profile.id, value, mongo ? collection.value.trim() : null);
          created = value;
        },
      },
    ], { width: 440, onClose: () => resolve(created) });
  });
}
